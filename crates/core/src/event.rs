use shuvarie_db::SessionSummary;
use shuvarie_llm::FileChange;
use shuvarie_llm::{Model, TokenUsage};

use crate::approval::ApprovalReason;
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
    },
    ToolOutput {
        tool: String,
        worker: Option<String>,
        content: String,
    },
    WorkerStarted {
        name: String,
        args: serde_json::Value,
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
    },
    StreamDone {
        text: String,
        usage: TokenUsage,
    },
    StreamError {
        error: String,
    },
    StreamCancelled,
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
    ApprovalRequest {
        id: u64,
        tool: String,
        path: String,
        reason: ApprovalReason,
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
