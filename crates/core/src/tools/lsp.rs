use std::path::{Path, PathBuf};

use fff_search::{ContentCacheBudget, FFFMode, FilePickerOptions, file_picker::FilePicker};
use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::lsp_manager::SharedManager;

use crate::permissions::Access;

use super::is_hidden_path;

pub(crate) struct Lsp {
    lsp: SharedManager,
    access: Access,
}

impl Lsp {
    pub(crate) fn new(lsp: SharedManager, access: Access) -> Self {
        Self { lsp, access }
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
        let root = self.access.workspace_root().to_path_buf();
        let result: Result<ToolOutput, String> = async move {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing string argument 'action'".to_string())?
                .to_string();
            let name = args.get("name").and_then(Value::as_str).map(String::from);
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
            if action == "analyze" {
                return analyze_paths(&lsp, &root, args.get("path").and_then(Value::as_str)).await;
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
async fn analyze_paths(
    lsp: &SharedManager,
    root: &Path,
    path: Option<&str>,
) -> Result<ToolOutput, String> {
    let target = path.unwrap_or(".");
    let abs = resolve(root, target)?;
    let files = tokio::task::spawn_blocking(move || analyze_targets(abs))
        .await
        .map_err(|e| format!("lsp: {e}"))??;
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

fn resolve(root: &Path, path: &str) -> Result<PathBuf, String> {
    let joined = root.join(path);
    let canonical = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    if !canonical.starts_with(root) {
        return Err(format!("{path} resolves outside the workspace"));
    }
    Ok(canonical)
}

/// The files `lsp analyze` opens: the target itself, or everything the fff
/// index found under it, capped at [`ANALYZE_MAX_FILES`].
///
/// The walk is the one `glob`, `grep` and `fuzzy_find` index with, so a
/// repository's `.gitignore` keeps build output out of the budget and `.git/`
/// and `node_modules/` are never descended into. The `ignore` walk this
/// replaces kept hidden entries, walked `.git/objects` and spent most of the
/// cap on object files; and because the cap trims the set before the manager
/// filters by server, which files got analyzed followed directory order. The
/// sort makes the kept files a stable prefix instead.
fn analyze_targets(abs: PathBuf) -> Result<Vec<PathBuf>, String> {
    if !abs.is_dir() {
        return Ok(vec![abs]);
    }
    let mut picker = FilePicker::new(FilePickerOptions {
        base_path: abs.to_string_lossy().into_owned(),
        mode: FFFMode::Ai,
        // Nothing here outlives the call, so a watcher would only burn threads.
        watch: false,
        // Discovery reads paths, never contents.
        cache_budget: Some(ContentCacheBudget::with_max_files(0)),
        ..Default::default()
    })
    .map_err(|e| format!("lsp: {e}"))?;
    picker.collect_files().map_err(|e| format!("lsp: {e}"))?;

    let mut targets: Vec<(String, PathBuf)> = Vec::new();
    for file in picker.get_files() {
        let rel = file.relative_path(&picker);
        // fff indexes dot entries inside a git repository; no server is wanted
        // for `.env` or `.git/` metadata.
        if is_hidden_path(&rel) {
            continue;
        }
        targets.push((rel, file.absolute_path(&picker, picker.base_path())));
    }
    targets.sort();
    targets.truncate(ANALYZE_MAX_FILES);
    Ok(targets.into_iter().map(|(_, path)| path).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::tempdir;

    #[test]
    fn analyze_targets_skips_hidden_and_ignored_files() {
        let (dir, _guard) = tempdir();
        // fff only reads .gitignore inside a repository, so the rule needs a
        // `.git` next to it -- the same one that makes `.git/` internals
        // indexable and therefore something to filter.
        std::fs::create_dir(".git").unwrap();
        std::fs::write(".git/config", "").unwrap();
        std::fs::write(".gitignore", "target/\n*.log\n").unwrap();
        std::fs::create_dir("target").unwrap();
        std::fs::write("target/out.bin", "").unwrap();
        std::fs::write("debug.log", "").unwrap();
        std::fs::create_dir("src").unwrap();
        std::fs::write("src/main.rs", "").unwrap();
        std::fs::write("src/lib.rs", "").unwrap();

        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let targets = analyze_targets(root.clone()).unwrap();
        let rel: Vec<String> = targets
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(rel, vec!["src/lib.rs", "src/main.rs"], "{rel:?}");
        drop(dir);
    }

    /// The cap trims before the manager filters by server, so which files
    /// survive it has to be the sorted prefix rather than scan order.
    #[test]
    fn analyze_targets_caps_to_a_sorted_prefix() {
        let (dir, _guard) = tempdir();
        for i in 0..(ANALYZE_MAX_FILES + 10) {
            std::fs::write(format!("f{i:03}.txt", i = i), "").unwrap();
        }
        let root = std::env::current_dir().unwrap().canonicalize().unwrap();
        let targets = analyze_targets(root.clone()).unwrap();
        assert_eq!(targets.len(), ANALYZE_MAX_FILES);
        let names: Vec<String> = targets
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.first().unwrap(), "f000.txt");
        assert_eq!(names.last().unwrap(), "f049.txt");
        drop(dir);
    }
}
