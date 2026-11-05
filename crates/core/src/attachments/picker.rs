//! The composer's attachment fuzzy finder (`/attach [dir]`): one cached FFF
//! index per searched directory, plus the ranking query the popup sends on
//! every keystroke.
//!
//! The index is what makes the popup live: walking a tree is the expensive
//! part, so it happens once when the finder opens (or when it is pointed at
//! another directory) and each keystroke then re-ranks the same in-memory
//! snapshot. FFF ranks match quality first with a recency tiebreak, so an empty
//! query is a useful default listing rather than an empty popup.

use std::path::{Path, PathBuf};

use fff_search::{
    ContentCacheBudget, FFFMode, FFFQuery, FilePickerOptions, FuzzyQuery, FuzzySearchOptions,
    PaginationArgs, QueryParser, file_picker::FilePicker,
};

use super::normalize_path;

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

/// A cached FFF index over one directory, alive for as long as the finder
/// stays open on it. Rebuilt by [`AttachmentIndex::build`] on the blocking
/// pool (a scan of a large tree is not an async task's work).
pub struct AttachmentIndex {
    /// The indexed directory, already resolved.
    base: PathBuf,
    picker: FilePicker,
}

impl AttachmentIndex {
    /// Scan `base` into a fresh index. No watcher, no content cache: the index
    /// answers path rankings only and outlives no longer than the popup.
    pub fn build(base: &Path) -> Result<Self, String> {
        if !base.is_dir() {
            return Err(format!("not a directory: {}", base.display()));
        }
        let mut picker = FilePicker::new(FilePickerOptions {
            base_path: base.to_string_lossy().into_owned(),
            mode: FFFMode::Ai,
            // Nothing outlives the popup, so a watcher thread would only idle.
            watch: false,
            // Ranking reads no file contents; `max_files: 0` keeps the cache
            // from mapping any of them.
            cache_budget: Some(ContentCacheBudget::with_max_files(0)),
            ..Default::default()
        })
        .map_err(|error| format!("cannot index {}: {error}", base.display()))?;
        picker
            .collect_files()
            .map_err(|error| format!("cannot index {}: {error}", base.display()))?;
        Ok(Self {
            base: base.to_path_buf(),
            picker,
        })
    }

    /// The directory this index was built for; a request naming another one
    /// needs a rebuild.
    pub fn base(&self) -> &Path {
        &self.base
    }

    /// Rank the indexed files against `query`, best match first. Paths come
    /// back in attachment form — relative to the workspace root when the
    /// searched directory sits inside it, absolute otherwise — so the caller
    /// can hand them straight back as an attachment path.
    pub fn candidates(&self, query: &str, workspace_root: &Path, limit: usize) -> Vec<String> {
        // An index built for a directory inside the workspace (its root
        // included) reports workspace-relative paths; one pointing elsewhere
        // reports absolute ones. Both are valid attachment paths.
        let display_root = match self.base.strip_prefix(workspace_root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => self.base.to_string_lossy().into_owned(),
        };
        let parsed = QueryParser::default().parse(query);
        // FFF's matcher drops fuzzy parts shorter than two bytes, so a
        // single-character query would answer with the whole index ranked by
        // recency instead of narrowing it. Filter those queries here, so the
        // search starts at the first keystroke.
        if let Some(needle) = below_fff_threshold(&parsed) {
            return self.short_query_candidates(&needle, &display_root, limit);
        }
        // FFF paginates the ranked list; page through it until enough hits
        // survive the hidden filter, since that filter runs after ranking and
        // dropping a page's worth would otherwise shorten the answer.
        let mut found: Vec<(i32, String)> = Vec::new();
        let mut offset = 0usize;
        loop {
            let result = self.picker.fuzzy_search(
                &parsed,
                None,
                FuzzySearchOptions {
                    pagination: PaginationArgs { offset, limit },
                    ..Default::default()
                },
            );
            let page = result.items.len();
            for (item, score) in result.items.iter().zip(&result.scores) {
                let rel = item.relative_path(&self.picker);
                if is_hidden(&rel) {
                    continue;
                }
                found.push((score.total, join(&display_root, &rel)));
            }
            offset += page;
            // Everything ranked was looked at, or the answer is already full.
            if page == 0 || found.len() >= limit || offset >= result.total_matched {
                break;
            }
        }
        // FFF ranks by score with a modification-time tiebreak, and the ranking
        // of equally-scored files depends on the order its parallel scan
        // produced — so order ties by path: the same query over unchanged files
        // has to answer the same way.
        found.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        found.truncate(limit);
        found.into_iter().map(|(_, path)| path).collect()
    }

