use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use grep::regex::RegexMatcherBuilder;
use grep::searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::resolve_read;

use super::arg_value;

const DEFAULT_MAX_RESULTS: usize = 200;

pub(crate) struct Grep;

impl Tool for Grep {
    const NAME: &'static str = "grep";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fast ripgrep-based content search: matches a case-sensitive regular expression against every text file under `path`, respecting .gitignore and skipping hidden and binary files, printing `path:line: content`. Results are capped (default 200)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for (Rust regex syntax, case-sensitive)" },
                "path": { "type": "string", "description": "Directory or file to search, relative to the workspace root (defaults to the workspace root)" },
                "include": { "type": "string", "description": "Only search files matching this glob (e.g. '*.rs', 'src/**/*.ts'); a plain string like 'test' matches file names containing it" },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of matching lines (default 200)" }
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
            let pattern = arg_value(&args, "pattern")?;
            let path = args
                .get("path")
                .and_then(Value::as_str)
                .unwrap_or(".")
                .to_string();
            let include = args
                .get("include")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_MAX_RESULTS as u64)
                .max(1) as usize;
            let abs = resolve_read(&path)?;

            tokio::task::spawn_blocking(move || {
                search(&abs, &path, &pattern, include.as_deref(), max)
            })
            .await
            .map_err(|e| format!("grep: {e}"))
            .and_then(|inner| inner)
            .map(ToolOutput::text)
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

fn search(
    abs: &Path,
    display_root: &str,
    pattern: &str,
    include: Option<&str>,
    max: usize,
) -> Result<String, String> {
    let matcher = RegexMatcherBuilder::new()
        .build(pattern)
        .map_err(|e| format!("bad pattern: {e}"))?;
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(b'\x00'))
        .build();

    let hits = AtomicUsize::new(0);
    let mut results: Vec<(String, Vec<String>)> = Vec::new();

    if abs.is_file() {
        let mut lines = Vec::new();
        let mut sink = Collector {
            rel: display_root,
            out: &mut lines,
            hits: &hits,
            max,
        };
        let _ = searcher.search_path(&matcher, abs, &mut sink);
        if !lines.is_empty() {
            results.push((display_root.to_string(), lines));
        }
    } else {
        let mut walk_builder = ignore::WalkBuilder::new(abs);
        walk_builder
            .hidden(true)
            .git_ignore(true)
            .standard_filters(true)
            .require_git(false);
        if let Some(include) = include {
            let glob = include_to_glob(include);
            let mut overrides = ignore::overrides::OverrideBuilder::new(abs);
            overrides
                .add(&glob)
                .map_err(|e| format!("bad include glob '{glob}': {e}"))?;
            let overrides = overrides
                .build()
                .map_err(|e| format!("bad include glob '{glob}': {e}"))?;
            walk_builder.overrides(overrides);
        }
        for entry in walk_builder.build().flatten() {
            if hits.load(Ordering::Relaxed) >= max {
                break;
            }
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }
            let display = display_path(entry.path(), abs, display_root);
            let mut lines = Vec::new();
            let mut sink = Collector {
                rel: &display,
                out: &mut lines,
                hits: &hits,
                max,
            };
            let _ = searcher.search_path(&matcher, entry.path(), &mut sink);
            if !lines.is_empty() {
                results.push((display, lines));
            }
        }
    }

    results.sort();
    let matched = hits.load(Ordering::Relaxed);
    if matched == 0 {
        return Ok(format!("no matches for /{pattern}/ in {display_root}"));
    }
    let mut out: String = results.into_iter().flat_map(|(_, lines)| lines).collect();
    if matched >= max {
        out.push_str(&format!("… (results truncated at {max})\n"));
    }
    Ok(out)
}

fn display_path(entry: &Path, root: &Path, display_root: &str) -> String {
    let rel = entry.strip_prefix(root).unwrap_or(entry);
    if rel.as_os_str().is_empty() {
        display_root.to_string()
    } else if display_root == "." {
        rel.to_string_lossy().into_owned()
    } else {
        Path::new(display_root)
            .join(rel)
            .to_string_lossy()
            .into_owned()
    }
}

fn include_to_glob(include: &str) -> String {
    if include.contains('/') || include.contains(['*', '?', '[', '{']) {
        include.to_string()
    } else {
        format!("*{include}*")
    }
}

struct Collector<'a> {
    rel: &'a str,
    out: &'a mut Vec<String>,
    hits: &'a AtomicUsize,
    max: usize,
}

impl Sink for Collector<'_> {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        let mut line = String::from_utf8_lossy(mat.bytes()).into_owned();
        if line.ends_with('\n') {
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
        }
        let number = mat.line_number().unwrap_or(0);
        self.out.push(format!("{}:{}:{}\n", self.rel, number, line));
        let count = self.hits.fetch_add(1, Ordering::Relaxed) + 1;
        Ok(count < self.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    #[tokio::test]
    async fn grep_finds_with_substring_include() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep
            .call(&mut new_ctx(), json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("r.rs:1"), "{text}");
        assert!(!text.contains("r.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_caps_results() {
        let (dir, _guard) = tempdir();
        let content: String = (0..10).map(|i| format!("line {i} fn\n")).collect();
        std::fs::write("lines.rs", content).unwrap();
        let out = Grep
            .call(&mut new_ctx(), json!({ "pattern": "fn", "max_results": 3 }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("lines.rs:1"), "{text}");
        assert!(text.contains("lines.rs:3"), "{text}");
        assert!(!text.contains("lines.rs:4"), "{text}");
        assert!(text.contains("truncated"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_respects_gitignore_and_hidden() {
        let (dir, _guard) = tempdir();
        std::fs::write(".gitignore", "ignored.txt\n").unwrap();
        std::fs::write("ignored.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "needle\n").unwrap();
        let out = Grep
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_include_glob_matches_subdirs() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("src").unwrap();
        std::fs::write("src/a.rs", "needle\n").unwrap();
        std::fs::write("src/b.txt", "needle\n").unwrap();
        let out = Grep
            .call(
                &mut new_ctx(),
                json!({ "pattern": "needle", "include": "*.rs" }),
            )
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("src/a.rs:1"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_skips_binary_files() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", b"\x00\nneedle\n" as &[u8]).unwrap();
        std::fs::write("text.dat", "needle\n").unwrap();
        let out = Grep
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("text.dat:1"), "{text}");
        assert!(!text.contains("bin.dat"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_single_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "needle\nother\n").unwrap();
        std::fs::write("b.txt", "needle\n").unwrap();
        let out = Grep
            .call(
                &mut new_ctx(),
                json!({ "pattern": "needle", "path": "a.txt" }),
            )
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("a.txt:1"), "{text}");
        assert!(!text.contains("b.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_is_case_sensitive() {
        let (dir, _guard) = tempdir();
        std::fs::write("c.txt", "Needle\n").unwrap();
        let none = Grep
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        assert!(none.as_text().unwrap().contains("no matches"));
        drop(dir);
    }

    #[tokio::test]
    async fn grep_reports_bad_pattern() {
        let (dir, _guard) = tempdir();
        let err = Grep
            .call(&mut new_ctx(), json!({ "pattern": "([" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bad pattern"));
        drop(dir);
    }
}
