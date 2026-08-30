pub mod agents;
pub mod approval;
pub mod catalog;
pub mod command;
pub mod compaction;
pub mod config;
pub mod connections;
pub mod context;
pub mod core_task;
pub mod embeddings;
mod error;
pub mod event;
mod kdlserde;
pub mod lsp_manager;
pub mod question;
pub mod session;
pub mod skills;
pub mod test_util;
pub mod tool_record;
pub mod tools;

pub use approval::ApprovalReason;
pub use command::Command;
pub use config::{
    AgentConfig, Config, ContextConfig, EmbeddingConfig, LspConfigRepr, LspServerSpecRepr,
    SkillsConfig, UiPrefs,
};
pub use connections::{Connections, ProviderConfig};
pub use core_task::run;
pub use error::{ConfigParseError, CoreError, Result};
pub use event::Event;
pub use question::{QuestionPrompt, QuestionRequest};
pub use session::Session;
pub use shuvarie_db::{MsgRole, SearchHit, SearchSource, SessionSummary};
pub use shuvarie_llm::{ChatMsg, Model, Role, TokenUsage};
pub use shuvarie_lsp::{DiagnosticInfo, DiagnosticSeverity, LspStatus, ServerStatus};
pub use skills::{Skill, Skills};
