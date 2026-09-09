use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::resolve_read;

pub(crate) struct ListDir;

impl Tool for ListDir {
    const NAME: &'static str = "list_dir";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "List the entries of a directory, one per line (directories suffixed with '/'). Entries are sorted by name."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list, relative to the workspace root (defaults to the root itself)" }
            },
            "required": []
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let abs = resolve_read(path)?;
            let entries = std::fs::read_dir(&abs).map_err(|e| format!("read_dir {path}: {e}"))?;
            let mut names: Vec<String> = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|e| format!("read_dir {path}: {e}"))?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let suffix = if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    "/"
                } else {
                    ""
                };
                names.push(format!("{name}{suffix}"));
            }
            names.sort();
            if names.is_empty() {
                Ok(ToolOutput::text(format!("{path} is empty")))
            } else {
                Ok(ToolOutput::text(names.join("\n")))
            }
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
    async fn list_dir_sorted() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("zdir").unwrap();
        std::fs::write("afile", "").unwrap();
        let out = ListDir.call(&mut new_ctx(), json!({})).await.unwrap();
        let lines: Vec<&str> = out.as_text().unwrap().lines().collect();
        assert_eq!(lines, vec!["afile", "zdir/"]);
        drop(dir);
    }
}
