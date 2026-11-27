//! The composer's attachment fuzzy finder (`/attach [dir]`): one cached file
//! list per searched directory, plus the ranking query the popup sends on every
//! keystroke.
//!
//! The walk is what makes the popup live: listing a tree is the expensive part,
//! so it happens once when the finder opens (or when it is pointed at another
//! directory) and each keystroke then only re-ranks the same in-memory snapshot.
//! An empty query lists the snapshot alphabetically, which is a useful default
//! listing rather than an empty popup.

use std::path::{Path, PathBuf};

use frizbee::{Config, Matcher};

use super::normalize_path;
use crate::walk;

/// How many paths one ranking reply carries; the popup scrolls through them.
pub const MAX_CANDIDATES: usize = 200;

/// The directory an `/attach` request searches: the composer's argument
/// (`None`/empty = the workspace root), resolved like every other attachment
/// path (workspace-relative, absolute, or `~`-prefixed).
pub fn resolve_dir(workspace_root: &Path, dir: Option<&str>) -> PathBuf {
    match dir.map(str::trim).filter(|dir| !dir.is_empty()) {
        Some(dir) => normalize_path(workspace_root, dir),
        None => workspace_root.to_path_buf(),
    }
}

/// A cached file list over one directory, alive for as long as the finder stays
/// open on it. Rebuilt by [`AttachmentIndex::build`] on the blocking pool (a
/// scan of a large tree is not an async task's work).
pub struct AttachmentIndex {
    /// The searched directory, already resolved.
    base: PathBuf,
    /// Every file under it, `/`-joined and relative to `base`, sorted.
    files: Vec<String>,
}

impl AttachmentIndex {
    /// Scan `base` into a fresh index. The walk is the same one `glob`, `grep`
    /// and `lsp analyze` use ([`crate::walk`]), so the finder offers exactly the
    /// files those tools would search: gitignore rules apply, dot entries do
    /// not. Nothing is watched and no content is read — the popup ranks paths
    /// only and outlives nothing.
    pub fn build(base: &Path) -> Result<Self, String> {
        if !base.is_dir() {
            return Err(format!("not a directory: {}", base.display()));
        }
        Ok(Self {
            base: base.to_path_buf(),
            files: walk::files(base)
                .into_iter()
                .map(|found| found.rel)
                .collect(),
        })
    }

    /// The directory this index was built for; a request naming another one
    /// needs a rebuild.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Rank the indexed files against `query`, best match first. Paths come back
    /// in attachment form — relative to the workspace root when the searched
    /// directory sits inside it, absolute otherwise — so the caller can hand them
    /// straight back as an attachment path.
    pub fn candidates(&self, query: &str, workspace_root: &Path, limit: usize) -> Vec<String> {
        // An index built for a directory inside the workspace (its root
        // included) reports workspace-relative paths; one pointing elsewhere
        // reports absolute ones. Both are valid attachment paths.
        let display_root = match self.base.strip_prefix(workspace_root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => self.base.to_string_lossy().into_owned(),
        };
        // The query language is frizbee's: whitespace-separated atoms, every one
        // of which must match, with `'sub`, `^pre`, `suf$`, `^exact$` and `!neg`
        // available for the cases a plain fuzzy needle gets wrong. Defaults do
        // the rest — smart casing (a lowercase query ignores case), no typo
        // tolerance, and `ScoreThenIndexAsc`, which keeps equal scores in the
        // alphabetical order the snapshot is already in, so the same query over
        // unchanged files answers the same way.
        let mut matcher = Matcher::from_query(query, &Config::default());
        let mut matches = matcher.match_list(&self.files);
        matches.truncate(limit);
        matches
            .into_iter()
            .map(|hit| join(&display_root, &self.files[hit.index as usize]))
            .collect()
    }
}

