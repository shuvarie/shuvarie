//! The one directory walk behind every search surface: `glob`, `grep`,
//! `lsp analyze`, and the `/attach` finder.
//!
//! `ignore`'s walker is the definition of "which files exist" for the whole
//! core. Doing it here once means the four call sites cannot disagree about
//! what a repository holds — `glob` cannot offer a path `grep` then refuses to
//! search — and the same rules every tool documents apply everywhere:
//! `.gitignore` (plus `.git/info/exclude` and the user's global ignore) is
//! honoured inside a repository, hidden entries are skipped, links are not
//! followed.
//!
//! Every caller but the finder wants the answer ordered, so [`files`] sorts by
//! the relative path; the walk itself is parallel and unordered.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use ignore::{WalkBuilder, WalkState};

/// One file the walk found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// The path relative to the walk root, `/`-separated on every platform so
    /// callers can split it on `/` without caring which one they run on.
    pub(crate) rel: String,
    /// The path to open: `root` joined with `rel`.
    pub(crate) abs: PathBuf,
}

/// Every file under `root`, gitignore-filtered, hidden entries skipped, sorted
/// by [`Found::rel`].
///
/// `root` itself is never filtered, so pointing a tool at `.config` searches
/// it — that is what "unless `path` names one explicitly" promises. Entries
/// that cannot be read are skipped rather than failing the walk: one
/// unreadable directory should not blind a search of the other thousand.
pub(crate) fn files(root: &Path) -> Vec<Found> {
    let found: Mutex<Vec<Found>> = Mutex::new(Vec::new());
    WalkBuilder::new(root).build_parallel().run(|| {
        let found = &found;
        Box::new(move |entry| {
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            let path = entry.path();
            // `is_file` stats, so a link to a file counts as that file. Links to
            // directories are never descended into (the walker does not follow
            // them), which keeps a cyclic tree from looping.
            if !path.is_file() {
                return WalkState::Continue;
            }
            let Some(rel) = relative(root, path) else {
                return WalkState::Continue;
            };
            if rel.is_empty() {
                return WalkState::Continue;
            }
            found
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(Found {
                    rel,
                    abs: path.to_path_buf(),
                });
            WalkState::Continue
        })
    });
    let mut found = found
        .into_inner()
        .unwrap_or_else(|error| error.into_inner());
    found.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    found
}

/// `path` under `root`, joined with `/`; `None` when it is not under `root`
/// (the walker only ever yields such paths) or holds a component a relative
/// path cannot spell.
fn relative(root: &Path, path: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let mut rel = String::new();
    for component in rest.components() {
        let Component::Normal(part) = component else {
            return None;
        };
        if !rel.is_empty() {
            rel.push('/');
        }
        rel.push_str(&part.to_string_lossy());
    }
    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::tempdir;

    fn rels(root: &Path) -> Vec<String> {
        files(root).into_iter().map(|found| found.rel).collect()
    }

    #[test]
    fn walks_files_sorted_and_skips_directories() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("src/deep").unwrap();
        std::fs::write("src/deep/c.rs", "").unwrap();
        std::fs::write("z.txt", "").unwrap();
        std::fs::write("a.txt", "").unwrap();
        assert_eq!(
            rels(dir.path()),
            vec!["a.txt", "src/deep/c.rs", "z.txt"],
            "files only, `/`-joined, sorted"
        );
    }

    /// Hidden entries go even inside a repository, where a bare walk would
    /// happily index `.env` and `.git/`; and a gitignore rule only applies next
    /// to a `.git`, which is the `ignore` crate's own `require_git` default.
    #[test]
    fn skips_hidden_and_honours_gitignore_inside_a_repository() {
        let (dir, _guard) = tempdir();
        std::fs::write("kept.txt", "").unwrap();
        std::fs::write(".env", "").unwrap();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "").unwrap();
        // Outside a repository nothing marks this file as a gitignore.
        std::fs::write(".gitignore", "kept.txt\n").unwrap();
        assert_eq!(rels(dir.path()), vec!["kept.txt"]);

        std::fs::create_dir(".git").unwrap();
        assert!(
            rels(dir.path()).is_empty(),
            "inside a repository the rule applies"
        );
    }

    /// A hidden root is still walked when it is asked for by name.
    #[test]
    fn a_hidden_root_is_still_walked() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir(".config").unwrap();
        std::fs::write(".config/keys.txt", "").unwrap();
        std::fs::write("kept.txt", "").unwrap();
        assert_eq!(rels(&dir.path().join(".config")), vec!["keys.txt"]);
    }
}
