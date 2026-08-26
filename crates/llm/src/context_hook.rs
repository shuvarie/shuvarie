//! Per-call context budget hook (rig `AgentHook`).
//!
//! Before every model call in the multi-turn agent loop, estimate the
//! request size (preamble + history + tool definitions) as roughly
//! `chars / 4`. When the estimate exceeds the usable budget
//! (`context_length - reserved`), replace the history sent this turn with a
//! trimmed copy: keep the most recent messages verbatim within a
//! "preserve-recent" tail budget, and replace older tool-result messages with
//! a short `[tool result omitted]` marker so the model knows a tool ran
//! without re-sending large outputs. This bounds the quadratic input-token
//! growth inherent to multi-turn agent loops (each model call re-sends the
//! full accumulated conversation, including every prior tool result).
//!
//! Only what is *sent* changes — rig's run state and persistence are
//! untouched (`RequestPatch.history` replaces the history for this turn
//! only).

use rig::agent::hook::{CompletionCall, ToolResultEvent};
use rig::agent::{
    AgentHook, CompletionCallAction, HookContext, RequestPatch, StepEventKind, ToolResultAction,
};
use rig::completion::Usage;
use rig::completion::message::{AssistantContent, Message, Text, ToolResult, ToolResultContent};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Sentinel stop reason used by the overflow path so `provider.rs` can
/// distinguish a budget stop from a real error.
pub const OVERFLOW_REASON: &str = "context overflow";

const CHARS_PER_TOKEN: usize = 4;

fn estimate_tokens(s: &str) -> u64 {
    (s.chars().count() / CHARS_PER_TOKEN) as u64
}

fn message_text_len(msg: &Message) -> usize {
    match msg {
        Message::System { content } => content.chars().count(),
        Message::User { content } => content
            .iter()
            .map(|c| match c {
                rig::completion::message::UserContent::Text(t) => t.text.chars().count(),
                rig::completion::message::UserContent::ToolResult(tr) => tool_result_len(tr),
                _ => 0,
            })
            .sum(),
        Message::Assistant { content, .. } => content
            .iter()
            .map(|c| match c {
                AssistantContent::Text(t) => t.text.chars().count(),
                AssistantContent::ToolCall(tc) => {
                    tc.function.name.len() + tc.function.arguments.to_string().len()
                }
                AssistantContent::Reasoning(r) => r
                    .content
                    .iter()
                    .map(|c| match c {
                        rig::completion::message::ReasoningContent::Text { text, .. } => {
                            text.chars().count()
                        }
                        rig::completion::message::ReasoningContent::Summary(s) => s.chars().count(),
                        _ => 0,
                    })
                    .sum(),
                _ => 0,
            })
            .sum(),
    }
}

fn tool_result_len(tr: &ToolResult) -> usize {
    tr.content
        .iter()
        .map(|c| match c {
            ToolResultContent::Text(t) => t.text.chars().count(),
            ToolResultContent::Json { value } => value.to_string().len(),
            _ => 0,
        })
        .sum()
}

/// Configuration for [`ContextHook`].
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Full model context window length.
    pub context_length: u64,
    /// Tokens reserved for the reply + safety buffer.
    pub reserved: u64,
    /// Disable trimming entirely (pass through unchanged).
    pub disabled: bool,
}

impl ContextBudget {
    pub fn new(context_length: u64, reserved: u64) -> Self {
        Self {
            context_length,
            reserved,
            disabled: false,
        }
    }

    pub fn usable(&self) -> u64 {
        self.context_length.saturating_sub(self.reserved)
    }

    fn preserve_recent(&self) -> u64 {
        let pct = self.usable() / 4;
        pct.clamp(2_000, 15_000)
    }
}

/// Shared, hook-visible accumulator of real provider input-token usage
/// across the model calls of a single run. Updated from the stream side
/// (`provider.rs` forwards `MultiTurnStreamItem::CompletionCall` items);
/// read by the hook to stop the run once the cumulative real usage crosses
/// the budget.
#[derive(Default)]
pub struct UsageTracker {
    pub cumulative_input: AtomicU64,
    pub overflowed: std::sync::atomic::AtomicBool,
}

impl UsageTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record(&self, usage: Usage) {
        self.cumulative_input
            .fetch_add(usage.input_tokens, Ordering::Relaxed);
    }

    pub fn input(&self) -> u64 {
        self.cumulative_input.load(Ordering::Relaxed)
    }

    pub fn set_overflow(&self) {
        self.overflowed.store(true, Ordering::Relaxed);
    }

    pub fn take_overflow(&self) -> bool {
        self.overflowed.swap(false, Ordering::Relaxed)
    }
}

/// rig `AgentHook` that bounds per-call input tokens by trimming old tool
/// results from the history, and stops the run once real cumulative usage
/// overflows the budget.
pub struct ContextHook {
    budget: ContextBudget,
    /// Last estimated prompt-token count (for telemetry/diagnostics).
    last_estimate: Mutex<u64>,
    tracker: Arc<UsageTracker>,
}

