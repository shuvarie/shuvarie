mod delete_file;
mod edit_file;
mod glob;
mod grep;
mod list_dir;
mod lsp;
mod question;
mod read_file;
mod run_shell;
pub mod todos;
mod web_search;
mod webfetch;
mod write_file;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::Value;
use shuvarie_llm::{DiffLine, DiffLineKind, DynamicTool, Tool};

use crate::WebSearchConfig;
use crate::lsp_manager::SharedManager;
use crate::permissions::Access;
use crate::question::QuestionGate;
use crate::shell::Shell;

use edit_file::EditFile;
use glob::Glob;
use grep::Grep;
use list_dir::ListDir;
use lsp::Lsp;
use question::Question;
use read_file::ReadFile;
use run_shell::RunShell;
use web_search::WebSearch;
use webfetch::WebFetch;
use write_file::WriteFile;

use delete_file::DeleteFile;

pub(crate) use run_shell::run_shell_command;
pub use run_shell::{ShellChunk, ShellOutputTx};

const DEFAULT_TIMEOUT_SECS: u64 = 30;

type ReadKey = (String, Option<u64>, Option<u64>);

/// Per-turn dedupe cache for `read_file`: tracks `(path, offset, limit)` keys
/// that have already been returned to the model this turn, plus a set of all
/// paths read this turn (any range) used by `write_file`'s read-first check
/// for overwrites. Repeated identical reads get a short note instead of
/// re-sending file content, which keeps the agent loop from blowing up the
/// context by re-reading the same large file.
#[derive(Clone, Default)]
pub struct ReadCache {
    seen: Arc<Mutex<std::collections::HashSet<ReadKey>>>,
    read_paths: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl ReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this exact `(path, offset, limit)` was already read
    /// this turn, otherwise records it and returns `false`.
    fn mark(&self, path: &str, offset: Option<u64>, limit: Option<u64>) -> bool {
        let key = (path.to_string(), offset, limit);
        let mut seen = self.seen.lock().unwrap();
        let fresh = seen.insert(key);
        if fresh {
            self.read_paths.lock().unwrap().insert(path.to_string());
        }
        !fresh
    }

    /// Returns `true` if the path was read (any range) earlier this turn.
    pub(crate) fn was_read(&self, path: &str) -> bool {
        self.read_paths.lock().unwrap().contains(path)
    }
}

/// Per-target-file mutation locks shared by `write_file`, `edit_file`, and
/// `apply_patch`, so worker agents and the manager never interleave
/// read-modify-write spans on the same file. Different files stay parallel.
#[derive(Clone, Default)]
pub struct FileLocks {
    locks: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl FileLocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn lock(&self, path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = self
            .locks
            .lock()
            .await
            .entry(path.to_owned())
            .or_default()
            .clone();
        entry.lock_owned().await
    }
}

pub(crate) fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn workspace_root() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cwd: {e}"))
}

