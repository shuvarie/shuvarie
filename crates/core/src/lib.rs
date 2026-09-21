pub mod agents;
pub mod apply_patch;
pub mod catalog;
pub mod command;
pub mod compaction;
pub mod context;
pub mod core_task;
pub mod embeddings;
pub mod event;
pub mod lsp_manager;
pub mod mcp_manager;
pub mod permissions;
pub mod question;
pub mod scenes;
pub mod session;
pub mod shell;
pub mod skills;
#[cfg(test)]
pub(crate) mod test_util;
pub mod title;
pub mod tool_record;
pub mod tools;
pub mod truncate;

pub use command::Command;
pub use core_task::{StartupSession, run};
pub use event::Event;
pub use permissions::{
    Access, AskScope, Decision, PathKind, PermissionAnswer, PermissionGate, PermissionRequest,
    Permissions,
};
pub use question::{QuestionPrompt, QuestionRequest};
pub use session::Session;
pub use shuvarie_config::*;
pub use shuvarie_db::{MsgRole, SearchHit, SearchSource, SessionSummary, StoredScroll};
pub use shuvarie_llm::{ChatMsg, Model, Role, TokenUsage};
pub use shuvarie_lsp::{DiagnosticInfo, DiagnosticSeverity, LspStatus, ServerStatus};
pub use shuvarie_mcp::{McpStatus, McpStatusState};
pub use skills::{Skill, SkillWarning, Skills};
pub use tools::todos::TodoState;
