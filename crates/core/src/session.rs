use std::collections::HashMap;

use shuvarie_db::{ReasoningSegment, StoredMessage, StoredSession};
use shuvarie_llm::ChatMsg;
use shuvarie_llm::TokenUsage;

use crate::tool_record::ToolRecord;

pub const CONTINUE_PROMPT: &str =
    "Continue from where you left off; do not repeat what you already wrote.";

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

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: Option<uuid::Uuid>,
    pub title: Option<String>,
    pub messages: Vec<ChatMsg>,
    pub reasoning: HashMap<u64, Vec<ReasoningSegment>>,
    pub interrupted: HashMap<u64, bool>,
    pub tool_records: Vec<ToolRecord>,
    pub tokens: u64,
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
    /// The `seq` of the most recent compaction summary message, if any.
    /// Messages before this seq are replaced by the summary when building the
    /// history sent to the LLM.
    pub summary_seq: Option<u64>,
    /// Usage of the most recent main-stream request, restored from storage so
    /// a loaded session can re-seed the sidebar's context anchor and read/
    /// cache-hit metrics. Not maintained by the live turn loop (which reports
    /// per-request usage through events).
    pub last_usage: Option<TokenUsage>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_stored(stored: StoredSession) -> Self {
        let mut s = Self {
            id: Some(stored.id),
            title: Some(stored.title),
            ..Self::default()
        };
        for m in &stored.messages {
            s.input_tokens = s.input_tokens.saturating_add(m.input_tokens);
            s.output_tokens = s.output_tokens.saturating_add(m.output_tokens);
            s.tokens = s.tokens.saturating_add(m.total_tokens);
            s.reasoning_tokens = s.reasoning_tokens.saturating_add(m.reasoning_tokens);
            s.cached_tokens = s.cached_tokens.saturating_add(m.cached_input_tokens);
            s.cost += m.cost;
        }
        s.messages = stored
            .messages
            .iter()
            .map(|m| ChatMsg {
                role: m.role.into(),
                content: m.content.clone(),
            })
            .collect();
        // Map each stored message's DB `seq`/`id` to its dense position in
        // `messages`. The DB seq is not guaranteed to be contiguous (undo/redo
        // and resume delete rows and re-append with higher seqs), so anything
        // keyed by the raw seq must be translated to the dense index that the
        // rest of the pipeline (and the TUI) expects.
        let seq_to_index: HashMap<u64, usize> = stored
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| (m.seq, i))
            .collect();
        let id_to_index: HashMap<u64, usize> = stored
            .messages
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id, i))
            .collect();
        for m in &stored.messages {
            let idx = seq_to_index.get(&m.seq).copied().unwrap_or_default();
            if !m.reasoning.is_empty() {
                s.reasoning.insert(idx as u64, m.reasoning.clone());
            }
            if m.interrupted {
                s.interrupted.insert(idx as u64, true);
            }
            if m.summary {
                s.summary_seq = Some(idx as u64);
            }
            if !m.summary
                && let Some(usage) = request_usage_of(m)
            {
                s.last_usage = Some(usage);
            }
        }
        s.tool_records = stored
            .tool_calls
            .into_iter()
            .map(|tc| {
                let mut record = ToolRecord::from_stored(tc);
                if let Some(idx) = id_to_index.get(&record.message_id) {
                    record.message_seq = *idx as u64;
                }
                record
            })
            .collect();
        s
    }

    pub fn clear(&mut self) {
        self.id = None;
        self.title = None;
        self.messages.clear();
        self.reasoning.clear();
        self.interrupted.clear();
        self.tool_records.clear();
        self.tokens = 0;
        self.cost = 0.0;
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.reasoning_tokens = 0;
        self.cached_tokens = 0;
        self.last_usage = None;
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::user(content));
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::assistant(content));
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

    /// Whether `/continue` applies: the session has content and the last
    /// message is an incomplete (interrupted) assistant message.
    pub fn can_continue(&self) -> bool {
        let Some(last) = self.messages.last() else {
            return false;
        };
        let idx = self.messages.len() - 1;
        last.role == shuvarie_llm::Role::Assistant
            && !last.content.is_empty()
            && self
                .interrupted
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

    /// Build the chat history to send to the LLM for a new turn: all messages
    /// except the last (the pending user message), with compaction applied —
    /// everything before the most recent summary message is dropped (the
    /// summary replaces it).
    pub fn history_for_send(&self) -> Vec<ChatMsg> {
        let total = self.messages.len();
        let end = total.saturating_sub(1);
        if end == 0 {
            return Vec::new();
        }
        let begin = self.summary_seq.map(|s| (s as usize).min(end)).unwrap_or(0);
        self.messages[begin..end].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_db::MsgRole;

    fn stored_message(seq: u64, role: MsgRole, content: &str) -> shuvarie_db::StoredMessage {
        shuvarie_db::StoredMessage {
            id: seq,
            role,
            content: content.to_string(),
            reasoning: Vec::new(),
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
            id: uuid::Uuid::now_v7(),
            title: "t".into(),
            provider: None,
            model: None,
            messages,
            tool_calls: Vec::new(),
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
}
