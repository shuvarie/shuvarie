use std::path::Path;

use fff_search::{
    ContentCacheBudget, FFFMode, FilePickerOptions, FuzzySearchOptions, PaginationArgs,
    file_picker::FilePicker,
};
use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind};

use super::{arg_value, display_path, is_hidden_path};

const GLOB_MAX_RESULTS: usize = 100;

pub(crate) struct Glob {
    access: Access,
}

impl Glob {
    pub(crate) fn new(access: Access) -> Self {
        Self { access }
    }
}

impl Tool for Glob {
    const NAME: &'static str = "glob";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fast file pattern matching tool that works with any codebase size. Matches file paths against a single glob (fff-search engine): `*.rs` also reaches nested files, `src/**/*.ts` narrows to a subtree, and `?`, `[abc]` and `{a,b}` work as usual. Honours .gitignore inside git repositories and skips hidden files/directories (unless `path` names one explicitly). Returns file paths -- never directories -- prefixed with `path` as given, sorted, and capped at 100; use it when you already know the pattern, where `file_search` handles a half-remembered name."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern matched against the paths relative to `path` (e.g. '*.rs', 'src/**/*.ts'); a bare '*' matches the whole path, so it crosses directories" },
                "path": { "type": "string", "description": "The directory to search in, relative to the workspace root (defaults to the workspace root)" }
            },
            "required": ["pattern"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let access = self.access.clone();
            let pattern = arg_value(&args, "pattern")?;
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            let abs = access.resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;
            if abs.is_file() {
                return Err(format!("glob path must be a directory: {path}"));
            }

            tokio::task::spawn_blocking(move || search(&abs, &path, &pattern))
                .await
                .map_err(|e| format!("glob: {e}"))
                .and_then(|inner| inner)
                .map(ToolOutput::text)
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// One glob call, driven by a fresh fff index of the search root.
///
/// Like `grep` and `file_search`, every call builds its own picker:
/// [`FilePicker::collect_files`] is the synchronous scan (no background
/// watcher, no frecency database) and the picker is dropped with the call, so a
/// file created or deleted between two calls is seen by the second one.
fn search(root: &Path, display_root: &str, pattern: &str) -> Result<String, String> {
    // fff compiles a `Constraint::Glob` with globset and lets a pattern that
    // fails to compile never match anything, which would answer "No files
    // found" for a typo the caller could have fixed. Compile it here first so
    // the bad pattern is reported as the error it is.
    globset::Glob::new(pattern).map_err(|e| format!("bad glob pattern '{pattern}': {e}"))?;

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        // Nothing here outlives the call, so a watcher would only burn threads.
        watch: false,
        // The glob filter reads paths, never contents: `max_files: 0` keeps the
        // cache from mapping any file on the off chance something asks.
        cache_budget: Some(ContentCacheBudget::with_max_files(0)),
        ..Default::default()
    })
    .map_err(|e| format!("glob: {e}"))?;
    picker.collect_files().map_err(|e| format!("glob: {e}"))?;

    // `limit: 0` asks fff for the whole match set rather than one page. The
    // ranking is frecency-based (nothing here ever records frecency, so it
    // mostly reflects modification times), while this tool's contract is
    // "sorted paths, first N"; truncating a frecency-ranked page would make the
    // cap drop arbitrary entries, and any count a script derives from the
    // answer would change between two identical queries.
    let result = picker.glob(
        pattern,
        FuzzySearchOptions {
            pagination: PaginationArgs {
                offset: 0,
                limit: 0,
            },
            ..Default::default()
        },
    );

    let mut found: Vec<String> = Vec::with_capacity(result.items.len());
    for item in &result.items {
        let rel = item.relative_path(&picker);
        // fff indexes dot entries inside a git repository; the tool's own filter
        // must not hand `.env` or `.shuvarie/` back to the model.
        if is_hidden_path(&rel) {
            continue;
        }
        found.push(display_path(&rel, display_root));
    }
    found.sort_unstable();

    let total = found.len();
    found.truncate(GLOB_MAX_RESULTS);
    if found.is_empty() {
        return Ok("No files found".to_string());
    }

    let mut out = found.join("\n");
    if total > GLOB_MAX_RESULTS {
        out.push_str(&format!(
            "\n\n(Results are truncated: showing first {GLOB_MAX_RESULTS} results. Consider using a more specific path or pattern.)"
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    async fn glob(args: Value) -> Result<String, String> {
        Glob::new(crate::test_util::access())
            .call(&mut new_ctx(), args)
            .await
            .map(|out| out.as_text().unwrap().to_string())
            .map_err(|err| err.to_string())
    }

    #[tokio::test]
    async fn glob_matches_recursively() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("src/sub").unwrap();
        std::fs::write("src/a.rs", "").unwrap();
        std::fs::write("src/sub/b.rs", "").unwrap();
        std::fs::write("src/c.txt", "").unwrap();
        let text = glob(json!({ "pattern": "**/*.rs" })).await.unwrap();
        assert!(text.contains("src/a.rs"), "{text}");
        assert!(text.contains("src/sub/b.rs"), "{text}");
        assert!(!text.contains("src/c.txt"), "{text}");
        drop(dir);
    }

    /// fff compiles globs with `literal_separator` off, so a bare `*` crosses
    /// directory boundaries: `*.rs` is the recursive "every Rust file" query,
    /// not just the top level.
    #[tokio::test]
    async fn a_bare_star_reaches_nested_files() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("crates/core/src").unwrap();
        std::fs::write("crates/core/src/lib.rs", "").unwrap();
        std::fs::write("top.rs", "").unwrap();
        let text = glob(json!({ "pattern": "*.rs" })).await.unwrap();
        assert!(text.contains("crates/core/src/lib.rs"), "{text}");
        assert!(text.contains("top.rs"), "{text}");
        drop(dir);
    }

    /// The picker indexes directories beside files; a glob query answers with
    /// files only, so a directory that spells a pattern is not a hit.
    #[tokio::test]
    async fn directories_are_not_results() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("looks.rs").unwrap();
        std::fs::write("real.rs", "").unwrap();
        let text = glob(json!({ "pattern": "*.rs" })).await.unwrap();
        assert_eq!(text.lines().next().unwrap(), "real.rs", "{text}");
        assert!(!text.contains("looks.rs"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn glob_honors_gitignore_and_hidden() {
        let (dir, _guard) = tempdir();
        // fff only reads .gitignore inside a repository, so the ignore rule has
        // to sit next to a `.git` for the walk to apply it.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".gitignore", "ignored.txt\n").unwrap();
        std::fs::write("ignored.txt", "").unwrap();
        std::fs::write("kept.txt", "").unwrap();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "").unwrap();
        let text = glob(json!({ "pattern": "*.txt" })).await.unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn glob_caps_and_reports_empty() {
        let (dir, _guard) = tempdir();
        for i in 0..150 {
            std::fs::write(format!("f{i}.txt"), "").unwrap();
        }
        let text = glob(json!({ "pattern": "*.txt" })).await.unwrap();
        assert!(text.contains("truncated"), "{text}");
        let none = glob(json!({ "pattern": "*.zzz" })).await.unwrap();
        assert!(none.contains("No files found"), "{none}");
        drop(dir);
    }

    /// A capped result has to be the same capped result every time, or a count
    /// taken from it cannot be checked against anything.
    #[tokio::test]
    async fn a_truncated_answer_is_the_same_answer_twice() {
        let (dir, _guard) = tempdir();
        for i in 0..150 {
            std::fs::write(format!("f{i}.txt"), "").unwrap();
        }
        let mut answers = Vec::new();
        for _ in 0..3 {
            answers.push(glob(json!({ "pattern": "*.txt" })).await.unwrap());
        }
        assert_eq!(answers[0], answers[1], "truncation is not stable");
        assert_eq!(answers[1], answers[2], "truncation is not stable");
        // And it is a prefix of the sorted whole, not an arbitrary slice.
        let first = answers[0].lines().next().unwrap();
        assert_eq!(
            first, "f0.txt",
            "the kept set should start at the sort order"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn glob_prefixes_hits_with_the_search_root() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("crates/core").unwrap();
        std::fs::write("crates/core/search.rs", "").unwrap();
        std::fs::write("root_search.rs", "").unwrap();
        let text = glob(json!({ "pattern": "*.rs", "path": "crates/core" }))
            .await
            .unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines, vec!["crates/core/search.rs"], "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn glob_rejects_a_bad_pattern() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "").unwrap();
        let err = glob(json!({ "pattern": "src/[unclosed" }))
            .await
            .unwrap_err();
        assert!(err.contains("bad glob pattern"), "{err}");
        drop(dir);
    }

    #[tokio::test]
    async fn glob_rejects_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "").unwrap();
        let err = glob(json!({ "pattern": "*.txt", "path": "a.txt" }))
            .await
            .unwrap_err();
        assert!(err.contains("must be a directory"), "{err}");
        drop(dir);
    }
}
