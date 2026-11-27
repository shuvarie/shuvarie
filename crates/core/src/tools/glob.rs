use std::path::Path;

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind};
use crate::walk;

use super::{arg_value, display_path};

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
        "Fast file pattern matching tool that works with any codebase size. Matches file paths against a single glob: `*.rs` also reaches nested files, `src/**/*.ts` narrows to a subtree, and `?`, `[abc]` and `{a,b}` work as usual. Honours .gitignore inside git repositories and skips hidden files/directories (unless `path` names one explicitly). Returns file paths -- never directories -- prefixed with `path` as given, sorted, and capped at 100; use it when you already know the pattern, or the shape of a half-remembered path (`**/*search*`)."
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

/// One glob call: the shared walk ([`crate::walk`]), filtered by a compiled
/// glob and truncated to a sorted prefix.
///
/// The pattern is compiled with `globset`'s defaults, which is the behaviour
/// the tool documents: `literal_separator` is off, so a bare `*` crosses
/// directories and `*.rs` is the recursive "every Rust file" query. Nothing is
/// cached between calls, so a file created after the previous one is seen by
/// this call.
fn search(root: &Path, display_root: &str, pattern: &str) -> Result<String, String> {
    // A pattern that fails to compile has to be reported as the error it is:
    // silently matching nothing would answer "No files found" for a typo the
    // caller could have fixed.
    let matcher = globset::Glob::new(pattern)
        .map_err(|e| format!("bad glob pattern '{pattern}': {e}"))?
        .compile_matcher();

    // The walk answers in sorted order, so the kept set below is a stable
    // prefix of it: two identical queries over unchanged files agree.
    let found: Vec<String> = walk::files(root)
        .into_iter()
        .filter(|file| matcher.is_match(&file.rel))
        .map(|file| display_path(&file.rel, display_root))
        .collect();

    let total = found.len();
    if total == 0 {
        return Ok("No files found".to_string());
    }

    let mut out = found
        .into_iter()
        .take(GLOB_MAX_RESULTS)
        .collect::<Vec<_>>()
        .join("\n");
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

    /// The pattern is compiled with `literal_separator` off, so a bare `*`
    /// crosses directory boundaries: `*.rs` is the recursive "every Rust file"
    /// query, not just the top level.
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

    /// The walk yields files only, so a directory whose name spells a pattern
    /// is not a hit.
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
        // A `.gitignore` only applies inside a repository, so the ignore rule
        // has to sit next to a `.git` for the walk to apply it.
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
