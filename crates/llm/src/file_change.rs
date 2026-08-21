use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FileChange {
    Edit { path: String, diff: Vec<DiffLine> },
    Write { path: String, content: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffLineKind {
    Context,
    Add,
    Remove,
    Ellipsis,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u64>,
    pub new_line: Option<u64>,
    pub text: String,
}
