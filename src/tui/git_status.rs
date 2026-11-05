//! The worktree reads behind the unstaged-files guard: what is not staged right
//! now, and the one write that gets it out of the way (`git add --all`).
//!
//! The read goes through `gix`, which is already how the sidebar resolves the
//! branch (`workspace.rs`) and the watch resolves `.git/HEAD` (`git_watch.rs`),
//! so the TUI asks one library about the repository it is standing in.
//!
//! Staging stays a `git` invocation: `gix` deliberately exposes no
//! worktree-to-index write. It can *read* the index (`open_index`, `index`,
//! `index_from_tree`) and it can check out *into* the worktree
//! (`gix-worktree-state`), but adding entries means hashing each worktree file
//! through the attribute/clean-filter pipeline, writing the blob, and building
//! the index entry — i.e. reimplementing `git add`, ignore rules, symlinks and
//! submodules included. That is not a trade worth making for one keystroke.

use std::path::Path;
use std::process::Command;

use gix::status::UntrackedFiles;
use gix::status::index_worktree::Item as WorktreeItem;
use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};

/// What happened to an unstaged path. The guard colours rows by this and the
/// dialog prints its one-letter mark, so it carries the meaning rather than a
/// porcelain code that would have to be parsed back out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnstagedKind {
    /// A tracked file's worktree copy differs from the index.
    Modified,
    /// A tracked file is gone from the worktree.
    Deleted,
    /// The kind of thing at the path changed (file ↔ symlink ↔ …).
    TypeChanged,
    /// The path is in a merge conflict.
    Conflicted,
    /// An `--intent-to-add` path: the index knows the name but holds none of
    /// its content, so it is still unstaged work (git's ` A`).
    Added,
    /// The path is not tracked at all.
    Untracked,
}

/// One worktree entry that differs from the index: a tracked path whose
/// worktree copy changed (or went missing), or an untracked path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnstagedFile {
    /// The path, relative to the repository root.
    pub path: String,
    pub kind: UnstagedKind,
}

impl UnstagedFile {
    /// Whether the entry is an untracked path.
    pub fn untracked(&self) -> bool {
        self.kind == UnstagedKind::Untracked
    }

    /// Whether the worktree copy is gone.
    pub fn deleted(&self) -> bool {
        self.kind == UnstagedKind::Deleted
    }

    /// Whether the path is new to the index (`--intent-to-add`).
    pub fn added(&self) -> bool {
        self.kind == UnstagedKind::Added
    }

    /// The row's status mark: git's one-letter code for the change, `??` for an
    /// untracked path.
    pub fn code(&self) -> &'static str {
        match self.kind {
            UnstagedKind::Modified => "M",
            UnstagedKind::Deleted => "D",
            UnstagedKind::TypeChanged => "T",
            UnstagedKind::Conflicted => "U",
            UnstagedKind::Added => "A",
            UnstagedKind::Untracked => "??",
        }
    }
}

