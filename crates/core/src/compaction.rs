//! LLM-driven compaction: when the context budget is exceeded, summarize the
//! older portion of the conversation into a compact Markdown summary so the
//! session can continue without re-sending the full history on every turn.
//!
//! Modeled on OpenCode/Pi-style compaction: keep a token-budgeted verbatim
//! "tail" of recent messages (`keep_recent_tokens` from `[context]`, floored
//! at a minimum message count), summarize everything before it, and on
//! repeated compactions resume the summarized span from the previous summary
//! message — folding it in as prior context instead of summarizing a
//! summary-of-a-summary. The serialized transcript includes tool activity
//! (name, args, truncated output) and ends with ground-truth
//! `<read-files>`/`<modified-files>` lists derived from the tool records of
//! the span. The summary is persisted as a `summary`-flagged assistant
//! message; subsequent turns send `[summary, tail]` instead of the full
//! history.

use shuvarie_db::{MsgRole, StoredMessage};
use shuvarie_llm::FileChange;
use shuvarie_llm::ProviderClient;

use crate::tool_record::ToolRecord;

const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;
const TOOL_ARGS_MAX_CHARS: usize = 500;
const MIN_TAIL_MESSAGES: usize = 4;

const SUMMARY_PREAMBLE: &str = "\
You are a conversation summarizer for an agentic coding assistant. \
You will be given a transcript of an earlier conversation (user messages, \
assistant replies, and tool activity) that no longer fits in the model's \
context window. Produce a concise structured summary so the assistant can \
continue working without the full history. Use this format:\n\n\
## Objective\nWhat the user asked for and the current goal.\n\n\
## Important Details\nKey facts, decisions, constraints, and context discovered.\n\n\
## Work State\nWhat has been done so far, files changed, commands run, and their results.\n\n\
## Next Move\nWhat remains to be done and the immediate next step.\n\n\
## Relevant Files\nPaths of files that were read or modified, with a one-line note each.\n\n\
The transcript may include tool-activity lines ([tool (worker) status] followed \
by truncated output) and may end with <read-files> and <modified-files> \
sections listing the files actually read or modified in the span — ground \
the Relevant Files section in those lists.\n\n\
Keep the summary concise and information-dense. Do not include full file \
contents or tool outputs — reference them by path and line where relevant.";

/// A compaction plan: the message span `[start, cut)` to summarize, with the
/// verbatim tail `[cut, len)` kept for the next request.
pub struct CompactionPlan {
    /// Index where the summarized head begins: the previous summary message
    /// when one exists (folded in as prior context so its coverage carries
    /// into the new summary), else the session start.
    pub start: usize,
    /// Index where the verbatim tail begins.
    pub cut: usize,
}

/// Select the split point: keep the most recent messages verbatim within the
/// `keep_recent_tokens` tail budget and summarize everything before it. The
/// tail is floored at `MIN_TAIL_MESSAGES` messages so a small-history
/// overflow still has something to summarize, and the head resumes from the
/// previous summary message when one exists. `None` when there is no
/// summarizable span (too few messages, or nothing new since the previous
/// summary before the tail).
pub fn select_plan(messages: &[StoredMessage], keep_recent_tokens: u64) -> Option<CompactionPlan> {
    let len = messages.len();
    if len <= MIN_TAIL_MESSAGES {
        return None;
    }
    let start = messages.iter().rposition(|m| m.summary).unwrap_or(0);
    // Walk backwards from the newest message, accumulating token estimates
    // until the tail budget is reached. The `len` sentinel covers the case
    // where the whole span fits the budget — the message-count floor then
    // decides the cut so a small-history overflow still has a head to
    // summarize.
    let mut tail_tokens = 0u64;
    let mut cut = len;
    for i in (start..len).rev() {
        tail_tokens += shuvarie_llm::estimate_text_tokens(&messages[i].content);
        if tail_tokens >= keep_recent_tokens {
            cut = i;
            break;
        }
    }
    let cut = cut.min(len - MIN_TAIL_MESSAGES);
    (start < cut).then_some(CompactionPlan { start, cut })
}

