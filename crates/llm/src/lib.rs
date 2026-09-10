pub mod agent;
mod context_hook;
mod error;
pub mod file_change;
pub mod message;
pub mod model;
pub mod provider;
pub mod retry;
pub mod stream;
pub mod tool;
pub mod usage;

pub use agent::WorkerAgent;
pub use agent::WorkerRequest;
pub use context_hook::{
    ContextBudget, ContextHook, OVERFLOW_REASON, UsageTracker, estimate_text_tokens,
};
pub use error::{LlmError, Result};
pub use file_change::{
    DiffLine, DiffLineKind, FileChange, PatchFileChange, PatchFileKind, ShellStreams,
};
pub use message::{ChatMsg, Role};
pub use model::Model;
pub use provider::ProviderClient;
pub use retry::{ConnectionFailure, classify_connection_error};
pub use stream::{StreamItem, StreamStream};
pub use tool::{
    DynamicTool, FileChangeHook, PortableDynamicTool, Tool, ToolContext, ToolDefinition,
    ToolErrorKind, ToolExecutionError, ToolOutput, ToolSet, into_dynamic,
};
pub use usage::{TokenUsage, context_footprint, read_tokens};