    /// Ranking for a query FFF will not fuzzy-match (see
    /// [`below_fff_threshold`]): a plain case-insensitive substring scan over
    /// the index, narrowed by every needle. File-name hits rank above paths
    /// that only match through a parent directory, each group alphabetical —
    /// the same preference FFF scores, without the two-byte floor.
    fn short_query_candidates(
        &self,
        needles: &[String],
        display_root: &str,
        limit: usize,
    ) -> Vec<String> {
        let mut named: Vec<String> = Vec::new();
        let mut elsewhere: Vec<String> = Vec::new();
        for file in self.picker.get_files() {
            if file.is_deleted() {
                continue;
            }
            let rel = file.relative_path(&self.picker);
            if is_hidden(&rel) {
                continue;
            }
            let lower = rel.to_ascii_lowercase();
            if !needles.iter().all(|needle| lower.contains(needle.as_str())) {
                continue;
            }
            let name_start = lower.rfind('/').map_or(0, |slash| slash + 1);
            let path = join(display_root, &rel);
            if needles
                .iter()
                .all(|needle| lower[name_start..].contains(needle.as_str()))
            {
                named.push(path);
            } else {
                elsewhere.push(path);
            }
        }
        named.sort_unstable();
        elsewhere.sort_unstable();
        named.extend(elsewhere);
        named.truncate(limit);
        named
    }
}

/// The query's needles, lowercased, when FFF cannot match them: its matcher
/// filters fuzzy parts below two bytes, so a one-character token (or several of
/// them) would be dropped and the answer would degrade to a recency listing.
/// `None` when the query is empty (an unfiltered listing is exactly right
/// there), when some part is long enough for FFF, or when constraints such as
/// `*.rs` narrow the index first — those are left to FFF.
fn below_fff_threshold(parsed: &FFFQuery<'_>) -> Option<Vec<String>> {
    if !parsed.constraints.is_empty() {
        return None;
    }
    let parts: Vec<&str> = match &parsed.fuzzy_query {
        FuzzyQuery::Text(text) => vec![*text],
        FuzzyQuery::Parts(parts) => parts.clone(),
        FuzzyQuery::Empty => return None,
    };
    if parts.is_empty() || parts.iter().any(|part| part.len() >= 2) {
        return None;
    }
    Some(parts.iter().map(|part| part.to_lowercase()).collect())
}

/// Join a search root (as the caller sees it) with an index-relative path.
fn join(display_root: &str, rel: &str) -> String {
    if display_root.is_empty() {
        rel.to_string()
    } else {
        format!("{}/{}", display_root.trim_end_matches('/'), rel)
    }
}

/// Whether a path has a dot-led segment. The finder offers attachments, so the
/// dot-directories (`.git`, `.shuvarie`) and dotfiles that FFF indexes inside a
/// repository stay out of the list.
fn is_hidden(rel: &str) -> bool {
    rel.split('/').any(|segment| segment.starts_with('.'))
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
        // A dotfile never surfaces, whatever the query.
        for query in ["env", "config", ""] {
            let found = index.candidates(query, root, MAX_CANDIDATES);
            assert!(
                !found.iter().any(|path| is_hidden(path)),
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
        assert_eq!(found.len(), 2, "the limit is honoured: {found:?}");
    }

    /// FFF's matcher needs two bytes of fuzzy text; a single character must
    /// still narrow the list (the popup starts filtering at the first keystroke).
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
            "one character is a real query: the file-name hit first, then the \
             hit that only matches through its parent directory"
        );
        // Case is ignored: the fallback lowercases its needles, so a capital
        // still matches the lower-case path.
        assert_eq!(
            index.candidates("Z", root, MAX_CANDIDATES),
            vec!["zeta.md", "Zeta/plan.md"],
            "a capital needle matches case-insensitively"
        );
        assert!(
            index.candidates("q", root, MAX_CANDIDATES).is_empty(),
            "a miss is a miss"
        );
    }

    #[test]
    fn a_short_query_prefers_file_names_over_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("cat-src")).unwrap();
        std::fs::write(root.join("cat-src/main.rs"), "").unwrap();
        std::fs::write(root.join("cat.txt"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("c", root, MAX_CANDIDATES),
            vec!["cat.txt", "cat-src/main.rs"],
            "the name hit ranks first, both groups alphabetical"
        );
    }

    /// Every needle narrows the list, even when they are all one character.
    #[test]
    fn short_multi_part_queries_are_all_applied() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("ab.md"), "").unwrap();
        std::fs::write(root.join("a.md"), "").unwrap();
        std::fs::write(root.join("b.md"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("a b", root, MAX_CANDIDATES),
            vec!["ab.md"],
            "a path must carry every token"
        );
    }

    /// Two characters or more go through FFF's ranking as before; this pins that
    /// the fallback did not take the whole search over.
    #[test]
    fn longer_queries_keep_using_the_fuzzy_engine() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("plan.pdf"), "").unwrap();
        std::fs::write(root.join("plain.txt"), "").unwrap();
        let index = index(root);
        assert_eq!(
            index.candidates("pln", root, MAX_CANDIDATES),
            vec!["plan.pdf", "plain.txt"],
            "a typo-tolerant fuzzy hit, best match first"
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