/// Serialize the head of the conversation into a transcript for the
/// summarizer. Each message becomes a role section with its content
/// (truncated to `TOOL_OUTPUT_MAX_CHARS`); assistant sections are followed
/// by the tool activity that belongs to them (name, worker, truncated args
/// and output). The transcript ends with ground-truth file lists derived
/// from `tool_records`. `base_seq` is the dense message index of
/// `messages[0]` in the full session, used to match each record to its
/// message.
pub fn serialize_head(
    messages: &[StoredMessage],
    tool_records: &[ToolRecord],
    base_seq: usize,
) -> String {
    let mut out = String::new();
    for (idx, m) in messages.iter().enumerate() {
        let role = match m.role {
            MsgRole::User => "User",
            MsgRole::Assistant => "Assistant",
            MsgRole::System => "System",
        };
        if m.summary {
            out.push_str(&format!("### {role} (prior summary)\n{}\n\n", m.content));
            continue;
        }
        out.push_str(&format!("### {role}\n"));
        let content = truncate(&m.content, TOOL_OUTPUT_MAX_CHARS);
        out.push_str(content);
        if !content.ends_with('\n') {
            out.push('\n');
        }
        if m.role == MsgRole::Assistant {
            for record in tool_records.iter().filter(|r| {
                r.message_seq as usize == base_seq + idx || r.message_id == messages[idx].id
            }) {
                push_tool_activity(&mut out, record);
            }
        }
        out.push('\n');
    }
    push_file_sections(&mut out, tool_records);
    out
}

/// Append one tool call's activity to the transcript: a header line with the
/// tool name, worker tag, status, and truncated args, then the truncated
/// output when present.
fn push_tool_activity(out: &mut String, record: &ToolRecord) {
    let name = match &record.worker {
        Some(worker) => format!("{} ({worker})", record.name),
        None => record.name.clone(),
    };
    let status = if record.ok { "ok" } else { "failed" };
    let args = truncate(&record.args_json, TOOL_ARGS_MAX_CHARS);
    out.push_str(&format!("[{name} {status}] {args}\n"));
    let output = record.output.trim();
    if !output.is_empty() {
        out.push_str(truncate(output, TOOL_OUTPUT_MAX_CHARS));
        out.push('\n');
    }
}

/// Append ground-truth `<read-files>`/`<modified-files>` sections derived
/// from the span's tool records: path-bearing read tools contribute the read
/// list, and file changes from `write_file`/`edit_file` contribute the
/// modified list. Paths are deduplicated in first-seen order; sections are
/// omitted when empty.
fn push_file_sections(out: &mut String, records: &[ToolRecord]) {
    let mut read: Vec<String> = Vec::new();
    let mut modified: Vec<String> = Vec::new();
    for record in records {
        match record.name.as_str() {
            "read_file" | "grep" | "glob" | "list_dir" | "lsp" => {
                if let Some(path) = args_path(record) {
                    push_unique(&mut read, path);
                }
            }
            "write_file" | "edit_file" => {
                for path in changed_paths(&record.file_change) {
                    push_unique(&mut modified, path);
                }
            }
            _ => {}
        }
    }
    for (section, files) in [("read-files", &read), ("modified-files", &modified)] {
        if files.is_empty() {
            continue;
        }
        out.push_str(&format!("<{section}>\n"));
        for path in files {
            out.push_str(path);
            out.push('\n');
        }
        out.push_str(&format!("</{section}>\n\n"));
    }
}

/// The `path` argument of a tool call's JSON args, when present.
fn args_path(record: &ToolRecord) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&record.args_json)
        .ok()?
        .get("path")?
        .as_str()
        .map(str::to_string)
}

/// Paths touched by a file change.
fn changed_paths(change: &Option<FileChange>) -> Vec<String> {
    match change {
        Some(FileChange::Edit { path, .. }) | Some(FileChange::Write { path, .. }) => {
            vec![path.clone()]
        }
        Some(FileChange::Patch { files }) => files
            .iter()
            .flat_map(|f| {
                let mut paths = vec![f.path.clone()];
                if let Some(moved_to) = &f.moved_to {
                    paths.push(moved_to.clone());
                }
                paths
            })
            .collect(),
        None => Vec::new(),
    }
}

