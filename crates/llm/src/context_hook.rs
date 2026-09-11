//! Per-call context budget hook (rig `AgentHook`).
//!
//! Forecast, trim, stop: before every model call in the multi-turn agent
//! loop, forecast the request's input tokens. The forecast is anchored on the
//! **real** request-side token cost reported by the most recent model call
//! (`total_tokens - output_tokens`, which works across providers regardless
//! of whether cached tokens are reported inside or alongside `input_tokens`),
//! plus a chars/4 estimate of only what was appended since that call. With no
//! usage anchor yet (first call of the run, or a provider that reports no
//! usage), the forecast falls back to chars/4 over the whole request plus the
//! preamble's estimated tokens (the preamble itself is not visible to hooks).
//!
//! When the forecast exceeds the usable budget (`context_length - reserved`),
//! the hook applies a mechanical trim: keep the most recent messages verbatim
//! within the `keep_recent_tokens` tail budget and replace older tool-result
//! content with recovery-hint stubs so the model knows a tool ran and how to
//! re-read it. If even the trimmed request would not fit, the hook stops the
//! run *before* the provider call so the caller can compact the session
//! (LLM-summzed summary) and auto-continue — the model never sees a failed
//! request. Only what is *sent* changes: rig's run state and persistence are
//! untouched (`RequestPatch.history` replaces the history for this turn only).

