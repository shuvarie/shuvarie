use std::collections::{BTreeSet, HashMap, HashSet};

use shuvarie_db::{ReasoningSegment, StoredMessage, StoredScroll, StoredSession, TextSegment};
use shuvarie_llm::ChatMsg;
use shuvarie_llm::{Role, TokenUsage};

use crate::tool_record::ToolRecord;

/// A stored message's main-request usage: the persisted `request` payload when
/// the turn carried one, else the row's usage columns (older sessions and
/// redone turns persisted only the combined per-row totals). Rows without any
/// reported usage yield `None`.
fn request_usage_of(m: &StoredMessage) -> Option<TokenUsage> {
    if m.request.total_tokens > 0 {
        return Some(m.request);
    }
    let usage = TokenUsage {
        input_tokens: m.input_tokens,
        output_tokens: m.output_tokens,
        total_tokens: m.total_tokens,
        cached_input_tokens: m.cached_input_tokens,
        reasoning_tokens: m.reasoning_tokens,
        ..TokenUsage::default()
    };
    (usage.total_tokens > 0).then_some(usage)
}

/// One tool call attached to a tree node (popup display).
#[derive(Debug, Clone)]
pub struct TreeNodeTool {
    pub name: String,
    pub ok: bool,
    pub killed: bool,
    pub worker: Option<String>,
}