impl ContextHook {
    pub fn new(budget: ContextBudget, tracker: Arc<UsageTracker>) -> Self {
        Self {
            budget,
            last_estimate: Mutex::new(0),
            tracker,
        }
    }

    fn estimate_request(&self, prompt: &Message, history: &[Message]) -> u64 {
        let prompt_tokens = estimate_tokens(&message_text_len(prompt).to_string());
        let history_tokens: u64 = history
            .iter()
            .map(|m| estimate_tokens(&message_text_len(m).to_string()))
            .sum();
        prompt_tokens + history_tokens
    }

    /// Build a trimmed history that fits within the usable budget, keeping
    /// the most recent messages verbatim and replacing older tool-result
    /// user-messages with a short marker.
    fn trim_history(&self, history: &[Message]) -> Vec<Message> {
        let usable = self.budget.usable() as usize * CHARS_PER_TOKEN;
        let preserve = self.budget.preserve_recent() as usize * CHARS_PER_TOKEN;

        if history.is_empty() {
            return Vec::new();
        }

        // Walk from the end, accumulating the verbatim tail up to the
        // preserve-recent budget.
        let mut tail_len = 0usize;
        let mut split = history.len();
        for (i, msg) in history.iter().enumerate().rev() {
            let len = message_text_len(msg);
            if tail_len + len > preserve {
                split = i + 1;
                break;
            }
            tail_len += len;
            if i == 0 {
                split = 0;
            }
        }

        let mut out: Vec<Message> = Vec::with_capacity(history.len());
        // Head: replace tool-result user messages with a marker; keep
        // user/assistant text.
        for msg in &history[..split] {
            out.push(condense_message(msg));
        }
        // Tail: keep verbatim.
        out.extend_from_slice(&history[split..]);

        // If still over budget, drop from the front (oldest) until it fits.
        let mut total: usize = out.iter().map(message_text_len).sum();
        while total > usable && out.len() > 1 {
            let dropped = out.remove(0);
            total = total.saturating_sub(message_text_len(&dropped));
        }
        if total > usable && !out.is_empty() {
            // Single message still too big; keep the most recent one only.
            let last = out.split_off(out.len().saturating_sub(1));
            out = last;
        }
        out
    }
}

/// Replace a message's tool-result content with a short marker, preserving
/// the message's role/structure so the conversation stays valid.
fn condense_message(msg: &Message) -> Message {
    match msg {
        Message::User { content } => {
            let has_tool_result = content
                .iter()
                .any(|c| matches!(c, rig::completion::message::UserContent::ToolResult(_)));
            if !has_tool_result {
                return msg.clone();
            }
            let condensed: Vec<rig::completion::message::UserContent> = content
                .iter()
                .map(|c| match c {
                    rig::completion::message::UserContent::ToolResult(tr) => {
                        rig::completion::message::UserContent::ToolResult(ToolResult {
                            call: tr.call.clone(),
                            provider: tr.provider.clone(),
                            name: tr.name.clone(),
                            content: vec![ToolResultContent::Text(Text::new(
                                "[tool result omitted to fit context budget]".to_string(),
                            ))],
                        })
                    }
                    other => other.clone(),
                })
                .collect();
            Message::User { content: condensed }
        }
        _ => msg.clone(),
    }
}

impl AgentHook for ContextHook {
    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCall<'_>,
    ) -> impl futures_util::Future<Output = CompletionCallAction> + Send {
        let estimate = self.estimate_request(event.prompt, event.history);
        *self.last_estimate.lock().unwrap() = estimate;
        let tracker_input = self.tracker.input();
        let overflow_flagged = self.tracker.take_overflow();
        let usable = self.budget.usable();
        let disabled = self.budget.disabled;
        let need_trim = !disabled && estimate >= usable;
        let trimmed = if need_trim {
            Some(self.trim_history(event.history))
        } else {
            None
        };
        async move {
            if overflow_flagged || estimate >= usable || tracker_input >= usable {
                return CompletionCallAction::Stop(OVERFLOW_REASON.to_string());
            }
            match trimmed {
                Some(history) => CompletionCallAction::Patch(RequestPatch::new().history(history)),
                None => CompletionCallAction::Continue,
            }
        }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl futures_util::Future<Output = ToolResultAction> + Send {
        let text = event.presentation.render();
        let max = self.budget.usable() as usize / 8;
        let cap = max.clamp(2_000, 16_000);
        let chars = text.chars().count();
        let action = if chars <= cap {
            ToolResultAction::Keep
        } else {
            let omitted = chars - cap;
            let truncated: String = text.chars().take(cap).collect();
            let note = format!(
                "{truncated}\n… (tool output truncated: {omitted} chars omitted to fit context budget)"
            );
            ToolResultAction::rewrite(note)
        };
        async move { action }
    }

    fn observes(&self, _kind: StepEventKind) -> bool {
        false
    }
}
