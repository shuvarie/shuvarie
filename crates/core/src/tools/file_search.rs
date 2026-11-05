use std::path::Path;

use fff_search::{
    ContentCacheBudget, FFFMode, FilePickerOptions, FuzzyQuery, FuzzySearchOptions, PaginationArgs,
    QueryParser, file_picker::FilePicker,
};
use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind};

use super::{arg_value, display_path, is_hidden_path};

const DEFAULT_MAX_RESULTS: usize = 20;

pub(crate) struct FileSearch {
    access: Access,
}

impl FileSearch {
    pub(crate) fn new(access: Access) -> Self {
        Self { access }
    }
}

impl Tool for FileSearch {
    const NAME: &'static str = "file_search";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fuzzy file search (fff-search FFF engine): ranks the paths indexed under `path` by how well they match `query`, best match first, and prints them one per line. The match is fuzzy and typo-tolerant, and prefers hits in the file name over hits in parent directories, plus files touched by recent commits -- use it to locate a file whose exact path you do not know, where `glob` needs the pattern already. The query may also carry FFF's constraints: '*.rs' / 'type:rust' narrow by extension/type, 'src/' by path segment, '!test' excludes matches, and several words are matched independently. Honours .gitignore inside git repositories and skips hidden files/directories (unless `path` names one explicitly). Paths are prefixed with `path` as given, so they can be passed straight to `read_file`. Results are capped (default 20)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Fuzzy pattern matched against file paths (e.g. 'filesearch', 'file srch', 'tools file'); may include constraints like '*.rs', 'type:rust', 'src/' or '!test'" },
                "path": { "type": "string", "description": "Directory to search, relative to the workspace root (defaults to the workspace root)" },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of paths (default 20)" }
            },
            "required": ["query"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let access = self.access.clone();
            let query = arg_value(&args, "query")?;
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_RESULTS as u64)
                .max(1) as usize;
            let abs = access.resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;
            if abs.is_file() {
                return Err(format!("file_search path must be a directory: {path}"));
            }

            tokio::task::spawn_blocking(move || search(&abs, &path, &query, max))
                .await
                .map_err(|e| format!("file_search: {e}"))
                .and_then(|inner| inner)
                .map(ToolOutput::text)
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// One fuzzy search, driven by a fresh fff index of the search root.
///
/// Like `grep`, every call builds its own picker: [`FilePicker::collect_files`]
/// is the synchronous scan (no background watcher, no frecency database) and the
/// picker is dropped with the call, so a file created or deleted between two
/// calls is seen by the second one. A cached index would answer from the state
/// of the first call.
fn search(root: &Path, display_root: &str, query: &str, max: usize) -> Result<String, String> {
    let parsed = QueryParser::default().parse(query);

    // fff treats fuzzy text shorter than two bytes as *empty*: it then ranks
    // every indexed file by recency instead of matching anything, so `q: "a"`
    // would answer with the most recently touched files rather than paths
    // containing an `a`. Constraints alone are still meaningful (`*.rs` is a
    // deliberate "list the Rust files" query), and a multi-word query always
    // goes through the matcher, so only the single-token case can be empty.
    if parsed.constraints.is_empty() {
        let fuzzy_len = match &parsed.fuzzy_query {
            FuzzyQuery::Text(text) => text.len(),
            FuzzyQuery::Parts(_) => usize::MAX,
            FuzzyQuery::Empty => 0,
        };
        if fuzzy_len < 2 {
            return Err(
                "query needs at least 2 characters of fuzzy text, or a constraint such as '*.rs' or 'type:rust'"
                    .to_string(),
            );
        }
    }

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        // Nothing here outlives the call, so a watcher would only burn threads.
        watch: false,
        // Ranking reads no file contents; `max_files: 0` keeps the cache from
        // mapping any of them on the off chance something asks.
        cache_budget: Some(ContentCacheBudget::with_max_files(0)),
        ..Default::default()
    })
    .map_err(|e| format!("file_search: {e}"))?;
    picker
        .collect_files()
        .map_err(|e| format!("file_search: {e}"))?;

    // fff paginates the ranked list; page through it until enough hits survive
    // the hidden filter, since the filter is applied after ranking and dropping
    // a page's worth would otherwise shorten the answer.
    let mut found: Vec<(i32, String)> = Vec::new();
    let mut offset = 0usize;
    let total_matched = loop {
        let result = picker.fuzzy_search(
            &parsed,
            None,
            FuzzySearchOptions {
                pagination: PaginationArgs { offset, limit: max },
                ..Default::default()
            },
        );
        let page = result.items.len();
        for (item, score) in result.items.iter().zip(&result.scores) {
            let rel = item.relative_path(&picker);
            if is_hidden_path(&rel) {
                continue;
            }
            found.push((score.total, display_path(&rel, display_root)));
        }
        offset += page;
        // Everything ranked was looked at, or the answer is already full.
        if page == 0 || found.len() >= max || offset >= result.total_matched {
            break result.total_matched;
        }
    };

    // fff ranks by score with a modification-time tiebreak, and the ranking of
    // equally-scored files depends on the order its parallel scan produced, so
    // two identical calls could disagree. Order ties by path: the same query
    // over unchanged files has to answer the same way.
    found.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    found.truncate(max);

    if found.is_empty() {
        return Ok(format!("no matches for '{query}' in {display_root}"));
    }

    let mut out = String::new();
    for (_, path) in &found {
        out.push_str(path);
        out.push('\n');
    }
    if found.len() < total_matched {
        out.push_str(&format!(
            "… (top {} of {total_matched} matches; refine the query to see more)\n",
            found.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    async fn find(args: Value) -> Result<String, String> {
        FileSearch::new(crate::test_util::access())
            .call(&mut new_ctx(), args)
            .await
            .map(|out| out.as_text().unwrap().to_string())
            .map_err(|err| err.to_string())
    }

    #[tokio::test]
    async fn file_search_locates_a_file_from_a_typo() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("crates/core/src/tools").unwrap();
        std::fs::write("crates/core/src/tools/file_search.rs", "").unwrap();
        std::fs::write("crates/core/src/main.rs", "").unwrap();
        std::fs::write("README.md", "").unwrap();
        let out = find(json!({ "query": "filesearch" })).await.unwrap();
        let first = out.lines().next().unwrap();
        assert_eq!(first, "crates/core/src/tools/file_search.rs", "{out}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_prefers_the_filename_over_a_parent_directory() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("render").unwrap();
        std::fs::write("render/chat.rs", "").unwrap();
        std::fs::create_dir_all("crates/render").unwrap();
        std::fs::write("crates/render/render.rs", "").unwrap();
        let out = find(json!({ "query": "render" })).await.unwrap();
        let first = out.lines().next().unwrap();
        assert_eq!(first, "crates/render/render.rs", "{out}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_honors_a_glob_constraint() {
        let (dir, _guard) = tempdir();
        std::fs::write("notes.rs", "").unwrap();
        std::fs::write("notes.txt", "").unwrap();
        let out = find(json!({ "query": "*.rs" })).await.unwrap();
        assert!(out.contains("notes.rs"), "{out}");
        assert!(!out.contains("notes.txt"), "{out}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_prefixes_hits_with_the_search_root() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("crates/core").unwrap();
        std::fs::write("crates/core/search.rs", "").unwrap();
        std::fs::write("root_search.rs", "").unwrap();
        let out = find(json!({ "query": "search", "path": "crates/core" }))
            .await
            .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, vec!["crates/core/search.rs"], "{out}");
        drop(dir);
    }

    /// fff indexes dot entries inside a git repository; the tool's own filter
    /// must not hand `.env` or `.shuvarie/` back to the model.
    #[tokio::test]
    async fn file_search_skips_hidden_inside_a_repository() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".env", "").unwrap();
        std::fs::create_dir(".shuvarie").unwrap();
        std::fs::write(".shuvarie/env-session.txt", "").unwrap();
        std::fs::write("env.rs", "").unwrap();
        let out = find(json!({ "query": "env" })).await.unwrap();
        assert!(out.contains("env.rs"), "{out}");
        assert!(!out.contains(".env"), "{out}");
        assert!(!out.contains("session"), "{out}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_caps_results_and_says_so() {
        let (dir, _guard) = tempdir();
        for i in 0..5 {
            std::fs::write(format!("match_{i}.txt"), "").unwrap();
        }
        let out = find(json!({ "query": "match", "max_results": 2 }))
            .await
            .unwrap();
        let hits: Vec<&str> = out
            .lines()
            .filter(|line| line.starts_with("match_"))
            .collect();
        assert_eq!(hits.len(), 2, "{out}");
        assert!(out.contains("top 2 of 5 matches"), "{out}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_answers_the_same_way_twice() {
        let (dir, _guard) = tempdir();
        for i in 0..5 {
            std::fs::write(format!("same_{i}.txt"), "").unwrap();
        }
        let first = find(json!({ "query": "same" })).await.unwrap();
        let second = find(json!({ "query": "same" })).await.unwrap();
        assert_eq!(first, second, "ranking is not stable");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_reports_no_matches() {
        let (dir, _guard) = tempdir();
        std::fs::write("notes.txt", "").unwrap();
        let out = find(json!({ "query": "zzzzzzzz" })).await.unwrap();
        assert!(out.contains("no matches"), "{out}");
        drop(dir);
    }

    /// fff would answer a single character with every file ranked by recency,
    /// which is not the question a fuzzy search asks.
    #[tokio::test]
    async fn file_search_rejects_a_one_character_query() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "").unwrap();
        let err = find(json!({ "query": "a" })).await.unwrap_err();
        assert!(err.contains("at least 2 characters"), "{err}");
        drop(dir);
    }

    #[tokio::test]
    async fn file_search_rejects_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "").unwrap();
        let err = find(json!({ "query": "a.txt", "path": "a.txt" }))
            .await
            .unwrap_err();
        assert!(err.contains("must be a directory"), "{err}");
        drop(dir);
    }
}
