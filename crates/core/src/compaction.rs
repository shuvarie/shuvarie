//! LLM-driven compaction: when the context budget is exceeded, summarize the
//! older portion of the conversation into a compact Markdown summary so the
//! session can continue without re-sending the full history on every turn.
//!
//! Modeled on OpenCode's compaction agent: serialize the "head" of the
//! session (older messages, with tool outputs truncated) into a text block,
//! keep a verbatim "tail" of recent turns, and ask the LLM to produce a
//! structured summary (Objective / Important Details / Work State / Next Move
//! / Relevant Files). The summary is persisted as a `summary`-flagged
//! assistant message; subsequent turns send `[summary, tail]` instead of the
//! full history.

use shuvarie_db::StoredMessage;
use shuvarie_llm::ProviderClient;

const TOOL_OUTPUT_MAX_CHARS: usize = 2_000;
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
Keep the summary concise and information-dense. Do not include full file \
contents or tool outputs — reference them by path and line where relevant.";

/// A compaction plan: the index where the head ends and the verbatim tail
/// begins.
pub struct CompactionPlan {
    /// Number of leading messages that will be summarized (the "head").
    pub head_count: usize,
}

/// Select the split point: keep the most recent messages verbatim (the
/// "tail") and summarize the rest (the "head"). The tail is at least
/// `MIN_TAIL_MESSAGES` and never the whole session.
pub fn select_plan(messages: &[StoredMessage]) -> Option<CompactionPlan> {
    if messages.len() <= MIN_TAIL_MESSAGES {
        return None;
    }
    Some(CompactionPlan {
        head_count: messages.len() - MIN_TAIL_MESSAGES,
    })
}

/// Serialize the head of the conversation into a text block for the
/// summarizer, truncating tool outputs to `TOOL_OUTPUT_MAX_CHARS`.
pub fn serialize_head(messages: &[StoredMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        let role = match m.role {
            shuvarie_db::MsgRole::User => "User",
            shuvarie_db::MsgRole::Assistant => "Assistant",
            shuvarie_db::MsgRole::System => "System",
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
        out.push('\n');
    }
    out
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let boundary = s
            .char_indices()
            .take(max)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(max);
        &s[..boundary]
    }
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

    fn msg(role: shuvarie_db::MsgRole, content: &str) -> StoredMessage {
        StoredMessage {
            id: 0,
            role,
            content: content.to_string(),
            reasoning: String::new(),
            interrupted: false,
            seq: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cached_input_tokens: 0,
            reasoning_tokens: 0,
            cost: 0.0,
            summary: false,
        }
    }

    #[test]
    fn select_plan_requires_more_than_min_tail() {
        let msgs: Vec<StoredMessage> = (0..MIN_TAIL_MESSAGES)
            .map(|_| msg(shuvarie_db::MsgRole::User, "hi"))
            .collect();
        assert!(select_plan(&msgs).is_none());
        let bigger: Vec<StoredMessage> = (0..MIN_TAIL_MESSAGES + 3)
            .map(|_| msg(shuvarie_db::MsgRole::User, "hi"))
            .collect();
        let plan = select_plan(&bigger).unwrap();
        assert_eq!(plan.head_count, 3);
    }

    #[test]
    fn serialize_head_truncates_long_content() {
        let long = "x".repeat(TOOL_OUTPUT_MAX_CHARS + 100);
        let m = msg(shuvarie_db::MsgRole::User, &long);
        let s = serialize_head(&[m]);
        assert!(s.contains("### User"));
        assert!(s.len() < long.len() + 100);
    }

    #[test]
    fn serialize_head_marks_prior_summary() {
        let mut m = msg(shuvarie_db::MsgRole::Assistant, "## Objective\nDo stuff");
        m.summary = true;
        let s = serialize_head(&[m]);
        assert!(s.contains("prior summary"));
    }
}