/// A node of the session tree: one stored message (a user prompt, an
/// assistant reply, or a compaction summary) with its parent link and tool
/// calls. The active path runs from the leaf up through `parent` links.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub id: u64,
    pub parent: Option<u64>,
    pub role: Role,
    pub seq: u64,
    pub content: String,
    pub summary: bool,
    pub interrupted: bool,
    pub tools: Vec<TreeNodeTool>,
    /// The node lies on the active path (the conversation currently shown).
    pub on_path: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: Option<uuid::Uuid>,
    pub title: Option<String>,
    /// The active path (root → tip) as chat messages. Forks and reloads
    /// rebuild it from the tree; live appends extend the tip.
    pub messages: Vec<ChatMsg>,
    pub reasoning: HashMap<u64, Vec<ReasoningSegment>>,
    /// The turn's text runs keyed by dense message index, with the tool-call
    /// positions they streamed at; drives the reload interleave.
    pub text_segments: HashMap<u64, Vec<TextSegment>>,
    pub interrupted: HashMap<u64, bool>,
    /// Tool calls belonging to the active path, keyed by dense message index.
    pub tool_records: Vec<ToolRecord>,
    pub tokens: u64,
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
    /// Dense indices (within `messages`) of the summary nodes on the active
    /// path. `history_for_send` drops everything before the newest one.
    pub summaries: BTreeSet<u64>,
    /// The full stored tree (all branches, not just the active path); drives
    /// the `/tree` popup. Only populated on load.
    pub nodes: Vec<TreeNode>,
    /// DB id of the active path's tip: the parent new appends hang from.
    pub leaf_id: Option<u64>,
    /// The session's active scene; `None` = the built-in default scene.
    pub scene: Option<String>,
    /// Usage of the most recent main-stream request, restored from storage so
    /// a loaded session can re-seed the sidebar's context anchor and read/
    /// cache-hit metrics. Not maintained by the live turn loop (which reports
    /// per-request usage through events).
    pub last_usage: Option<TokenUsage>,
    /// Chat pane scroll position persisted at the last leave of the session;
    /// the TUI seeds its scroll engine from it when the session loads.
    pub scroll: StoredScroll,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_stored(stored: StoredSession) -> Self {
        let mut nodes: Vec<TreeNode> = stored
            .messages
            .iter()
            .map(|m| TreeNode {
                id: m.id,
                parent: m.parent_id,
                role: m.role.into(),
                seq: m.seq,
                content: m.content.clone(),
                summary: m.summary,
                interrupted: m.interrupted,
                tools: Vec::new(),
                on_path: false,
            })
            .collect();
        let mut tools_by_msg: HashMap<u64, Vec<TreeNodeTool>> = HashMap::new();
        for tc in &stored.tool_calls {
            tools_by_msg
                .entry(tc.message_id)
                .or_default()
                .push(TreeNodeTool {
                    name: tc.name.clone(),
                    ok: tc.ok,
                    killed: tc.killed,
                    worker: tc.worker.clone(),
                });
        }
        for node in &mut nodes {
            if let Some(tools) = tools_by_msg.remove(&node.id) {
                node.tools = tools;
            }
        }
        let by_id: HashMap<u64, usize> = nodes.iter().enumerate().map(|(i, n)| (n.id, i)).collect();

        // The active tip: the stored leaf when it still exists, else the
        // newest message by seq.
        let leaf_pos = stored
            .leaf_id
            .and_then(|id| by_id.get(&id).copied())
            .or_else(|| {
                nodes
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, n)| n.seq)
                    .map(|(i, _)| i)
            });

        // Walk leaf → root, reversing into path order. The `seen` guard makes
        // a corrupt parent cycle terminate instead of looping.
        let mut path_ids: Vec<u64> = Vec::with_capacity(nodes.len());
        let mut seen: HashSet<u64> = HashSet::new();
        if let Some(pos) = leaf_pos {
            let mut cur = Some(nodes[pos].id);
            while let Some(id) = cur {
                if !seen.insert(id) {
                    break;
                }
                path_ids.push(id);
                cur = by_id.get(&id).copied().and_then(|p| nodes[p].parent);
            }
            path_ids.reverse();
            for id in &path_ids {
                if let Some(&pos) = by_id.get(id) {
                    nodes[pos].on_path = true;
                }
            }
        }
        let dense_of: HashMap<u64, usize> = path_ids
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, i))
            .collect();

        let mut s = Self {
            id: Some(stored.id),
            title: Some(stored.title),
            leaf_id: stored.leaf_id,
            scene: stored.scene,
            scroll: stored.scroll,
            ..Self::default()
        };
        for (idx, id) in path_ids.iter().enumerate() {
            let m = &stored.messages[by_id[id]];
            s.input_tokens = s.input_tokens.saturating_add(m.input_tokens);
            s.output_tokens = s.output_tokens.saturating_add(m.output_tokens);
            s.tokens = s.tokens.saturating_add(m.total_tokens);
            s.reasoning_tokens = s.reasoning_tokens.saturating_add(m.reasoning_tokens);
            s.cached_tokens = s.cached_tokens.saturating_add(m.cached_input_tokens);
            s.cost += m.cost;
            s.messages.push(ChatMsg {
                role: m.role.into(),
                content: m.content.clone(),
            });
            let idx = idx as u64;
            if !m.reasoning.is_empty() {
                s.reasoning.insert(idx, m.reasoning.clone());
            }
            if !m.text_segments.is_empty() {
                s.text_segments.insert(idx, m.text_segments.clone());
            }
            if m.interrupted {
                s.interrupted.insert(idx, true);
            }
            if m.summary {
                s.summaries.insert(idx);
            }
            if !m.summary
                && let Some(usage) = request_usage_of(m)
            {
                s.last_usage = Some(usage);
            }
        }
        s.tool_records = stored
            .tool_calls
            .iter()
            .filter_map(|tc| {
                let idx = dense_of.get(&tc.message_id).copied()?;
                let mut record = ToolRecord::from_stored(tc.clone());
                record.message_seq = idx as u64;
                Some(record)
            })
            .collect();
        s.nodes = nodes;
        s
    }

    pub fn clear(&mut self) {
        self.id = None;
        self.title = None;
        self.messages.clear();
        self.reasoning.clear();
        self.text_segments.clear();
        self.interrupted.clear();
        self.tool_records.clear();
        self.tokens = 0;
        self.cost = 0.0;
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.reasoning_tokens = 0;
        self.cached_tokens = 0;
        self.summaries.clear();
        self.nodes.clear();
        self.leaf_id = None;
        self.scene = None;
        self.last_usage = None;
        self.scroll = StoredScroll::default();
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::user(content));
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::assistant(content));
    }

    /// The active path's last user prompt node, if any.
    pub fn last_user_node(&self) -> Option<&TreeNode> {
        self.nodes
            .iter()
            .rev()
            .find(|n| n.on_path && n.role == Role::User)
    }

    pub fn last_assistant_interrupted(&self) -> bool {
        let Some(idx) = self
            .messages
            .iter()
            .rposition(|m| m.role == shuvarie_llm::Role::Assistant)
        else {
            return false;
        };
        self.interrupted
            .get(&(idx as u64))
            .copied()
            .unwrap_or(false)
    }

    pub fn add_usage(&mut self, usage: TokenUsage, cost: f64) {
        self.tokens = self.tokens.saturating_add(usage.total_tokens);
        self.cost += cost;
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(usage.cached_input_tokens);
    }

    /// The session's cumulative usage as a [`TokenUsage`], for usage snapshots.
    pub fn usage(&self) -> TokenUsage {
        TokenUsage {
            total_tokens: self.tokens,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cached_input_tokens: self.cached_tokens,
            reasoning_tokens: self.reasoning_tokens,
            ..TokenUsage::default()
        }
    }

    /// Build the chat history to send to the LLM for a new turn: everything
    /// before the pending user message (the active path's tip), truncated at
    /// the newest summary node — the summary replaces the history before it.
    pub fn history_for_send(&self) -> Vec<ChatMsg> {
        let total = self.messages.len();
        if total <= 1 {
            return Vec::new();
        }
        let end = total - 1;
        let begin = self
            .summaries
            .iter()
            .rev()
            .find(|&&s| (s as usize) < end)
            .map(|&s| s as usize)
            .unwrap_or(0);
        self.messages[begin..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_db::MsgRole;

    fn stored_message(seq: u64, role: MsgRole, content: &str) -> shuvarie_db::StoredMessage {
        shuvarie_db::StoredMessage {
            id: seq + 1,
            parent_id: None,
            role,
            content: content.to_string(),
            reasoning: Vec::new(),
            text_segments: Vec::new(),
            interrupted: false,
            seq,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: false,
            request: TokenUsage::default(),
        }
    }

    fn stored_session(messages: Vec<shuvarie_db::StoredMessage>) -> shuvarie_db::StoredSession {
        shuvarie_db::StoredSession {
            scene: None,
            id: uuid::Uuid::now_v7(),
            title: "t".into(),
            provider: None,
            model: None,
            leaf_id: None,
            messages,
            tool_calls: Vec::new(),
            scroll: shuvarie_db::StoredScroll::default(),
            created_at: jiff::Timestamp::now(),
            updated_at: jiff::Timestamp::now(),
        }
    }

    #[test]
    fn from_stored_seeds_last_usage_from_the_request_payload() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "hi"),
            stored_message(1, MsgRole::Assistant, "ok"),
        ]);
        stored.messages[1].input_tokens = 12_000;
        stored.messages[1].output_tokens = 200;
        stored.messages[1].total_tokens = 12_200;
        stored.messages[1].cached_input_tokens = 11_000;
        stored.messages[1].request = TokenUsage {
            input_tokens: 20_000,
            output_tokens: 200,
            total_tokens: 20_200,
            cached_input_tokens: 19_400,
            ..TokenUsage::default()
        };

        let session = Session::from_stored(stored);
        let last = session.last_usage.expect("request usage restored");
        assert_eq!(last.total_tokens, 20_200);
        assert_eq!(last.cached_input_tokens, 19_400);
    }

    #[test]
    fn from_stored_falls_back_to_row_usage_without_a_request_payload() {
        let mut stored = stored_session(vec![stored_message(0, MsgRole::Assistant, "ok")]);
        stored.messages[0].input_tokens = 500;
        stored.messages[0].output_tokens = 100;
        stored.messages[0].total_tokens = 600;

        let session = Session::from_stored(stored);
        let last = session.last_usage.expect("row usage restored");
        assert_eq!(last.total_tokens, 600);
    }

    #[test]
    fn from_stored_skips_zero_and_summary_rows_for_last_usage() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::Assistant, "ok"),
            stored_message(1, MsgRole::Assistant, "ok"),
            stored_message(2, MsgRole::User, "again"),
        ]);
        chain(&mut stored.messages);
        stored.leaf_id = Some(stored.messages[2].id);
        stored.messages[0].total_tokens = 30;
        stored.messages[0].request = TokenUsage {
            total_tokens: 30,
            ..TokenUsage::default()
        };
        stored.messages[1].summary = true;
        stored.messages[1].total_tokens = 9_999;
        stored.messages[1].request = TokenUsage {
            total_tokens: 9_999,
            ..TokenUsage::default()
        };

        let session = Session::from_stored(stored);
        let last = session.last_usage.expect("main request restored");
        assert_eq!(last.total_tokens, 30, "summary rows are not main requests");
    }

    #[test]
    fn from_stored_without_reported_usage_leaves_last_usage_none() {
        let stored = stored_session(vec![stored_message(0, MsgRole::User, "hi")]);
        let session = Session::from_stored(stored);
        assert!(session.last_usage.is_none());
    }

    #[test]
    fn from_stored_maps_text_segments_to_dense_indices() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "go"),
            stored_message(7, MsgRole::Assistant, "start\n\nresumed"),
        ]);
        chain(&mut stored.messages);
        stored.leaf_id = Some(stored.messages[1].id);
        stored.messages[1].text_segments = vec![
            shuvarie_db::TextSegment {
                after_tool: 0,
                text: "start".into(),
            },
            shuvarie_db::TextSegment {
                after_tool: 1,
                text: "\n\nresumed".into(),
            },
        ];

        let session = Session::from_stored(stored);
        let runs = session
            .text_segments
            .get(&1)
            .expect("segments keyed by dense index");
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[1].after_tool, 1);
    }

    /// Chain helper: link each message to the previous one via `parent_id`.
    fn chain(messages: &mut [shuvarie_db::StoredMessage]) {
        for i in 1..messages.len() {
            messages[i].parent_id = Some(messages[i - 1].id);
        }
    }

    #[test]
    fn from_stored_walks_the_active_path_from_the_leaf() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "one"),
            stored_message(1, MsgRole::Assistant, "r1"),
            stored_message(2, MsgRole::User, "two"),
            stored_message(3, MsgRole::Assistant, "r2"),
            stored_message(3, MsgRole::User, "fork prompt"),
            stored_message(4, MsgRole::Assistant, "fork reply"),
        ]);
        chain(&mut stored.messages);
        // Fork: the last two messages branch off the first assistant.
        stored.messages[4].parent_id = Some(stored.messages[1].id);
        stored.messages[5].parent_id = Some(stored.messages[4].id);
        stored.leaf_id = Some(stored.messages[5].id);

        let session = Session::from_stored(stored);
        let path: Vec<&str> = session
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(path, vec!["one", "r1", "fork prompt", "fork reply"]);
        assert_eq!(session.nodes.iter().filter(|n| n.on_path).count(), 4);
        let on_fork = session.nodes.iter().find(|n| n.content == "r2").unwrap();
        assert!(!on_fork.on_path);
    }

    #[test]
    fn from_stored_falls_back_to_newest_message_without_leaf() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "one"),
            stored_message(1, MsgRole::Assistant, "r1"),
        ]);
        chain(&mut stored.messages);

        let session = Session::from_stored(stored);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.messages[1].content, "r1");
    }

    #[test]
    fn from_stored_excludes_forked_away_usage_and_tools() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "one"),
            stored_message(1, MsgRole::Assistant, "r1"),
            stored_message(2, MsgRole::User, "fork"),
        ]);
        chain(&mut stored.messages);
        stored.messages[1].input_tokens = 100;
        stored.messages[1].total_tokens = 100;
        stored.messages[2].parent_id = Some(stored.messages[0].id);
        stored.leaf_id = Some(stored.messages[0].id);
        let mut forked_tool = tool_call_of(stored.messages[2].id);
        forked_tool.name = "read_file".into();
        stored.tool_calls = vec![forked_tool];

        let session = Session::from_stored(stored);
        assert_eq!(session.tokens, 0, "forked-away branch usage excluded");
        assert!(session.tool_records.is_empty());
    }

    fn tool_call_of(message_id: u64) -> shuvarie_db::StoredToolCall {
        shuvarie_db::StoredToolCall {
            id: 9,
            message_id,
            session_id: uuid::Uuid::nil(),
            seq: 0,
            name: "read_file".into(),
            args_json: "{}".into(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            killed: false,
            worker: None,
            file_change_json: String::new(),
            original_content: None,
            new_content: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn history_for_send_stops_at_the_newest_summary() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "u1"),
            stored_message(1, MsgRole::Assistant, "a1"),
            stored_message(2, MsgRole::User, "u2"),
            stored_message(3, MsgRole::Assistant, "summary"),
            stored_message(4, MsgRole::User, "u3"),
            stored_message(5, MsgRole::Assistant, "a3"),
        ]);
        chain(&mut stored.messages);
        stored.messages[3].summary = true;
        stored.leaf_id = Some(stored.messages[5].id);

        let session = Session::from_stored(stored);
        let history: Vec<String> = session
            .history_for_send()
            .iter()
            .map(|m| m.content.clone())
            .collect();
        assert_eq!(history, vec!["summary", "u3"], "summary + tail, no tip");
    }

    #[test]
    fn history_for_send_without_summary_is_full_but_pending() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "u1"),
            stored_message(1, MsgRole::Assistant, "a1"),
            stored_message(2, MsgRole::User, "pending"),
        ]);
        chain(&mut stored.messages);
        stored.leaf_id = Some(stored.messages[2].id);

        let session = Session::from_stored(stored);
        let history: Vec<String> = session
            .history_for_send()
            .iter()
            .map(|m| m.content.clone())
            .collect();
        assert_eq!(history, vec!["u1", "a1"]);
        assert!(session.messages[2].content == "pending");
    }

    #[test]
    fn last_user_node_finds_the_active_prompt() {
        let mut stored = stored_session(vec![
            stored_message(0, MsgRole::User, "u1"),
            stored_message(1, MsgRole::Assistant, "a1"),
            stored_message(2, MsgRole::User, "u2"),
        ]);
        chain(&mut stored.messages);
        stored.leaf_id = Some(stored.messages[2].id);

        let session = Session::from_stored(stored);
        let node = session.last_user_node().expect("user node on path");
        assert_eq!(node.content, "u2");
        assert_eq!(node.parent, Some(2), "the parent is the assistant row");
    }
}
