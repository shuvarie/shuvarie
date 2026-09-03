use std::path::PathBuf;

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::lsp_manager::SharedManager;

use super::workspace_root;

pub(crate) struct Lsp {
    lsp: SharedManager,
}

impl Lsp {
    pub(crate) fn new(lsp: SharedManager) -> Self {
        Self { lsp }
    }
}

impl Tool for Lsp {
    const NAME: &'static str = "lsp";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Manage Language Server Protocol (LSP) servers for the workspace. \
                `action` is one of `start` | `stop` | `restart` | `list` | `analyze`. For `start`/`stop`/`restart`, \
                `name` is the exact server id (a language like `rust`, `go`, `typescript`). \
                For `list`, `name` is an optional substring/fuzzy filter (matched against server \
                name + language), and `all` controls whether to list all configured servers \
                (true) or only the currently running ones (false, the default). \
                For `analyze`, `path` is a file or directory (defaults to the workspace root); \
                the matching server is started if needed and the file(s) are opened so the \
                server publishes diagnostics, which are returned."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["start", "stop", "restart", "list", "analyze"], "description": "Action to perform" },
                "name": { "type": "string", "description": "Server name (exact id for start/stop/restart; substring filter for list)" },
                "all": { "type": "boolean", "description": "For `list`: list all configured servers instead of only running ones (default false)" },
                "path": { "type": "string", "description": "For `analyze`: file or directory to analyze (defaults to the workspace root)" }
            },
            "required": ["action"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let lsp = self.lsp.clone();
        let result: Result<ToolOutput, String> = async move {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing string argument 'action'".to_string())?
                .to_string();
            let name = args.get("name").and_then(Value::as_str).map(String::from);
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            if action == "analyze" {
                return analyze_paths(&lsp, args.get("path").and_then(Value::as_str)).await;
            }
            let mut mgr = lsp.lock().await;
            match action.as_str() {
                "start" => {
                    let Some(name) = name else {
                        return Err("'name' is required for `lsp start`".into());
                    };
                    mgr.start(&name)
                        .await
                        .map(|_| ToolOutput::text(format!("started LSP server {name}")))
                }
                "stop" => {
                    let Some(name) = name else {
                        return Err("'name' is required for `lsp stop`".into());
                    };
                    mgr.stop(&name)
                        .await
                        .map(|_| ToolOutput::text(format!("stopped LSP server {name}")))
                }
                "restart" => {
                    let Some(name) = name else {
                        return Err("'name' is required for `lsp restart`".into());
                    };
                    mgr.restart(&name)
                        .await
                        .map(|_| ToolOutput::text(format!("restarted LSP server {name}")))
                }
                "list" => {
                    let entries = mgr.list(all, name.as_deref());
                    let mut text = String::new();
                    if entries.is_empty() {
                        text.push_str("no servers matching filter");
                    } else {
                        for e in entries {
                            text.push_str(&format!(
                                "{:<12} {:<28} {}\n",
                                e.name,
                                e.command.join(" "),
                                e.status.as_str()
                            ));
                        }
                    }
                    Ok(ToolOutput::text(text))
                }
                other => Err(format!("unknown LSP action '{other}'")),
            }
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

const ANALYZE_MAX_FILES: usize = 50;
const ANALYZE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
const ANALYZE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// `lsp analyze [path]`: open the target file(s) with their LSP server and
/// return the published diagnostics. The manager lock is released between
/// polls so the core task's diagnostic pump can drain `publishDiagnostics`
/// into the manager's map.
async fn analyze_paths(lsp: &SharedManager, path: Option<&str>) -> Result<ToolOutput, String> {
    let target = path.unwrap_or(".");
    let abs = resolve(target)?;
    let mut files: Vec<PathBuf> = Vec::new();
    if abs.is_dir() {
        let mut walk = ignore::WalkBuilder::new(&abs)
            .hidden(false)
            .git_ignore(true)
            .build();
        for entry in walk.by_ref().flatten() {
            if files.len() >= ANALYZE_MAX_FILES {
                break;
            }
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                files.push(entry.into_path());
            }
        }
    } else {
        files.push(abs);
    }
    if files.is_empty() {
        return Ok(ToolOutput::text("no files to analyze"));
    }
    let opened = {
        let mut mgr = lsp.lock().await;
        mgr.analyze(&files).await?
    };
    if opened.is_empty() {
        return Ok(ToolOutput::text(
            "no files matched a configured LSP server (check [lsp] config)",
        ));
    }
    let deadline = std::time::Instant::now() + ANALYZE_TIMEOUT;
    let mut out = String::new();
    loop {
        let (pending, rendered) = {
            let mgr = lsp.lock().await;
            let mut pending = Vec::new();
            let mut rendered = String::new();
            for rel in &opened {
                let diags = mgr.diagnostics_for(rel);
                if diags.is_empty() {
                    pending.push(rel.clone());
                    continue;
                }
                for d in &diags {
                    rendered.push_str(&format!(
                        "{}:{}:{}: {}: {}\n",
                        rel,
                        d.line,
                        d.col,
                        d.severity.as_str(),
                        d.message
                    ));
                }
            }
            (pending, rendered)
        };
        out.push_str(&rendered);
        if pending.is_empty() || std::time::Instant::now() >= deadline {
            if pending.is_empty() {
                break;
            }
            for rel in &pending {
                out.push_str(&format!("{rel}: no diagnostics published\n"));
            }
            break;
        }
        tokio::time::sleep(ANALYZE_POLL_INTERVAL).await;
    }
    if out.is_empty() {
        out.push_str("no diagnostics");
    }
    Ok(ToolOutput::text(out))
}

fn resolve(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(path);
    let canonical = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    if !canonical.starts_with(&root) {
        return Err(format!("{path} resolves outside the workspace"));
    }
    Ok(canonical)
}