use rig_agent::agent::hook::{CompletionCall, ToolResultEvent};
use rig_agent::agent::{
    AgentHook, CompletionCallAction, HookContext, RequestPatch, StepEventKind, ToolResultAction,
};
use rig_core::completion::Usage;
use rig_core::completion::message::{
    AssistantContent, Message, Text, ToolResult, ToolResultContent,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Sentinel stop reason used by the overflow path so `provider.rs` can
/// distinguish a budget stop from a real error.
pub const OVERFLOW_REASON: &str = "context overflow";

const CHARS_PER_TOKEN: usize = 4;
const DEFAULT_KEEP_RECENT_TOKENS: u64 = 20_000;
const DEFAULT_TOOL_OUTPUT_MAX_CHARS: usize = 16_000;

/// Estimate a text's token count with the chars/4 heuristic.
pub fn estimate_text_tokens(text: &str) -> u64 {
    text.chars().count() as u64 / CHARS_PER_TOKEN as u64
}

/// Request-side token cost of a completed call: everything the provider had
/// in context when generating (prompt tokens, including or plus whatever the
/// provider reports about cached prefixes), excluding the generated output.
fn request_tokens(usage: &Usage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens.saturating_sub(usage.output_tokens)
    } else {
        usage
            .input_tokens
            .saturating_add(usage.cached_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
    }
}

fn message_text_len(msg: &Message) -> usize {
    match msg {
        Message::System { content } => content.chars().count(),
        Message::User { content } => content
            .iter()
            .map(|c| match c {
                rig_core::completion::message::UserContent::Text(t) => t.text.chars().count(),
                rig_core::completion::message::UserContent::ToolResult(tr) => tool_result_len(tr),
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
                        rig_core::completion::message::ReasoningContent::Text { text, .. } => {
                            text.chars().count()
                        }
                        rig_core::completion::message::ReasoningContent::Summary(s) => {
                            s.chars().count()
                        }
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

/// Configuration for [`ContextBudget`].
#[derive(Debug, Clone)]
pub struct ContextBudget {
    /// Full model context window length.
    pub context_length: u64,
    /// Tokens reserved for the reply + safety buffer.
    pub reserved: u64,
    /// Tokens kept verbatim as the "tail" when trimming older messages.
    pub keep_recent_tokens: u64,
    /// Estimated tokens of the system preamble, which hook events do not
    /// include; added to the unanchored chars/4 forecast.
    pub preamble_tokens: u64,
    /// Maximum chars of a tool result's text sent to the model. `0` disables
    /// the per-result cap (the tool layer may still truncate).
    pub tool_output_max_chars: usize,
    /// Disable trimming, forecasts, and stops entirely (pass through).
    pub disabled: bool,
}

impl ContextBudget {
    pub fn new(context_length: u64, reserved: u64) -> Self {
        Self {
            context_length,
            reserved,
            keep_recent_tokens: DEFAULT_KEEP_RECENT_TOKENS,
            preamble_tokens: 0,
            tool_output_max_chars: DEFAULT_TOOL_OUTPUT_MAX_CHARS,
            disabled: false,
        }
    }

    pub fn with_keep_recent_tokens(mut self, tokens: u64) -> Self {
        self.keep_recent_tokens = tokens;
        self
    }

    pub fn with_preamble_tokens(mut self, tokens: u64) -> Self {
        self.preamble_tokens = tokens;
        self
    }

    pub fn with_tool_output_max_chars(mut self, chars: usize) -> Self {
        self.tool_output_max_chars = chars;
        self
    }

    pub fn usable(&self) -> u64 {
        self.context_length.saturating_sub(self.reserved)
    }

    fn keep_recent_chars(&self) -> usize {
        (self.keep_recent_tokens as usize).saturating_mul(CHARS_PER_TOKEN)
    }
}

/// Shared, hook-visible record of the real request-side token cost of the
/// most recent model call in a run. Updated from the stream side
/// (`provider.rs` forwards `MultiTurnStreamItem::CompletionCall` items); read
/// by the hook to anchor the next call's forecast.
///
/// This is the *last* call's request size, not a cumulative sum: each model
/// call re-sends the accumulated conversation, so per-request input is the
/// quantity that grows toward the budget.
#[derive(Default)]
pub struct UsageTracker {
    last_request_tokens: AtomicU64,
}

impl UsageTracker {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn record(&self, usage: Usage) {
        let tokens = request_tokens(&usage);
        if tokens > 0 {
            self.last_request_tokens.store(tokens, Ordering::Relaxed);
        }
    }

    pub fn input(&self) -> u64 {
        self.last_request_tokens.load(Ordering::Relaxed)
    }
}

/// Decision of the context-budget analysis for one model call.
#[derive(Debug)]
enum ContextDecision {
    /// Forecast is inside the budget; send the request as-is.
    Continue,
    /// Forecast exceeded the budget; send a trimmed history instead.
    Trim(Vec<Message>),
    /// Even the trimmed request would not fit; stop before the model call so
    /// the caller can compact the session and retry.
    Stop,
}

/// rig `AgentHook` that forecasts per-call input tokens, mechanically trims
/// old tool results when the forecast exceeds the budget, and stops the run
/// before a call that cannot fit even trimmed.
pub struct ContextHook {
    budget: ContextBudget,
    tracker: Arc<UsageTracker>,
    /// Char length of the request the hook last saw (or patched into) within
    /// the current run, used to size the growth delta against the anchor.
    last_request_chars: Mutex<Option<usize>>,
}

impl ContextHook {
    pub fn new(budget: ContextBudget, tracker: Arc<UsageTracker>) -> Self {
        Self {
            budget,
            tracker,
            last_request_chars: Mutex::new(None),
        }
    }

    fn request_chars(&self, prompt: &Message, history: &[Message]) -> usize {
        message_text_len(prompt) + history.iter().map(message_text_len).sum::<usize>()
    }

    /// Forecast input tokens for this request: the last call's real token
    /// cost plus a chars/4 estimate of what was appended since, or a plain
    /// chars/4 estimate of the whole request plus the preamble when no
    /// anchor exists.
    fn forecast(&self, request_chars: usize) -> u64 {
        let anchor = self.tracker.input();
        if anchor > 0 {
            let prev = self.last_request_chars.lock().unwrap();
            let prev = prev.unwrap_or(request_chars);
            let delta = (request_chars as i64 - prev as i64) / CHARS_PER_TOKEN as i64;
            (anchor as i64 + delta).max(0) as u64
        } else {
            request_chars as u64 / CHARS_PER_TOKEN as u64 + self.budget.preamble_tokens
        }
    }

    /// Decide what to do with this call's request. Also records the request
    /// char count consumed by the decision, which `forecast` diffs against on
    /// the following call.
    fn decide(&self, prompt: &Message, history: &[Message]) -> ContextDecision {
        if self.budget.disabled {
            return ContextDecision::Continue;
        }
        let chars = self.request_chars(prompt, history);
        if self.forecast(chars) < self.budget.usable() {
            *self.last_request_chars.lock().unwrap() = Some(chars);
            return ContextDecision::Continue;
        }
        let trimmed = self.trim_history(history);
        let trimmed_chars =
            message_text_len(prompt) + trimmed.iter().map(message_text_len).sum::<usize>();
        if self.forecast(trimmed_chars) >= self.budget.usable() {
            return ContextDecision::Stop;
        }
        *self.last_request_chars.lock().unwrap() = Some(trimmed_chars);
        ContextDecision::Trim(trimmed)
    }

    /// Build a trimmed history that keeps the most recent messages verbatim
    /// within the `keep_recent_tokens` tail budget and replaces older
    /// tool-result content with recovery-hint stubs. Text messages are never
    /// dropped or truncated — if the result still does not fit, the caller
    /// stops the run instead.
    fn trim_history(&self, history: &[Message]) -> Vec<Message> {
        let preserve = self.budget.keep_recent_chars();
        let _ = preserve;

        if history.is_empty() {
            return Vec::new();
        }

        // Walk from the end, accumulating the verbatim tail up to the
        // keep-recent budget.
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

        // Head: replace tool-result content with recovery-hint stubs; keep
        // user/assistant text. Tail: keep verbatim.
        let mut out: Vec<Message> = Vec::with_capacity(history.len());
        for msg in &history[..split] {
            out.push(condense_message(msg));
        }
        out.extend_from_slice(&history[split..]);
        out
    }
}

/// Recovery-hint stub for an elided tool result: the model knows which tool
/// ran, how large the result was, and that re-running the tool recovers it.
fn recovery_hint(tr: &ToolResult) -> String {
    let name = tr.name.clone();
    let chars = tool_result_len(tr);
    format!("[elided: {name} result ({chars} chars) — re-run {name} if you need it]")
}

/// Replace a message's tool-result content with a short recovery-hint stub,
/// preserving the message's role/structure so the conversation stays valid.
fn condense_message(msg: &Message) -> Message {
    match msg {
        Message::User { content } => {
            let has_tool_result = content
                .iter()
                .any(|c| matches!(c, rig_core::completion::message::UserContent::ToolResult(_)));
            if !has_tool_result {
                return msg.clone();
            }
            let condensed: Vec<rig_core::completion::message::UserContent> = content
                .iter()
                .map(|c| match c {
                    rig_core::completion::message::UserContent::ToolResult(tr) => {
                        rig_core::completion::message::UserContent::ToolResult(ToolResult {
                            call: tr.call.clone(),
                            provider: tr.provider.clone(),
                            name: tr.name.clone(),
                            content: vec![ToolResultContent::Text(Text::new(recovery_hint(tr)))],
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

/// Truncate an oversized tool output to `cap` chars; `None` when the cap is
/// disabled or the text fits.
fn truncate_tool_output(text: &str, cap: usize) -> Option<String> {
    if cap == 0 {
        return None;
    }
    let chars = text.chars().count();
    if chars <= cap {
        return None;
    }
    let omitted = chars - cap;
    let truncated: String = text.chars().take(cap).collect();
    Some(format!(
        "{truncated}\n… (tool output truncated: {omitted} chars omitted to fit context budget)"
    ))
}

impl AgentHook for ContextHook {
    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCall<'_>,
    ) -> impl futures_util::Future<Output = CompletionCallAction> + Send {
        let decision = self.decide(event.prompt, event.history);
        async move {
            match decision {
                ContextDecision::Continue => CompletionCallAction::Continue,
                ContextDecision::Trim(history) => {
                    CompletionCallAction::Patch(RequestPatch::new().history(history))
                }
                ContextDecision::Stop => CompletionCallAction::Stop(OVERFLOW_REASON.to_string()),
            }
        }
    }

    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl futures_util::Future<Output = ToolResultAction> + Send {
        let text = event.presentation.render();
        let action = match truncate_tool_output(&text, self.budget.tool_output_max_chars) {
            Some(note) => ToolResultAction::rewrite(note),
            None => ToolResultAction::Keep,
        };
        async move { action }
    }

    fn observes(&self, _kind: StepEventKind) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig_core::completion::message::{Message, Text, ToolCallId, ToolResult, UserContent};

    fn user_msg(text: &str) -> Message {
        Message::User {
            content: vec![UserContent::Text(Text::new(text.to_string()))],
        }
    }

    fn tool_result_msg(name: &str, content: &str) -> Message {
        Message::User {
            content: vec![UserContent::ToolResult(ToolResult {
                call: ToolCallId::mint(),
                provider: None,
                name: name.to_string(),
                content: vec![ToolResultContent::Text(Text::new(content.to_string()))],
            })],
        }
    }

    fn usage_of(total: u64, output: u64) -> Usage {
        let mut usage = Usage::new();
        usage.total_tokens = total;
        usage.output_tokens = output;
        usage
    }

    #[test]
    fn estimate_text_tokens_uses_chars_per_token() {
        assert_eq!(estimate_text_tokens(&"x".repeat(4_000)), 1_000);
        assert_eq!(estimate_text_tokens(""), 0);
    }

    #[test]
    fn request_tokens_subtracts_output_from_total() {
        let usage = usage_of(10_000, 2_000);
        assert_eq!(request_tokens(&usage), 8_000);
        assert_eq!(request_tokens(&Usage::new()), 0);
    }

    #[test]
    fn usage_tracker_keeps_last_call_not_cumulative() {
        let tracker = UsageTracker::new();
        tracker.record(usage_of(30_000, 2_000));
        tracker.record(usage_of(50_000, 2_000));
        assert_eq!(tracker.input(), 48_000);
        let zero = Usage::new();
        tracker.record(zero);
        assert_eq!(tracker.input(), 48_000);
    }

    #[test]
    fn first_call_uses_chars_per_token_plus_preamble() {
        let budget = ContextBudget::new(128_000, 20_000).with_preamble_tokens(3_000);
        let hook = ContextHook::new(budget, UsageTracker::new());
        // 40 000 chars / 4 = 10 000 tokens, + 3 000 preamble = 13 000.
        let decision = hook.decide(&user_msg("x"), &[user_msg(&"y".repeat(40_000))]);
        assert!(matches!(decision, ContextDecision::Continue));
    }

    #[test]
    fn anchored_forecast_uses_last_call_delta() {
        let budget = ContextBudget::new(12_000, 2_000).with_keep_recent_tokens(1_000);
        let tracker = UsageTracker::new();
        let hook = ContextHook::new(budget, Arc::clone(&tracker));
        // Establish the previous request's char baseline (one small result).
        hook.decide(
            &user_msg("x"),
            &[tool_result_msg("read_file", &"y".repeat(4_000))],
        );
        // Anchor arrives from the stream side: previous call costed 9 000
        // request tokens (total 9 500, output 500) — just under the 10 000
        // usable budget.
        tracker.record(usage_of(9_500, 500));
        // Nothing appended: forecast stays at the anchor (9 000 < 10 000 usable).
        let decision = hook.decide(
            &user_msg("x"),
            &[tool_result_msg("read_file", &"y".repeat(4_000))],
        );
        assert!(
            matches!(decision, ContextDecision::Continue),
            "{decision:?}"
        );
        // Grow the history by ~100 000 chars ≈ 25 000 tokens → forecast ≈
        // 35 000, but the growth is a tool result the trim can stub away.
        let history = vec![
            tool_result_msg("read_file", &"y".repeat(4_000)),
            tool_result_msg("run_shell", &"z".repeat(100_000)),
        ];
        let decision = hook.decide(&user_msg("x"), &history);
        assert!(matches!(decision, ContextDecision::Trim(_)), "{decision:?}");
    }

    #[test]
    fn trim_stubs_old_tool_results_with_recovery_hint() {
        let budget = ContextBudget::new(128_000, 20_000);
        let hook = ContextHook::new(budget, UsageTracker::new());
        let big = "z".repeat(100_000);
        let history = vec![tool_result_msg("read_file", &big), user_msg("recent user")];
        let trimmed = hook.trim_history(&history);
        assert_eq!(trimmed.len(), 2);
        let Message::User { content } = trimmed.first().unwrap() else {
            panic!("expected user message");
        };
        let text = content.iter().find_map(|c| match c {
            UserContent::ToolResult(tr) => tr.content.iter().find_map(|cc| match cc {
                ToolResultContent::Text(t) => Some(t.text.clone()),
                _ => None,
            }),
            _ => None,
        });
        let text = text.expect("stub text");
        assert!(text.contains("elided"), "{text}");
        assert!(text.contains("read_file"), "{text}");
        assert!(text.contains("re-run read_file"), "{text}");
    }

    #[test]
    fn trim_keeps_recent_tail_verbatim() {
        let budget = ContextBudget::new(128_000, 20_000).with_keep_recent_tokens(1_000);
        let hook = ContextHook::new(budget, UsageTracker::new());
        let big = "z".repeat(100_000);
        let tail = "recent".to_string();
        let history = vec![
            tool_result_msg("read_file", &big),
            user_msg("old user"),
            user_msg(&tail),
        ];
        let trimmed = hook.trim_history(&history);
        match &trimmed[2] {
            Message::User { content } => match &content[0] {
                UserContent::Text(t) => assert_eq!(t.text, tail),
                other => panic!("expected text message, got {other:?}"),
            },
            other => panic!("expected user message, got {other:?}"),
        }
    }

    #[test]
    fn stop_when_even_trimmed_request_would_overflow() {
        // Usable budget 19 000 tokens; a single user message of 100 000 chars
        // (25 000 tokens) plus a 2 000-token keep-recent floor cannot fit.
        let budget = ContextBudget::new(20_000, 1_000).with_keep_recent_tokens(2_000);
        let hook = ContextHook::new(budget, UsageTracker::new());
        let decision = hook.decide(&user_msg(&"y".repeat(100_000)), &[]);
        assert!(matches!(decision, ContextDecision::Stop), "{decision:?}");
    }

    #[test]
    fn disabled_budget_passes_through() {
        let mut budget = ContextBudget::new(20_000, 1_000).with_preamble_tokens(100_000);
        budget.disabled = true;
        let hook = ContextHook::new(budget, UsageTracker::new());
        let decision = hook.decide(&user_msg(&"y".repeat(1_000_000)), &[]);
        assert!(matches!(decision, ContextDecision::Continue));
    }

    #[test]
    fn truncate_tool_output_respects_cap() {
        let text = "a".repeat(1_500);
        let truncated = truncate_tool_output(&text, 1_200).expect("should truncate");
        assert!(truncated.starts_with(&"a".repeat(1_200)));
        assert!(truncated.contains("300 chars omitted"));
        assert!(truncate_tool_output(&"a".repeat(1_000), 1_200).is_none());
        assert!(truncate_tool_output("small", 0).is_none());
        assert!(truncate_tool_output(&"a".repeat(1_300), 1_200).is_some());
    }
}
