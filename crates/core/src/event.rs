use shuvarie_db::SessionSummary;
use shuvarie_llm::FileChange;
use shuvarie_llm::{Model, ShellStreams, TokenUsage};

use crate::question::QuestionPrompt;

#[derive(Debug, Clone)]
pub enum Event {
    Pong,
    ModelsLoaded {
        provider_name: String,
        models: Vec<Model>,
    },
    ModelsError {
        provider_name: String,
        error: String,
    },
    ConfigSaved,
    ConfigError {
        error: String,
    },
    SessionStarted,
    SessionCreated {
        id: uuid::Uuid,
        title: String,
    },
    TokenReceived {
        content: String,
    },
    ReasoningReceived {
        content: String,
    },
    ContextLoaded {
        paths: Vec<String>,
    },
    ToolStarted {
        name: String,
        args: serde_json::Value,
        worker: Option<String>,
        call_id: String,
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        duration_ms: u64,
        call_id: String,
    },
    ToolOutput {
        tool: String,
        worker: Option<String>,
        stdout: String,
        stderr: String,
    },
    WorkerStarted {
        name: String,
        args: serde_json::Value,
        call_id: String,
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
        duration_ms: u64,
        call_id: String,
    },
    StreamDone {
        text: String,
        usage: TokenUsage,
    },
    /// The agent is busy, so a submitted prompt was queued (steered) instead
    /// of starting a new turn; it will be sent when the current agent-loop
    /// round completes.
    PromptSteered {
        content: String,
    },
    /// A new user turn started streaming: either an accepted `SendMessage`
    /// (`steered: false`) or a dispatched steered prompt (`steered: true`, in
    /// which case the first queued entry must leave the chat display).
    TurnStarted {
        content: String,
        steered: bool,
    },
    /// Reply to [`Command::RecallSteered`]: the recalled prompt content, or
    /// `None` when nothing was queued.
    SteeredRecalled {
        stacked: bool,
        content: Option<String>,
    },
    /// The steered queue was wiped by a session-level transition (new,
    /// loaded, or deleted session).
    SteeredCleared,
    StreamError {
        error: String,
    },
    StreamCancelled,
    /// The context budget overflowed and an LLM compaction summarizer call
    /// is now running. No stream events arrive until the matching
    /// [`Event::CompactionFinished`], so the TUI must keep its busy indicator
    /// armed for the whole window.
    CompactionStarted,
    /// The compaction summarizer call finished (success or failure); the
    /// core task replays the interrupted turn afterwards.
    CompactionFinished,
    /// A retryable connection failure occurred; the core task will re-send the
    /// turn after `delay_ms`. `attempt` is the upcoming retry number (1-based)
    /// out of `max_attempts` (from `[retry].max-retries`).
    RetryScheduled {
        reason: String,
        message: String,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
    },
    /// Per-request usage for one completed LLM request (main stream or
    /// worker), added to the session's running totals. `context_tokens` is
    /// the request's context footprint (see
    /// `shuvarie_llm::context_footprint`) — present only for main-stream
    /// requests, since workers run separate conversations — so the UI can
    /// anchor its context-occupancy display on the latest one.
    UsageUpdate {
        usage: TokenUsage,
        cost: f64,
        context_tokens: Option<u64>,
    },
    /// Authoritative cumulative usage for the active session, sent after a
    /// turn commits. Replaces (rather than adds to) any client-side running
    /// totals so live per-request updates resync to the persisted numbers.
    UsageSnapshot {
        usage: TokenUsage,
        cost: f64,
    },
    SessionsLoaded {
        sessions: Vec<SessionSummary>,
    },
    SessionLoaded {
        id: uuid::Uuid,
        title: String,
        session: crate::Session,
    },
    SessionDeleted {
        id: uuid::Uuid,
    },
    SessionError {
        error: String,
    },
    TurnReverted {
        session: crate::Session,
    },
    TurnRestored {
        session: crate::Session,
    },
    SearchResults {
        hits: Vec<shuvarie_db::SearchHit>,
    },
    SearchError {
        error: String,
    },
    QuestionAsked {
        id: u64,
        questions: Vec<QuestionPrompt>,
    },
    LspStatus {
        servers: Vec<shuvarie_lsp::LspStatus>,
    },
    LspDiagnostics {
        path: String,
        diagnostics: Vec<shuvarie_lsp::DiagnosticInfo>,
    },
    LspError {
        error: String,
    },
    SkillsLoaded {
        skills: Vec<crate::Skill>,
        warnings: Vec<crate::SkillWarning>,
    },
}
