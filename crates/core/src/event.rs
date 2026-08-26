use shuvarie_catalog::{ModelInfo, TokenUsage};
use shuvarie_db::SessionSummary;
use shuvarie_llm::FileChange;

use crate::approval::ApprovalReason;

#[derive(Debug, Clone)]
pub enum Event {
    Pong,
    ModelsLoaded {
        provider_name: String,
        models: Vec<ModelInfo>,
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
        id: u64,
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
        id: u64,
        title: String,
        session: crate::Session,
    },
    SessionDeleted {
        id: u64,
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
