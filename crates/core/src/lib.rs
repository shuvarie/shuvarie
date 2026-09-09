pub mod agents;
pub mod apply_patch;
pub mod catalog;
pub mod command;
pub mod compaction;
pub mod config;
pub mod context;
pub mod core_task;
pub mod embeddings;
mod error;
pub mod event;
pub mod lsp_manager;
pub mod permissions;
pub mod question;
pub mod session;
pub mod shell;
pub mod skills;
#[cfg(test)]
pub(crate) mod test_util;
pub mod tool_record;
pub mod tools;
pub mod truncate;

pub use command::Command;
pub use config::{
    Active, AgentConfig, Config, Connections, ContextConfig, EmbeddingConfig, LspConfigRepr,
    LspServerSpecRepr, ProviderConfig, ShellConfig, SkillsConfig, UiPrefs,
};
pub use core_task::{StartupSession, run};
pub use error::{ConfigParseError, CoreError, Result};
pub use event::Event;
pub use question::{QuestionPrompt, QuestionRequest};
pub use session::Session;
pub use shuvarie_db::{MsgRole, SearchHit, SearchSource, SessionSummary};
pub use shuvarie_llm::{ChatMsg, Model, Role, TokenUsage};
pub use shuvarie_lsp::{DiagnosticInfo, DiagnosticSeverity, LspStatus, ServerStatus};
pub use skills::{Skill, SkillWarning, Skills};
pub use tools::todos::TodoState;