fn push_unique(list: &mut Vec<String>, path: String) {
    if !list.contains(&path) {
        list.push(path);
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let boundary = s
        .char_indices()
        .take(max)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(max);
    &s[..boundary]
}

/// Run the compaction summarizer via a single-shot worker call (no tools).
/// Returns the summary text, or an error string.
pub async fn summarize(
    client: &ProviderClient,
    model: &str,
    head_text: &str,
) -> Result<String, String> {
    let task = format!(
        "Summarize the following conversation transcript. Keep it concise and \
         information-dense, following the format in your instructions.\n\n---\n\
         {head_text}\n---"
    );
    let (activity_tx, mut activity_rx) = tokio::sync::mpsc::channel::<shuvarie_llm::StreamItem>(16);
    let usage = std::sync::Arc::new(std::sync::Mutex::new(shuvarie_llm::TokenUsage::default()));
    let req = shuvarie_llm::WorkerRequest {
        client: client.clone(),
        name: "compaction".to_string(),
        model: model.to_string(),
        preamble: SUMMARY_PREAMBLE.to_string(),
        task,
        tools: Vec::new(),
        activity_tx,
        usage,
        max_turns: 2,
        context_budget: None,
    };
    let result = client.run_worker(&req).await;
    // Drain activity to avoid backpressure.
    while activity_rx.recv().await.is_some() {}
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_llm::TokenUsage;

    fn msg(role: MsgRole, content: &str) -> StoredMessage {
        StoredMessage {
            id: 0,
            role,
            content: content.to_string(),
            reasoning: Vec::new(),
            interrupted: false,
            seq: 0,
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

    fn summary_msg(content: &str) -> StoredMessage {
        let mut m = msg(MsgRole::Assistant, content);
        m.summary = true;
        m
    }

    fn record(name: &str, path: &str, message_seq: u64) -> ToolRecord {
        ToolRecord {
            name: name.to_string(),
            args_json: serde_json::json!({ "path": path }).to_string(),
            output: format!("contents of {path}"),
            stderr: String::new(),
            ok: true,
            killed: false,
            worker: None,
            message_id: 0,
            message_seq,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn select_plan_requires_more_than_min_tail() {
        let msgs: Vec<StoredMessage> = (0..MIN_TAIL_MESSAGES)
            .map(|_| msg(MsgRole::User, "hi"))
            .collect();
        assert!(select_plan(&msgs, 20_000).is_none());
        let bigger: Vec<StoredMessage> = (0..MIN_TAIL_MESSAGES + 3)
            .map(|_| msg(MsgRole::User, "hi"))
            .collect();
        let plan = select_plan(&bigger, 20_000).unwrap();
        assert_eq!(plan.start, 0);
        assert_eq!(
            plan.cut, 3,
            "small history floors at the message-count tail"
        );
    }

    #[test]
    fn select_plan_cut_honors_token_budget() {
        // 100k-token messages (400k chars); a 100k tail budget must keep the
        // last 100k-token message verbatim and cut before it.
        let big = "x".repeat(400_000);
        let msgs: Vec<StoredMessage> = (0..8)
            .map(|i| {
                if i == 1 || i == 3 {
                    msg(MsgRole::Assistant, &big)
                } else {
                    msg(MsgRole::User, "hi")
                }
            })
            .collect();
        let plan = select_plan(&msgs, 100_000).unwrap();
        assert_eq!(plan.start, 0);
        assert_eq!(plan.cut, 3, "the walk stops once the tail budget is met");
    }

    #[test]
    fn select_plan_resumes_from_previous_summary() {
        let mut msgs: Vec<StoredMessage> = Vec::new();
        for i in 0..10 {
            if i == 2 {
                msgs.push(summary_msg("prior summary"));
            } else {
                msgs.push(msg(MsgRole::User, &format!("m{i}")));
            }
        }
        let plan = select_plan(&msgs, 20_000).unwrap();
        assert_eq!(plan.start, 2, "head resumes at the previous summary");
        assert_eq!(plan.cut, 6, "floor tail applies from the span");
    }

    #[test]
    fn select_plan_none_when_summary_is_in_the_tail() {
        let mut msgs: Vec<StoredMessage> = (0..8)
            .map(|i| msg(MsgRole::User, &format!("m{i}")))
            .collect();
        let len = msgs.len();
        msgs[len - 2] = summary_msg("recent summary");
        assert!(
            select_plan(&msgs, 20_000).is_none(),
            "nothing new to summarize before the tail"
        );
    }

    #[test]
    fn serialize_head_includes_tool_activity() {
        let mut messages = vec![msg(MsgRole::User, "go"), msg(MsgRole::Assistant, "looking")];
        messages[1].id = 7;
        let mut rec = record("read_file", "src/main.rs", 1);
        rec.worker = Some("explore".into());
        rec.output = "fn main() {}".repeat(2_000);
        let s = serialize_head(&messages, &[rec], 0);
        assert!(s.contains("[read_file (explore) ok]"), "{s}");
        assert!(
            s.contains(&"fn main() {}".repeat(166)),
            "output truncated to the char cap: {s}"
        );
        assert!(!s.contains(&"fn main() {}".repeat(167)), "{s}");
    }

    #[test]
    fn serialize_head_matches_records_by_message_id() {
        // A record whose message_seq drifted (deleted rows re-append with
        // higher seqs) still lands on its assistant message via message_id.
        let mut messages = vec![msg(MsgRole::User, "go"), msg(MsgRole::Assistant, "hi")];
        messages[1].id = 42;
        let mut rec = record("grep", "src", 99);
        rec.message_id = 42;
        let s = serialize_head(&messages, &[rec], 0);
        assert!(s.contains("[grep ok]"), "record matched by id: {s}");
    }

    #[test]
    fn serialize_head_appends_file_lists() {
        let messages = vec![msg(MsgRole::User, "go"), msg(MsgRole::Assistant, "hi")];
        let mut edit = record("edit_file", "src/lib.rs", 1);
        edit.file_change = Some(FileChange::Edit {
            path: "src/lib.rs".into(),
            diff: Vec::new(),
            original: String::new(),
            new: String::new(),
        });
        let records = vec![record("read_file", "src/main.rs", 1), edit];
        let s = serialize_head(&messages, &records, 0);
        assert!(
            s.contains("<read-files>\nsrc/main.rs\n</read-files>"),
            "{s}"
        );
        assert!(
            s.contains("<modified-files>\nsrc/lib.rs\n</modified-files>"),
            "{s}"
        );
    }

    #[test]
    fn file_lists_dedup_paths() {
        let mut messages = vec![msg(MsgRole::User, "go"), msg(MsgRole::Assistant, "hi")];
        messages[1].id = 1;
        let records = vec![
            record("read_file", "src/main.rs", 1),
            record("grep", "src/main.rs", 1),
        ];
        let s = serialize_head(&messages, &records, 0);
        let read_section = s
            .split("<read-files>\n")
            .nth(1)
            .and_then(|rest| rest.split("</read-files>").next())
            .unwrap_or_default();
        assert_eq!(
            read_section.matches("src/main.rs").count(),
            1,
            "read list deduplicated: {s}"
        );
    }

    #[test]
    fn serialize_head_truncates_long_content() {
        let long = "x".repeat(TOOL_OUTPUT_MAX_CHARS + 100);
        let m = msg(MsgRole::User, &long);
        let s = serialize_head(&[m], &[], 0);
        assert!(s.contains("### User"));
        assert!(s.len() < long.len() + 100);
    }

    #[test]
    fn serialize_head_marks_prior_summary() {
        let m = summary_msg("## Objective\nDo stuff");
        let s = serialize_head(&[m], &[], 0);
        assert!(s.contains("prior summary"));
    }
}