/// The worktree's unstaged entries, one per path, in path order. Empty when
/// `path` is not inside a worktree (or the repository cannot be read), so the
/// guard has nothing to warn about.
///
/// Only the index↔worktree half of `git status` is wanted: changes already
/// staged (`HEAD`↔index) are not what the warning is about, so this uses
/// [`into_index_worktree_iter`](gix::status::Platform::into_index_worktree_iter)
/// and skips the tree comparison outright.
pub fn unstaged(path: &Path) -> Vec<UnstagedFile> {
    let Ok(repo) = gix::discover(path) else {
        return Vec::new();
    };
    let Ok(platform) = repo.status(gix::progress::Discard) else {
        return Vec::new();
    };
    let Ok(items) = platform
        // One row per file, not per collapsed directory (`git status` does the
        // same): the dialog lists the paths the user would stage.
        .untracked_files(UntrackedFiles::Files)
        .into_index_worktree_iter(Vec::<gix::bstr::BString>::new())
    else {
        return Vec::new();
    };
    let mut files: Vec<UnstagedFile> = items
        .filter_map(Result::ok)
        .filter_map(|item| classify(&item))
        .collect();
    // The iterator's order is an implementation detail (it is concurrent when
    // `gix`'s `parallel` feature is on); path order keeps the dialog stable
    // between two identical worktrees.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

/// `git add --all`: stage every unstaged change and untracked file, so the next
/// turn starts from a clean worktree. See the module docs for why this is the
/// one operation that still runs git itself.
pub fn stage_all(path: &Path) -> Result<(), String> {
    git(path, &["add", "--all"]).map(drop)
}

/// The unstaged entry one status item describes, or `None` when the item is not
/// an unstaged change: ignored and tracked directory entries, and index entries
/// that merely need their stat refreshed (`NeedsUpdate`) or that are only
/// `--intent-to-add` placeholders, are all not work the user has to stage.
fn classify(item: &WorktreeItem) -> Option<UnstagedFile> {
    match item {
        WorktreeItem::Modification {
            rela_path, status, ..
        } => {
            let kind = match status {
                EntryStatus::Conflict { .. } => UnstagedKind::Conflicted,
                EntryStatus::Change(Change::Removed) => UnstagedKind::Deleted,
                EntryStatus::Change(Change::Type { .. }) => UnstagedKind::TypeChanged,
                EntryStatus::Change(Change::Modification { .. }) => UnstagedKind::Modified,
                // A submodule whose `HEAD` or contents moved: modified, in the
                // same sense `git status` reports it.
                EntryStatus::Change(Change::SubmoduleModification(_)) => UnstagedKind::Modified,
                // The index holds the name but no content, so the file is
                // still unstaged work (git prints it as ` A`).
                EntryStatus::IntentToAdd => UnstagedKind::Added,
                // Only a stat refresh: the entry itself did not change.
                EntryStatus::NeedsUpdate(_) => return None,
            };
            Some(UnstagedFile {
                path: path_string(rela_path.as_ref()),
                kind,
            })
        }
        WorktreeItem::DirectoryContents { entry, .. } => {
            (entry.status == gix::dir::entry::Status::Untracked).then(|| UnstagedFile {
                path: path_string(entry.rela_path.as_ref()),
                kind: UnstagedKind::Untracked,
            })
        }
        // Rewrite tracking is off (the platform's default), so this is
        // unreachable; a tracked file renamed in the worktree would still be
        // untracked work at its new path, which is what an abort should show.
        WorktreeItem::Rewrite { dirwalk_entry, .. } => Some(UnstagedFile {
            path: path_string(dirwalk_entry.rela_path.as_ref()),
            kind: UnstagedKind::Untracked,
        }),
    }
}

/// Git paths are bytes; the dialog needs something to paint. Lossy decoding is
/// what `git` itself does when printing them.
fn path_string(path: &gix::bstr::BStr) -> String {
    String::from_utf8_lossy(path.as_ref()).into_owned()
}

/// Run `git -C path <args>` and hand back stdout; the failure is git's own
/// stderr (trimmed) so the footer can show why staging did not happen.
fn git(path: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        // Non-ASCII paths print literally rather than octal-escaped.
        .args(["-c", "core.quotepath=false"])
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_directory_that_is_not_a_worktree_has_nothing_unstaged() {
        let dir = temp_dir("plain");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(unstaged(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_names_each_kind_of_unstaged_work() {
        let dir = repo("kinds");
        std::fs::write(dir.join("modified.txt"), "one\n").unwrap();
        std::fs::write(dir.join("deleted.txt"), "one\n").unwrap();
        commit_all(&dir, "init");

        std::fs::write(dir.join("modified.txt"), "two\n").unwrap();
        std::fs::remove_file(dir.join("deleted.txt")).unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
        assert_eq!(
            unstaged(&dir),
            vec![
                UnstagedFile {
                    path: "deleted.txt".into(),
                    kind: UnstagedKind::Deleted,
                },
                UnstagedFile {
                    path: "modified.txt".into(),
                    kind: UnstagedKind::Modified,
                },
                UnstagedFile {
                    path: "untracked.txt".into(),
                    kind: UnstagedKind::Untracked,
                },
            ],
            "worktree and untracked changes, in path order"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn already_staged_work_is_not_unstaged() {
        let dir = repo("staged-only");
        std::fs::write(dir.join("staged.txt"), "one\n").unwrap();
        commit_all(&dir, "init");

        // Staged, so the worktree matches the index and nothing is left to warn
        // about — even though `HEAD` does differ.
        std::fs::write(dir.join("staged.txt"), "two\n").unwrap();
        git(&dir, &["add", "staged.txt"]).unwrap();
        assert_eq!(unstaged(&dir), Vec::new());

        // A new file that is only an `--intent-to-add` placeholder has no
        // content in the index yet, so it is still unstaged work (git prints it
        // as ` A`).
        std::fs::write(dir.join("intent.txt"), "new\n").unwrap();
        git(&dir, &["add", "--intent-to-add", "intent.txt"]).unwrap();
        assert_eq!(
            unstaged(&dir),
            vec![UnstagedFile {
                path: "intent.txt".into(),
                kind: UnstagedKind::Added,
            }]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_clears_the_unstaged_set() {
        let dir = repo("stage");
        std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        commit_all(&dir, "init");

        std::fs::write(dir.join("tracked.txt"), "two\n").unwrap();
        std::fs::write(dir.join("untracked.txt"), "new\n").unwrap();
        assert_eq!(unstaged(&dir).len(), 2);

        stage_all(&dir).unwrap();
        assert_eq!(
            unstaged(&dir),
            Vec::new(),
            "everything staged leaves nothing to warn about"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_marks_and_flags_read_the_kind() {
        let untracked = UnstagedFile {
            path: "a".into(),
            kind: UnstagedKind::Untracked,
        };
        assert!(untracked.untracked());
        assert!(!untracked.deleted());
        assert_eq!(untracked.code(), "??");

        let deleted = UnstagedFile {
            path: "b".into(),
            kind: UnstagedKind::Deleted,
        };
        assert!(!deleted.untracked());
        assert!(deleted.deleted());
        assert_eq!(deleted.code(), "D");

        let modified = UnstagedFile {
            path: "c".into(),
            kind: UnstagedKind::Modified,
        };
        assert_eq!(modified.code(), "M");

        let added = UnstagedFile {
            path: "d".into(),
            kind: UnstagedKind::Added,
        };
        assert!(added.added());
        assert!(!added.untracked(), "the index knows the name");
        assert_eq!(added.code(), "A");
    }

    /// A fresh, isolated repository with an identity so the tests can commit on
    /// machines that never configured one.
    fn repo(name: &str) -> PathBuf {
        let dir = temp_dir(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]).unwrap();
        git(&dir, &["config", "user.name", "shuvarie"]).unwrap();
        git(&dir, &["config", "user.email", "shuvarie@example.invalid"]).unwrap();
        dir
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("shuvarie-git-status-{name}-{}", std::process::id()))
    }

    fn commit_all(dir: &Path, message: &str) {
        git(dir, &["add", "--all"]).unwrap();
        git(dir, &["commit", "-qm", message]).unwrap();
    }
}