pub(crate) fn compute_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut options = similar::InlineChangeOptions::new();
    options.mode(similar::InlineChangeMode::UnicodeWords);
    let mut lines: Vec<DiffLine> = Vec::new();
    for group in diff.grouped_ops(3) {
        if !lines.is_empty() {
            lines.push(DiffLine {
                kind: DiffLineKind::Ellipsis,
                old_line: None,
                new_line: None,
                text: String::new(),
                edits: Vec::new(),
            });
        }
        for op in group {
            for change in diff.iter_inline_changes_with_options(&op, options) {
                let (kind, old_line, new_line) = match change.tag() {
                    similar::ChangeTag::Delete => (DiffLineKind::Remove, change.old_index(), None),
                    similar::ChangeTag::Insert => (DiffLineKind::Add, None, change.new_index()),
                    similar::ChangeTag::Equal => (
                        DiffLineKind::Context,
                        change.old_index(),
                        change.new_index(),
                    ),
                };
                let old_line = old_line.map(|i| i as u64 + 1);
                let new_line = new_line.map(|i| i as u64 + 1);
                let mut text = String::new();
                let mut edits = Vec::new();
                for &(emphasized, value) in change.values() {
                    let start = text.len() as u32;
                    text.push_str(value);
                    if emphasized {
                        edits.push((start, text.len() as u32));
                    }
                }
                let trimmed = text.trim_end_matches(['\r', '\n']).len() as u32;
                edits.retain_mut(|(start, end)| {
                    *end = (*end).min(trimmed);
                    *start < *end
                });
                lines.push(DiffLine {
                    kind,
                    old_line,
                    new_line,
                    text,
                    edits,
                });
            }
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
pub fn all_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    question_gate: QuestionGate,
    access: Access,
    shell_tx: ShellOutputTx,
    shell: Shell,
    todo_state: todos::TodoState,
    web_search: Option<&WebSearchConfig>,
) -> Vec<DynamicTool> {
    let mut tools = vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile::new(
                read_cache.clone(),
                max_output_chars,
                max_output_bytes,
                access.clone(),
            ),
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile::new(
                read_cache.clone(),
                Some(lsp.clone()),
                locks.clone(),
                access.clone(),
            ),
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile::new(Some(lsp.clone()), locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "apply_patch",
            crate::apply_patch::ApplyPatch::new(Some(lsp.clone()), locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "delete_file",
            DeleteFile::new(locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "run_shell",
            RunShell::new(shell_tx.clone(), shell.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir::new(access.clone())),
        shuvarie_llm::into_dynamic("grep", Grep::new(access.clone())),
        shuvarie_llm::into_dynamic("glob", Glob::new(access.clone())),
        shuvarie_llm::into_dynamic("lsp", Lsp::new(lsp)),
        shuvarie_llm::into_dynamic("webfetch", WebFetch::new(max_output_chars)),
        shuvarie_llm::into_dynamic("question", Question::new(question_gate)),
        shuvarie_llm::into_dynamic(todos::Todo::NAME, todos::Todo::new(todo_state)),
    ];
    if let Some(config) = web_search {
        tools.push(shuvarie_llm::into_dynamic(
            WebSearch::NAME,
            WebSearch::new(config, max_output_chars),
        ));
    }
    tools
}

pub fn read_tools(
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
    web_search: Option<&WebSearchConfig>,
) -> Vec<DynamicTool> {
    let mut tools = vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile::new(
                read_cache,
                max_output_chars,
                max_output_bytes,
                access.clone(),
            ),
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir::new(access.clone())),
        shuvarie_llm::into_dynamic("grep", Grep::new(access.clone())),
        shuvarie_llm::into_dynamic("glob", Glob::new(access.clone())),
        shuvarie_llm::into_dynamic("lsp", Lsp::new(lsp)),
        shuvarie_llm::into_dynamic("webfetch", WebFetch::new(max_output_chars)),
    ];
    if let Some(config) = web_search {
        tools.push(shuvarie_llm::into_dynamic(
            WebSearch::NAME,
            WebSearch::new(config, max_output_chars),
        ));
    }
    tools
}

pub fn command_tools(shell_tx: ShellOutputTx, shell: Shell, access: Access) -> Vec<DynamicTool> {
    vec![shuvarie_llm::into_dynamic(
        "run_shell",
        RunShell::new(shell_tx, shell, access),
    )]
}

pub fn edit_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
) -> Vec<DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile::new(
                read_cache.clone(),
                max_output_chars,
                max_output_bytes,
                access.clone(),
            ),
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile::new(read_cache, Some(lsp.clone()), locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile::new(Some(lsp.clone()), locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "apply_patch",
            crate::apply_patch::ApplyPatch::new(Some(lsp.clone()), locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic(
            "delete_file",
            DeleteFile::new(locks.clone(), access.clone()),
        ),
        shuvarie_llm::into_dynamic("lsp", Lsp::new(lsp)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{resolve_read, resolve_write};
    use crate::test_util::tempdir;
    use tempfile::TempDir;

    #[test]
    fn diff_computes_line_numbers_and_ellipsis() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let new = "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Remove));
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Add));
        let remove = diff
            .iter()
            .find(|l| l.kind == DiffLineKind::Remove)
            .unwrap();
        assert_eq!(remove.old_line, Some(3));
        assert_eq!(remove.text, "three\n");
        let add = diff.iter().find(|l| l.kind == DiffLineKind::Add).unwrap();
        assert_eq!(add.new_line, Some(3));
        assert_eq!(add.text, "THREE\n");
    }

    #[test]
    fn diff_inserts_ellipsis_between_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Ellipsis));
    }

    #[test]
    fn diff_marks_partial_edits() {
        let lines = compute_diff("let value = compute(x);\n", "let value = compute(y);\n");
        assert_eq!(
            (lines[0].kind, lines[1].kind),
            (DiffLineKind::Remove, DiffLineKind::Add)
        );
        assert_eq!(
            lines[0].edits, lines[1].edits,
            "paired rows emphasize the same run"
        );
        let (start, end) = lines[0].edits[0];
        assert_eq!(&lines[0].text[start as usize..end as usize], "x");
        assert_eq!(&lines[1].text[start as usize..end as usize], "y");
        assert!(
            (end as usize) <= lines[0].text.trim_end_matches(['\r', '\n']).len(),
            "ranges must exclude the row break"
        );

        let lines = compute_diff("x\n", "x\ny\n");
        assert!(
            lines.iter().all(|line| line.edits.is_empty()),
            "unpaired rows stay unemphasized: {lines:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tilde_paths_expand() {
        let (dir, _guard) = tempdir();
        let original_home = std::env::var("HOME").ok();
        let home = TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", home.path()) };
        std::fs::write(home.path().join("homefile.txt"), "x").unwrap();
        let abs = resolve_write("~/homefile.txt").unwrap();
        assert_eq!(abs, home.path().join("homefile.txt"));
        match original_home {
            Some(home_path) => unsafe { std::env::set_var("HOME", home_path) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        drop(dir);
    }

    #[test]
    fn resolve_read_classification() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".git").unwrap();
        std::fs::write(".git/config", "x").unwrap();
        assert!(resolve_read(".git/config").is_ok());
        assert!(resolve_read(".").is_ok());
        assert!(resolve_write("src/f.txt").is_ok());

        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(resolve_read(&rel).is_ok());
        assert!(resolve_write(&rel).is_ok());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }
}