/// Join a search root (as the caller sees it) with an index-relative path.
fn join(display_root: &str, rel: &str) -> String {
    if display_root.is_empty() {
        rel.to_string()
    } else {
        format!("{}/{}", display_root.trim_end_matches('/'), rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(dir: &Path) -> AttachmentIndex {
        AttachmentIndex::build(dir).expect("the fixture directory indexes")
    }

    #[test]
    fn resolves_the_argument_against_the_workspace_root() {
        let root = Path::new("/work/project");
        assert_eq!(resolve_dir(root, None), root);
        assert_eq!(resolve_dir(root, Some("  ")), root);
        assert_eq!(resolve_dir(root, Some("docs")), root.join("docs"));
        assert_eq!(
            resolve_dir(root, Some("/elsewhere")),
            Path::new("/elsewhere")
        );
    }

    #[test]
    fn ranks_hits_by_name_and_skips_hidden_entries() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("assets/plan.pdf"), "").unwrap();
        std::fs::write(root.join(".env"), "").unwrap();
        std::fs::write(root.join(".git/config"), "").unwrap();
        std::fs::write(root.join("notes.md"), "").unwrap();
        let index = index(root);
        let found = index.candidates("plan", root, MAX_CANDIDATES);
        assert_eq!(found, vec!["assets/plan.pdf"], "{found:?}");
        // A dot entry is not in the snapshot at all, whatever the query.
        for query in ["env", "config", ""] {
            let found = index.candidates(query, root, MAX_CANDIDATES);
            assert!(
                !found
                    .iter()
                    .any(|path| path.split('/').any(|segment| segment.starts_with('.'))),
                "hidden entry leaked for {query:?}: {found:?}"
            );
        }
    }

    #[test]
    fn prefixes_hits_with_the_search_root_relative_to_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs/deep")).unwrap();
        std::fs::write(root.join("docs/deep/report.pdf"), "").unwrap();
        std::fs::write(root.join("top.pdf"), "").unwrap();
        // The finder pointed at `docs`: hits come back workspace-relative.
        let index = index(&root.join("docs"));
        let found = index.candidates("report", root, MAX_CANDIDATES);
        assert_eq!(found, vec!["docs/deep/report.pdf"], "{found:?}");
        // A directory outside the workspace yields absolute attachment paths.
        let outside = index.candidates("report", Path::new("/nonexistent-root"), MAX_CANDIDATES);
        let expected = root.join("docs/deep/report.pdf");
        assert_eq!(outside, vec![expected.to_string_lossy().to_string()]);
    }

    #[test]
    fn an_empty_query_still_lists_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for name in ["a.txt", "b.txt", "c.txt"] {
            std::fs::write(root.join(name), "").unwrap();
        }
        let index = index(root);
        let found = index.candidates("", root, 2);
        assert_eq!(found, vec!["a.txt", "b.txt"], "the limit is honoured");
    }

    /// The ranking needs no minimum query length: the first keystroke already
    /// narrows the list, which is what makes the popup feel live.
    #[test]
    fn a_single_character_query_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("zeta.md"), "").unwrap();
        std::fs::write(root.join("alpha.md"), "").unwrap();
        std::fs::create_dir_all(root.join("Zeta")).unwrap();
        std::fs::write(root.join("Zeta/plan.md"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("z", root, MAX_CANDIDATES),
            vec!["zeta.md", "Zeta/plan.md"],
            "one character is a real query; the hit that also matches the case \
             earns the case bonus and outranks the capitalised one"
        );
        // Smart casing: a lowercase needle ignores case, so the capitalised path
        // is a hit too, and an uppercase one is taken at its word.
        assert_eq!(
            index.candidates("Z", root, MAX_CANDIDATES),
            vec!["Zeta/plan.md"],
            "a capital needle is case-sensitive"
        );
        assert!(
            index.candidates("q", root, MAX_CANDIDATES).is_empty(),
            "a miss is a miss"
        );
    }

    /// Every atom narrows the list: a path has to carry all of them.
    #[test]
    fn a_multi_atom_query_applies_every_atom() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("ab.md"), "").unwrap();
        std::fs::write(root.join("a.md"), "").unwrap();
        std::fs::write(root.join("b.md"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("a b", root, MAX_CANDIDATES),
            vec!["ab.md"],
            "a path must carry every atom"
        );
        // The atom syntax reaches the matcher intact: a `!` atom excludes.
        assert_eq!(
            index.candidates("md !^a", root, MAX_CANDIDATES),
            vec!["b.md"],
            "a negated atom drops the paths it matches"
        );
    }

    /// A query spelled with a gap is still a fuzzy hit: the matcher aligns the
    /// needle in order, best alignment first.
    #[test]
    fn a_gapped_query_takes_the_fuzzy_engine() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("plan.pdf"), "").unwrap();
        std::fs::write(root.join("plain.txt"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("pln", root, MAX_CANDIDATES),
            vec!["plan.pdf", "plain.txt"],
            "a fuzzy hit, best match first"
        );
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope");
        match AttachmentIndex::build(&missing) {
            Ok(_) => panic!("a missing directory must not index"),
            Err(error) => assert!(error.contains("not a directory"), "{error}"),
        }
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "").unwrap();
        match AttachmentIndex::build(&file) {
            Ok(_) => panic!("a file must not index"),
            Err(error) => assert!(error.contains("not a directory"), "{error}"),
        }
    }
}
