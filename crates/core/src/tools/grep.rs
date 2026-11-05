use std::path::Path;

use fff_search::{
    Constraint, ContentCacheBudget, FFFMode, FFFQuery, FuzzyQuery, GitRecencyConfig,
    file_picker::{FilePicker, FilePickerOptions},
    grep::{Casing, GrepMode, GrepSearchOptions},
};
use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind};

use super::arg_value;

const DEFAULT_MAX_RESULTS: usize = 200;

pub(crate) struct Grep {
    access: Access,
}

impl Grep {
    pub(crate) fn new(access: Access) -> Self {
        Self { access }
    }
}

impl Tool for Grep {
    const NAME: &'static str = "grep";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fast content search (fff-search grep engine): matches a case-sensitive regular expression against text files under `path`, honouring .gitignore inside git repositories and skipping binary files by extension and hidden files/directories (unless `path` names one explicitly). Prints `path:line: content` sorted by path then line, with lines truncated past 512 bytes; results are capped (default 200)."
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
            let access = self.access.clone();
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
            let abs = access.resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;

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

/// One grep call, driven by a fresh fff index of the search root.
///
/// fff greps its index rather than walking the tree itself, so every call
/// builds a picker: [`FilePicker::collect_files`] is the synchronous scan (no
/// background watcher, no frecency database) and the picker is dropped with the
/// call. That keeps results current for a tool that edits files between calls;
/// a cached index would serve stale matches for anything written after the scan.
fn search(
    abs: &Path,
    display_root: &str,
    pattern: &str,
    include: Option<&str>,
    max: usize,
) -> Result<String, String> {
    // fff indexes directories, so a single-file `path` searches its parent and
    // is narrowed to that exact file afterwards (see `only_file` below).
    let (root, only_file) = if abs.is_file() {
        let parent = abs
            .parent()
            .ok_or_else(|| format!("grep: {display_root} has no parent directory"))?;
        (parent.to_path_buf(), Some(abs.to_path_buf()))
    } else {
        (abs.to_path_buf(), None)
    };

    let include_glob = include.map(include_to_glob);
    let file_name = only_file
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned);
    // fff gets the same glob as a prefilter constraint, but it is re-checked
    // per hit below. `grep_search` retries a constrained query that found
    // nothing as literal text with the constraints dropped -- sensible for an
    // interactive picker, wrong here: `include: "*.kdl"` over a repo with no
    // `.kdl` match would otherwise report hits from every other file.
    let include_matcher = match include_glob.as_deref() {
        Some(glob) => Some(
            globset::Glob::new(glob)
                .map_err(|e| format!("bad include glob '{glob}': {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };

    let mut constraints: Vec<Constraint<'_>> = Vec::new();
    if let Some(glob) = include_glob.as_deref() {
        constraints.push(Constraint::Glob(glob));
    }
    if let Some(name) = file_name.as_deref() {
        constraints.push(Constraint::FilePath(name));
    }

    // fff's regex mode silently degrades an unparsable pattern to a literal
    // search, which would quietly change what the caller asked for. Validate
    // with the same `regex::bytes` configuration fff builds (multi-line
    // anchors, Unicode disabled) before paying for the index.
    regex::bytes::RegexBuilder::new(pattern)
        .multi_line(true)
        .unicode(false)
        .build()
        .map_err(|e| format!("bad pattern: {e}"))?;

    // fff reads the pattern out of its parsed query, and that parser strips a
    // leading `\` when it precedes `*`, `/` or `!` (its "this is a literal
    // constraint token" rule). `\*ptr` would reach the matcher as `*ptr` and
    // fail to compile, so an empty group keeps the text opaque to that rule --
    // `(?:)\*ptr` matches exactly what `\*ptr` does.
    let needle = match pattern.as_bytes() {
        [b'\\', b'*' | b'/' | b'!', ..] => format!("(?:){pattern}"),
        _ => pattern.to_string(),
    };

    let options = GrepSearchOptions {
        // The tool's contract is a case-sensitive regex, which is fff's regex
        // mode with `Casing::Sensitive` (its default is smart-case).
        mode: GrepMode::Regex,
        casing: Some(Casing::Sensitive),
        smart_case: false,
        page_limit: max,
        max_matches_per_file: max,
        // fff refuses files past `max_file_size` outright rather than streaming
        // them, and its default cap is 10 MiB, so a larger file would answer
        // "no matches" instead of being searched. Lift the cap: fff mmaps
        // anything past 1 MiB, which keeps the cost virtual, not resident.
        max_file_size: u64::MAX,
        ..Default::default()
    };

    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: root.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        // Nothing here outlives the call, so a watcher would only burn threads.
        watch: false,
        // The content cache carries its own per-file gate, which would keep the
        // 10 MiB limit alive for the actual read. `max_files: 0` also stops the
        // cache from mapping content a one-shot picker would never reuse.
        cache_budget: Some(ContentCacheBudget {
            max_file_size: u64::MAX,
            ..ContentCacheBudget::with_max_files(0)
        }),
        // Recency ranking is irrelevant to a content search; skip the commit
        // walk (`git status` is still read, as the walker needs it).
        git_recency: GitRecencyConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|e| format!("grep: {e}"))?;
    picker.collect_files().map_err(|e| format!("grep: {e}"))?;

    let query = FFFQuery {
        raw_query: &needle,
        constraints,
        fuzzy_query: FuzzyQuery::Text(&needle),
        location: None,
    };

    // fff greps its index in parallel chunks, so its match order varies between
    // runs. Sort by path then line to give a stable answer that can be diffed.
    let mut hits: Vec<(String, u64, String)> = Vec::new();
    let mut file_offset = 0usize;
    let mut truncated = false;
    loop {
        let page = GrepSearchOptions {
            file_offset,
            ..options.clone()
        };
        let result = picker.grep(&query, &page);

        // fff degrades an unparsable regex to a literal search; the tool reports
        // it as an error instead, because a silently literal search quietly
        // changes what the caller asked for.
        if let Some(err) = &result.regex_fallback_error {
            return Err(format!("bad pattern: {err}"));
        }

        // `literal_fallback` means the constrained scan came back empty across
        // the whole tree and fff retried without the constraints. Nothing it
        // found can survive the filters below, so stop rather than page through
        // the retry's results.
        if result.literal_fallback {
            break;
        }

        for hit in &result.matches {
            let Some(file) = result.files.get(hit.file_index) else {
                continue;
            };
            let display = match &only_file {
                Some(target) => {
                    if file.absolute_path(&picker, picker.base_path()) != *target {
                        continue;
                    }
                    display_root.to_string()
                }
                None => {
                    let rel = file.relative_path(&picker);
                    if is_hidden(&rel)
                        || include_matcher
                            .as_ref()
                            .is_some_and(|matcher| !matcher.is_match(&rel))
                    {
                        continue;
                    }
                    display_path(&rel, display_root)
                }
            };
            hits.push((display, hit.line_number, hit.line_content.clone()));
        }

        if hits.len() >= max {
            truncated = true;
            break;
        }
        // Everything searched, or the cursor stopped advancing.
        match result.next_file_offset {
            0 => break,
            next if next <= file_offset => break,
            next => file_offset = next,
        }
    }
    hits.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    if hits.is_empty() {
        return Ok(format!("no matches for /{pattern}/ in {display_root}"));
    }

    let mut lines = String::new();
    for (display, line_number, content) in hits.iter().take(max) {
        lines.push_str(&format!("{display}:{line_number}:{content}\n"));
    }
    if truncated {
        lines.push_str(&format!("… (results truncated at {max})\n"));
    }
    Ok(lines)
}

/// Whether an fff-relative path lies under a dot entry.
///
/// fff's own walker only hides dot entries outside a git repository, so inside
/// one it indexes `.env` and `.shuvarie/` as well -- which the tool must not
/// read implicitly. A `path` naming a hidden entry still searches it: the root
/// never counts as a hidden component of its own results.
fn is_hidden(rel: &str) -> bool {
    rel.split('/').any(|segment| segment.starts_with('.'))
}

fn display_path(rel: &str, display_root: &str) -> String {
    if rel.is_empty() {
        display_root.to_string()
    } else if display_root == "." {
        rel.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    #[tokio::test]
    async fn grep_finds_with_substring_include() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep::new(crate::test_util::access())
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
        let out = Grep::new(crate::test_util::access())
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
    async fn grep_respects_gitignore_inside_a_repository() {
        let (dir, _guard) = tempdir();
        // The walker only applies `.gitignore` inside a repository; for the
        // `ignore` machinery an existing `.git` is what marks one.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".gitignore", "ignored.txt\n").unwrap();
        std::fs::write("ignored.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("ignored.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_skips_hidden_outside_a_repository() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains("secret.txt"), "{text}");
        drop(dir);
    }

    /// fff's walker only hides dot entries outside a git repository, so this
    /// guards the tool's own filter: a repository must not leak `.env` or
    /// `.shuvarie/` into results just because fff indexed them.
    #[tokio::test]
    async fn grep_skips_hidden_inside_a_repository() {
        let (dir, _guard) = tempdir();
        // An empty `.git` is enough for the walker to treat the root as a repo.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".env", "needle=1\n").unwrap();
        std::fs::create_dir(".shuvarie").unwrap();
        std::fs::write(".shuvarie/session.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("kept.txt"), "{text}");
        assert!(!text.contains(".env"), "{text}");
        assert!(!text.contains("session.txt"), "{text}");
        drop(dir);
    }

    /// A `path` that names a hidden entry is explicit, so the tool's own hidden
    /// filter must not drop it (the permission layer still asks, as it should:
    /// dot directories are gated).
    #[tokio::test]
    async fn grep_searches_an_explicit_hidden_path() {
        use crate::test_util::access_with_answering_gate;

        let (dir, _guard) = tempdir();
        std::fs::create_dir(".config").unwrap();
        std::fs::write(".config/keys.txt", "needle\n").unwrap();
        std::fs::write("kept.txt", "needle\n").unwrap();
        let (access, mut requests) =
            access_with_answering_gate(&shuvarie_config::PermissionsConfig::builtin());
        let tool = Grep::new(access);
        let mut ctx = new_ctx();
        let (result, ()) = tokio::join!(
            tool.call(&mut ctx, json!({ "pattern": "needle", "path": ".config" })),
            async {
                let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    tokio::task::yield_now().await;
                    requests.recv().await
                })
                .await
                .expect("no permission request within 5s")
                .expect("permission request");
                request
                    .respond
                    .send(crate::permissions::PermissionAnswer::AllowSession)
                    .expect("answer sent");
            }
        );
        let out = result.unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains(".config/keys.txt:1"), "{text}");
        assert!(!text.contains("kept.txt"), "{text}");
        drop(dir);
    }

    /// fff skips files past its 10 MiB default; the tool lifts that cap so a
    /// large file answers with its matches instead of "no matches".
    #[tokio::test]
    async fn grep_searches_files_larger_than_the_fff_default_cap() {
        let (dir, _guard) = tempdir();
        let mut content = String::with_capacity(11 * 1024 * 1024);
        for _ in 0..(11 * 1024) {
            content.push_str(&"x".repeat(1023));
            content.push('\n');
        }
        content.push_str("needle\n");
        std::fs::write("big.txt", content).unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("big.txt:11265"), "{text}");
        drop(dir);
    }

    /// fff returns matches in parallel-chunk order; the tool sorts them so
    /// repeated calls over unchanged files agree.
    #[tokio::test]
    async fn grep_output_is_sorted_by_path_and_line() {
        let (dir, _guard) = tempdir();
        std::fs::write("b.txt", "needle 1\nneedle 2\n").unwrap();
        std::fs::write("a.txt", "needle 3\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        let order: Vec<&str> = text
            .lines()
            .filter(|line| line.contains("needle"))
            .collect();
        assert_eq!(
            order,
            vec!["a.txt:1:needle 3", "b.txt:1:needle 1", "b.txt:2:needle 2"],
            "{text}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn grep_include_glob_matches_subdirs() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("src").unwrap();
        std::fs::write("src/a.rs", "needle\n").unwrap();
        std::fs::write("src/b.txt", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
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
    async fn grep_skips_binary_extensions() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.png", "needle\n").unwrap();
        std::fs::write("text.dat", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("text.dat:1"), "{text}");
        assert!(!text.contains("bin.png"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn grep_single_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "needle\nother\n").unwrap();
        std::fs::write("b.txt", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
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
        let none = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        assert!(none.as_text().unwrap().contains("no matches"));
        drop(dir);
    }

    #[tokio::test]
    async fn grep_reports_bad_pattern() {
        let (dir, _guard) = tempdir();
        let err = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "([" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("bad pattern"));
        drop(dir);
    }

    /// fff retries a constrained query that found nothing with the constraints
    /// dropped; `include` must stay authoritative regardless.
    #[tokio::test]
    async fn grep_include_stays_authoritative_on_a_zero_match_glob() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "needle\n").unwrap();
        std::fs::write("b.rs", "needle\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "pattern": "needle", "include": "*.kdl" }),
            )
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("no matches"), "{text}");
        assert!(!text.contains("a.txt"), "{text}");
        assert!(!text.contains("b.rs"), "{text}");
        drop(dir);
    }

    /// fff's query parser strips a leading `\` before `*`, `/` and `!`, which
    /// would turn this valid regex into the compile error `*ptr`.
    #[tokio::test]
    async fn grep_keeps_an_escaped_leading_metacharacter() {
        let (dir, _guard) = tempdir();
        std::fs::write("c.txt", "a *ptr deref\nplain ptr\n").unwrap();
        let out = Grep::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "\\*ptr" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("c.txt:1:a *ptr deref"), "{text}");
        assert!(!text.contains("plain ptr"), "{text}");
        drop(dir);
    }
}
