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
    Delete {
        path: String,
        /// Content of the file before deletion (`None` when it could not be
        /// read); undo restores it, redo removes the file.
        #[serde(default)]
        original: Option<String>,
    },
    Patch {
        files: Vec<PatchFileChange>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PatchFileKind {
    Add,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchFileChange {
    pub path: String,
    pub kind: PatchFileKind,
    /// For `Update` hunks with a `*** Move to:` target, the new path the
    /// file was moved to (`path` stays the source).
    #[serde(default)]
    pub moved_to: Option<String>,
    #[serde(default)]
    pub original: Option<String>,
    #[serde(default)]
    pub new: Option<String>,
    #[serde(default)]
    pub diff: Vec<DiffLine>,
}

impl FileChange {
    pub fn path(&self) -> &str {
        match self {
            FileChange::Edit { path, .. }
            | FileChange::Write { path, .. }
            | FileChange::Delete { path, .. } => path,
            FileChange::Patch { files, .. } => files.first().map(|f| f.path.as_str()).unwrap_or(""),
        }
    }

    pub fn new_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { new, .. } => Some(new.clone()),
            FileChange::Write { content, .. } => Some(content.clone()),
            FileChange::Delete { .. } => None,
            FileChange::Patch { files, .. } => files.iter().find_map(|f| f.new.clone()),
        }
    }

    pub fn original_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { original, .. } => Some(original.clone()),
            FileChange::Write { original, .. } => original.clone(),
            FileChange::Delete { original, .. } => original.clone(),
            FileChange::Patch { files, .. } => files.iter().find_map(|f| f.original.clone()),
        }
    }

    /// The per-file records behind this change as `(path, original, new)`
    /// triples. Undo writes `original` back when present (otherwise removes
    /// the file); redo writes `new` when present (otherwise removes it). A
    /// delete carries its `original` with `new` unset, so undo restores the
    /// content and redo removes the file. A
    /// move expands into two triples: the source path restoring its original
    /// (no new) and the target path carrying the new content (no original),
    /// so undo deletes the target and redo deletes the source.
    pub fn patch_files(&self) -> Vec<(&str, Option<&str>, Option<&str>)> {
        match self {
            FileChange::Edit {
                path,
                original,
                new,
                ..
            } => {
                vec![(path, Some(original.as_str()), Some(new.as_str()))]
            }
            FileChange::Write {
                path,
                content,
                original,
            } => {
                vec![(path, original.as_deref(), Some(content.as_str()))]
            }
            FileChange::Delete { path, original } => {
                vec![(path, original.as_deref(), None)]
            }
            FileChange::Patch { files } => files
                .iter()
                .flat_map(|f| match (&f.kind, &f.moved_to) {
                    (PatchFileKind::Update, Some(target)) => vec![
                        (f.path.as_str(), f.original.as_deref(), None),
                        (target.as_str(), None, f.new.as_deref()),
                    ],
                    _ => vec![(f.path.as_str(), f.original.as_deref(), f.new.as_deref())],
                })
                .collect(),
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
    /// Byte ranges within the row's text of the partially-edited runs to
    /// highlight with a background; empty when the whole row changed. The
    /// ranges never cover the trailing row break.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<(u32, u32)>,
}

/// Display-only stdout/stderr split for a `run_shell` call, attached to the
/// tool result via `ToolContext::insert_result` and captured alongside
/// [`FileChange`] by the same hook. The model-facing output stays combined.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellStreams {
    pub stdout: String,
    pub stderr: String,
}
