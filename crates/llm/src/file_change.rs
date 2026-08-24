use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FileChange {
    Edit {
        path: String,
        diff: Vec<DiffLine>,
        #[serde(default)]
        original: String,
        #[serde(default)]
        new: String,
    },
    Write {
        path: String,
        content: String,
        #[serde(default)]
        original: Option<String>,
    },
}

impl FileChange {
    pub fn path(&self) -> &str {
        match self {
            FileChange::Edit { path, .. } | FileChange::Write { path, .. } => path,
        }
    }

    pub fn new_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { new, .. } => Some(new.clone()),
            FileChange::Write { content, .. } => Some(content.clone()),
        }
    }

    pub fn original_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { original, .. } => Some(original.clone()),
            FileChange::Write { original, .. } => original.clone(),
        }
    }
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
