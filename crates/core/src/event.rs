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
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        duration_ms: u64,
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
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
        duration_ms: u64,
    },
    StreamDone {
        text: String,
        usage: TokenUsage,
    },
    StreamError {
        error: String,
    },
    StreamCancelled,
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
    UsageUpdate {
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
    },
}
