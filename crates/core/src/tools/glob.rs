use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind, resolve_read};

use super::arg_value;

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
        "Fast file pattern matching tool that works with any codebase size. Supports glob patterns like \"**/*.js\" or \"src/**/*.ts\". Returns matching file paths. Use this tool when you need to find files by name patterns."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "The glob pattern to match files against" },
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
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let abs = resolve_read(path)?;
            access.authorize_path(PathKind::Read, &abs, path).await?;
            if abs.is_file() {
                return Err(format!("glob path must be a directory: {path}"));
            }
            let glob = globset::GlobBuilder::new(&pattern)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("bad glob pattern: {e}"))?;
            let matcher = globset::GlobSetBuilder::new()
                .add(glob)
                .build()
                .map_err(|e| format!("bad glob pattern: {e}"))?;
            let walk = ignore::WalkBuilder::new(&abs)
                .hidden(true)
                .git_ignore(true)
                .standard_filters(true)
                .require_git(false)
                .build();
            let mut matches: Vec<String> = Vec::new();
            for entry in walk.flatten() {
                if matches.len() >= GLOB_MAX_RESULTS {
                    break;
                }
                if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    let rel = entry
                        .path()
                        .strip_prefix(&abs)
                        .unwrap_or(entry.path())
                        .to_string_lossy()
                        .into_owned();
                    if matcher.is_match(&rel) {
                        matches.push(rel);
                    }
                }
            }
            matches.sort();
            if matches.is_empty() {
                return Ok(ToolOutput::text("No files found"));
            }
            let mut out = matches.join("\n");
            if matches.len() >= GLOB_MAX_RESULTS {
                out.push_str(&format!(
                    "\n\n(Results are truncated: showing first {GLOB_MAX_RESULTS} results. Consider using a more specific path or pattern.)"
                ));
            }
            Ok(ToolOutput::text(out))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    #[tokio::test]
    async fn glob_matches_recursively() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("src/sub").unwrap();
        std::fs::write("src/a.rs", "").unwrap();
        std::fs::write("src/sub/b.rs", "").unwrap();
        std::fs::write("src/c.txt", "").unwrap();
        let out = Glob::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "**/*.rs" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("src/a.rs"), "{text}");
        assert!(text.contains("src/sub/b.rs"), "{text}");
        assert!(!text.contains("src/c.txt"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn glob_honors_gitignore_and_hidden() {
        let (dir, _guard) = tempdir();
        std::fs::write(".gitignore", "ignored.txt\n").unwrap();
        std::fs::write("ignored.txt", "").unwrap();
        std::fs::write("kept.txt", "").unwrap();
        std::fs::create_dir(".hidden").unwrap();
        std::fs::write(".hidden/secret.txt", "").unwrap();
        let out = Glob::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "**/*.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
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
        let out = Glob::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "*.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("truncated"), "{text}");
        let none = Glob::new(crate::test_util::access())
            .call(&mut new_ctx(), json!({ "pattern": "*.zzz" }))
            .await
            .unwrap();
        assert!(none.as_text().unwrap().contains("No files found"));
        drop(dir);
    }

    #[tokio::test]
    async fn glob_rejects_file_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "").unwrap();
        let err = Glob::new(crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "pattern": "*.txt", "path": "a.txt" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must be a directory"));
        drop(dir);
    }
}
