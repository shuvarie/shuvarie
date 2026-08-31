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
            FileChange::Edit { path, .. } | FileChange::Write { path, .. } => path,
            FileChange::Patch { files, .. } => files.first().map(|f| f.path.as_str()).unwrap_or(""),
        }
    }

    pub fn new_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { new, .. } => Some(new.clone()),
            FileChange::Write { content, .. } => Some(content.clone()),
            FileChange::Patch { files, .. } => files.iter().find_map(|f| f.new.clone()),
        }
    }

    pub fn original_content(&self) -> Option<String> {
        match self {
            FileChange::Edit { original, .. } => Some(original.clone()),
            FileChange::Write { original, .. } => original.clone(),
            FileChange::Patch { files, .. } => files.iter().find_map(|f| f.original.clone()),
        }
    }

    /// The per-file records behind this change as `(path, original, new)`
    /// triples. Undo writes `original` back when present (otherwise removes
    /// the file); redo writes `new` when present (otherwise removes it). A
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
}
