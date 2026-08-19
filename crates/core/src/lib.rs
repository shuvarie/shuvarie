pub mod config;
pub mod core_task;
mod error;

pub mod command;
pub mod event;
pub mod session;

pub use command::Command;
pub use config::{Config, ProviderConfig, UiPrefs};
pub use core_task::run;
pub use error::{CoreError, Result};
pub use event::Event;
pub use session::Session;
pub use shuvarie_db::SessionSummary;
pub use shuvarie_llm::{ChatMsg, ModelInfo, Role};
