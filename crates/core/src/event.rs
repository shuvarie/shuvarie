use shuvarie_db::SessionSummary;
use shuvarie_llm::{ModelInfo, TokenUsage};

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
}
