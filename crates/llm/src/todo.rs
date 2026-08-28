use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    pub priority: String,
}

/// Host-only metadata attached to a `todo` tool call: the full updated task
/// list. The tool replaces the whole list on every call, so this carries the
/// complete new state rather than a delta.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TodoUpdate {
    pub todos: Vec<TodoItem>,
}
