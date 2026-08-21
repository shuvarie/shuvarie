use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

use crate::file_change::FileChange;

pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub file_change: Option<FileChange>,
}

impl ToolOutput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            file_change: None,
        }
    }

    pub fn with_file_change(text: impl Into<String>, file_change: FileChange) -> Self {
        Self {
            text: text.into(),
            file_change: Some(file_change),
        }
    }
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn call(&self, args: Value)
    -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>>;
}
