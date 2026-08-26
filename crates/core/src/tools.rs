use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use shuvarie_llm::{DiffLine, DiffLineKind, FileChange, Tool, ToolDefinition, ToolOutput};

use crate::approval::{ApprovalGate, ApprovalReason};
use crate::lsp_manager::SharedManager;

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

struct ReadFile {
    gate: ApprovalGate,
    lsp: Option<SharedManager>,
}

impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a text file from the workspace, optionally restricted to a line range. Returns the requested lines or an error when the path does not exist, is a directory, or contains binary data."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path of the file, relative to the workspace root" },
                    "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Defaults to 1" },
                    "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read. Defaults to all lines" }
                },
                "required": ["path"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let gate = self.gate.clone();
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            let (abs, reason) = resolve_checked(&path)?;
            if let Some(reason) = reason {
                gate.request("read_file", &path, reason).await?;
            }
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a file"));
            }
            let data = std::fs::read(&abs).map_err(|e| format!("read {path}: {e}"))?;
            if data.contains(&0) {
                return Err(format!("'{path}' appears to be binary; refusing to read"));
            }
            let content_owned =
                String::from_utf8_lossy(&data[..data.len().min(MAX_READ_BYTES)]).into_owned();
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_open(Path::new(&path), &content_owned)
                    .await;
            }
            let lines: Vec<&str> = content_owned.lines().collect();
            let start = offset.unwrap_or(1).max(1) as usize - 1;
            let end = match limit {
                Some(n) => (start + n as usize).min(lines.len()),
                None => lines.len(),
            };
            if start >= lines.len() {
                return Ok(ToolOutput::text("(empty)"));
            }
            let mut out = String::new();
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{:>6} | {line}\n", start + i + 1));
            }
            Ok(ToolOutput::text(out))
        })
    }
}

struct WriteFile {
    gate: ApprovalGate,
    lsp: Option<SharedManager>,
}

impl Tool for WriteFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Create or overwrite a file in the working directory, creating parent directories as needed."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path of the file to write" },
                    "content": { "type": "string", "description": "Full new contents of the file" }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let gate = self.gate.clone();
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let content = arg_value(&args, "content")?;
            let (abs, reason) = resolve_for_write_checked(&path)?;
            if let Some(reason) = reason {
                gate.request("write_file", &path, reason).await?;
            }
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
            }
            let original = std::fs::read_to_string(&abs).ok();
            std::fs::write(&abs, &content).map_err(|e| format!("write {path}: {e}"))?;
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &content)
                    .await;
            }
            Ok(ToolOutput::with_file_change(
                format!("wrote {} bytes to {path}", content.len()),
                FileChange::Write {
                    path,
                    content,
                    original,
                },
            ))
        })
    }
}

struct EditFile {
    gate: ApprovalGate,
    lsp: Option<SharedManager>,
}

impl Tool for EditFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "edit_file".into(),
            description: "Replace text in an existing file with a string replacement. When `old` appears more than once and `occurrence` is unset the edit is rejected; pass `occurrence` to pick the nth match (1-based)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path of the file to edit" },
                    "old": { "type": "string", "description": "Exact text to find (must appear in the file)" },
                    "new": { "type": "string", "description": "Replacement text" },
                    "occurrence": { "type": "integer", "minimum": 1, "description": "Which match to replace (1-based). Required when `old` appears multiple times" }
                },
                "required": ["path", "old", "new"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let gate = self.gate.clone();
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let old = arg_value(&args, "old")?;
            let new = arg_value(&args, "new")?;
            let occurrence = args.get("occurrence").and_then(Value::as_u64);
            let (abs, reason) = resolve_checked(&path)?;
            if let Some(reason) = reason {
                gate.request("edit_file", &path, reason).await?;
            }
            let content = std::fs::read_to_string(&abs).map_err(|e| format!("read {path}: {e}"))?;
            if old.is_empty() {
                return Err("cannot edit with an empty 'old' text".into());
            }
            let matches: Vec<usize> = content.match_indices(&old).map(|(i, _)| i).collect();
            if matches.is_empty() {
                return Err(format!("'old' text not found in {path} (occurrences: 0)"));
            }
            let idx = match occurrence {
                Some(n) if n as usize <= matches.len() => matches[n as usize - 1],
                Some(n) => {
                    return Err(format!(
                        "'old' text appears {} times; occurrence {n} is out of range",
                        matches.len()
                    ));
                }
                None if matches.len() > 1 => {
                    return Err(format!(
                        "'old' text appears {} times; pass 'occurrence' to select one",
                        matches.len()
                    ));
                }
                None => matches[0],
            };
            let mut edited = content.clone();
            edited.replace_range(idx..idx + old.len(), &new);
            let diff = compute_diff(&content, &edited);
            std::fs::write(&abs, &edited).map_err(|e| format!("write {path}: {e}"))?;
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &edited)
                    .await;
            }
            Ok(ToolOutput::with_file_change(
                format!("edited {path}: replaced 1 of {} occurrences", matches.len()),
                FileChange::Edit {
                    path,
                    diff,
                    original: content,
                    new: edited,
                },
            ))
        })
    }
}

struct RunShell;

impl Tool for RunShell {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_shell".into(),
            description: "Run a shell command line in the workspace, executed through the system shell (`sh -c` on Unix, `powershell -NoProfile -Command` on Windows). Pipes, redirects, and shell operators work naturally. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command is killed when it exceeds the timeout."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command line to run (passed to `sh -c` / `powershell -NoProfile -Command`)" },
                    "cwd": { "type": "string", "description": "Working directory, relative to the workspace root. Defaults to the workspace root" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Timeout in seconds (default 30)" }
                },
                "required": ["command"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        Box::pin(async move {
            let command = arg_value(&args, "command")?;
            let cwd = args.get("cwd").and_then(Value::as_str);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS);
            let cwd_abs = match cwd {
                Some(c) => resolve(c)?,
                None => workspace_root()?,
            };
            let mut builder = tokio::process::Command::new(shell_bin());
            shell_args(&mut builder, &command);
            builder
                .current_dir(&cwd_abs)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let mut child = builder.spawn().map_err(|e| format!("spawn shell: {e}"))?;
            let status = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                child.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(e)) => return Err(format!("wait shell: {e}")),
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(format!(
                        "shell command timed out after {timeout_secs}s (killed)"
                    ));
                }
            };
            let mut stdout = String::new();
            let mut stderr = String::new();
            if let Some(mut out) = child.stdout.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let _ = out.read_to_end(&mut buf).await;
                stdout = String::from_utf8_lossy(&buf).into_owned();
            }
            if let Some(mut err) = child.stderr.take() {
                use tokio::io::AsyncReadExt;
                let mut buf = Vec::new();
                let _ = err.read_to_end(&mut buf).await;
                stderr = String::from_utf8_lossy(&buf).into_owned();
            }
            let mut body = stdout;
            if !stderr.is_empty() {
                body.push_str(&stderr);
            }
            let trimmed = body.trim();
            let capped: String = trimmed.chars().take(MAX_COMMAND_OUTPUT).collect();
            if !status.success() {
                return Err(format!("shell exited with {status}:\n{capped}"));
            }
            if trimmed.is_empty() {
                Ok(ToolOutput::text(format!(
                    "shell exited with {status} (no output)"
                )))
            } else {
                Ok(ToolOutput::text(format!("exit {status}:\n{capped}")))
            }
        })
    }
}

#[cfg(unix)]
fn shell_bin() -> &'static str {
    "sh"
}

#[cfg(unix)]
fn shell_args(cmd: &mut tokio::process::Command, command: &str) {
    cmd.arg("-c").arg(command);
}

#[cfg(windows)]
fn shell_bin() -> &'static str {
    "powershell"
}

#[cfg(windows)]
fn shell_args(cmd: &mut tokio::process::Command, command: &str) {
    cmd.arg("-NoProfile").arg("-Command").arg(command);
}

struct ListDir {
    gate: ApprovalGate,
}

impl Tool for ListDir {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_dir".into(),
            description: "List the entries of a directory, one per line (directories suffixed with '/'). Entries are sorted by name."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Directory to list, relative to the workspace root (defaults to the root itself)" }
                },
                "required": []
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let (abs, reason) = resolve_checked(path)?;
            if let Some(reason) = reason {
                gate.request("list_dir", path, reason).await?;
            }
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
        })
    }
}

struct Grep {
    gate: ApprovalGate,
}

impl Tool for Grep {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Recursively search text files for a regular expression, printing `path:line: content`. Results are capped (default 200)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Regular expression to search for" },
                    "path": { "type": "string", "description": "Directory or file to search, relative to the workspace root (defaults to the workspace root)" },
                    "include": { "type": "string", "description": "Only search files whose name contains this string (e.g. '.rs')" },
                    "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of matches (default 200)" }
                },
                "required": ["pattern"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let gate = self.gate.clone();
        Box::pin(async move {
            let pattern = arg_value(&args, "pattern")?;
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let include = args.get("include").and_then(Value::as_str);
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let regex = regex::Regex::new(&pattern).map_err(|e| format!("bad pattern: {e}"))?;
            let (abs, reason) = resolve_checked(path)?;
            if let Some(reason) = reason {
                gate.request("grep", path, reason).await?;
            }
            let mut hits = 0;
            let mut out = String::new();
            let mut walk = |entry: &Path, rel: &Path| -> Result<bool, String> {
                if hits >= max {
                    return Ok(true);
                }
                if entry.is_dir() {
                    return Ok(false);
                }
                if let Some(inc) = include
                    && !entry
                        .file_name()
                        .map(|n| n.to_string_lossy().contains(inc))
                        .unwrap_or(false)
                {
                    return Ok(false);
                }
                let data = match std::fs::read(entry) {
                    Ok(d) => d,
                    Err(_) => return Ok(false),
                };
                if data.contains(&0) || data.len() > MAX_READ_BYTES {
                    return Ok(false);
                }
                let content = String::from_utf8_lossy(&data);
                for (i, line) in content.lines().enumerate() {
                    if hits >= max {
                        break;
                    }
                    if regex.is_match(line) {
                        out.push_str(&format!("{}:{}:{}\n", rel.display(), i + 1, line));
                        hits += 1;
                    }
                }
                Ok(false)
            };
            walk_dir(&abs, path, &mut walk).map_err(|e| format!("grep: {e}"))?;
            if out.is_empty() {
                Ok(ToolOutput::text(format!(
                    "no matches for /{pattern}/ in {path}"
                )))
            } else if hits >= max {
                out.push_str(&format!("… ({} results, cap {max})", hits + 1));
                Ok(ToolOutput::text(out))
            } else {
                Ok(ToolOutput::text(out))
            }
        })
    }
}

fn walk_dir(
    dir: &Path,
    display_root: &str,
    f: &mut impl FnMut(&Path, &Path) -> Result<bool, String>,
) -> Result<(), String> {
    let mut pending = vec![(dir.to_path_buf(), PathBuf::from(display_root))];
    while let Some((entry, rel)) = pending.pop() {
        if f(&entry, &rel)? {
            return Ok(());
        }
        if entry.is_dir() {
            let entries = std::fs::read_dir(&entry).map_err(|e| e.to_string())?;
            for sub in entries {
                let sub = sub.map_err(|e| e.to_string())?;
                let name = sub.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                pending.push((sub.path(), rel.join(&name)));
            }
        }
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cwd: {e}"))
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

fn hidden_reason(path: &str) -> Option<ApprovalReason> {
    let has_hidden = path
        .split(['/', '\\'])
        .any(|c| c.starts_with('.') && c != "." && c != "..");
    has_hidden.then_some(ApprovalReason::HiddenPath)
}

fn resolve_checked(path: &str) -> Result<(PathBuf, Option<ApprovalReason>), String> {
    let root = workspace_root()?;
    let joined = root.join(path);
    let canonical = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    let reason = if !canonical.starts_with(&root) {
        Some(ApprovalReason::OutsideWorkspace)
    } else {
        hidden_reason(path)
    };
    Ok((canonical, reason))
}

fn resolve_for_write_checked(path: &str) -> Result<(PathBuf, Option<ApprovalReason>), String> {
    let root = workspace_root()?;
    let joined = root.join(path);
    let reason = hidden_reason(path);
    if joined.exists() {
        let abs = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
        let reason = if !abs.starts_with(&root) {
            Some(ApprovalReason::OutsideWorkspace)
        } else {
            reason
        };
        return Ok((abs, reason));
    }
    let mut existing = joined.clone();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("{path}: invalid path"))?
            .to_os_string();
        missing.push(name);
        existing = existing
            .parent()
            .ok_or_else(|| format!("{path}: invalid path"))?
            .to_path_buf();
    }
    let mut abs = existing
        .canonicalize()
        .map_err(|e| format!("{path}: {e}"))?;
    for name in missing.iter().rev() {
        abs.push(name);
    }
    let reason = if !abs.starts_with(&root) {
        Some(ApprovalReason::OutsideWorkspace)
    } else {
        reason
    };
    Ok((abs, reason))
}

fn compute_diff(old: &str, new: &str) -> Vec<DiffLine> {
    let diff = similar::TextDiff::from_lines(old, new);
    let mut lines: Vec<DiffLine> = Vec::new();
    for group in diff.grouped_ops(3) {
        if !lines.is_empty() {
            lines.push(DiffLine {
                kind: DiffLineKind::Ellipsis,
                old_line: None,
                new_line: None,
                text: String::new(),
            });
        }
        for op in group {
            for change in diff.iter_changes(&op) {
                let (kind, old_line, new_line) = match change.tag() {
                    similar::ChangeTag::Delete => (DiffLineKind::Remove, change.old_index(), None),
                    similar::ChangeTag::Insert => (DiffLineKind::Add, None, change.new_index()),
                    similar::ChangeTag::Equal => (
                        DiffLineKind::Context,
                        change.old_index(),
                        change.new_index(),
                    ),
                };
                let old_line = old_line.map(|i| i as u64 + 1);
                let new_line = new_line.map(|i| i as u64 + 1);
                lines.push(DiffLine {
                    kind,
                    old_line,
                    new_line,
                    text: change.value().to_string(),
                });
            }
        }
    }
    lines
}

struct Lsp {
    lsp: SharedManager,
}

impl Tool for Lsp {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "lsp".into(),
            description: "Manage Language Server Protocol (LSP) servers for the workspace. \
                `action` is one of `start` | `stop` | `restart` | `list`. For `start`/`stop`/`restart`, \
                `name` is the exact server id (a language like `rust`, `go`, `typescript`). \
                For `list`, `name` is an optional substring/fuzzy filter (matched against server \
                name + language), and `all` controls whether to list all configured servers \
                (true) or only the currently running ones (false, the default)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["start", "stop", "restart", "list"], "description": "Action to perform" },
                    "name": { "type": "string", "description": "Server name (exact id for start/stop/restart; substring filter for list)" },
                    "all": { "type": "boolean", "description": "For `list`: list all configured servers instead of only running ones (default false)" }
                },
                "required": ["action"]
            }),
        }
    }

    fn call(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, String>> + Send>> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            let action = args
                .get("action")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing string argument 'action'".to_string())?
                .to_string();
            let name = args.get("name").and_then(Value::as_str).map(String::from);
            let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
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
        })
    }
}

pub fn all_tools(gate: ApprovalGate, lsp: SharedManager) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(WriteFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(EditFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(RunShell),
        std::sync::Arc::new(ListDir { gate: gate.clone() }),
        std::sync::Arc::new(Grep { gate }),
        std::sync::Arc::new(Lsp { lsp }),
    ]
}

pub fn read_tools(gate: ApprovalGate, lsp: SharedManager) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(ListDir { gate: gate.clone() }),
        std::sync::Arc::new(Grep { gate }),
        std::sync::Arc::new(Lsp { lsp }),
    ]
}

pub fn command_tools(_gate: ApprovalGate) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![std::sync::Arc::new(RunShell)]
}

pub fn edit_tools(gate: ApprovalGate, lsp: SharedManager) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(WriteFile {
            gate: gate.clone(),
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(EditFile {
            gate,
            lsp: Some(lsp.clone()),
        }),
        std::sync::Arc::new(Lsp { lsp }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    fn tempdir() -> (TempDir, MutexGuard<'static, ()>) {
        let guard = crate::test_util::test_util::lock_cwd();
        let dir = TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    fn gate() -> ApprovalGate {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        ApprovalGate::new(tx)
    }

    fn no_lsp() -> Option<SharedManager> {
        None
    }

    #[tokio::test]
    async fn read_file_with_range() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\nthree\n").unwrap();
        let out = ReadFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "a.txt", "offset": 2, "limit": 1 }))
        .await
        .unwrap();
        assert!(out.text.contains("two"), "{}", out.text);
        let err = ReadFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "missing.txt" }))
        .await
        .unwrap_err();
        assert!(err.contains("missing.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_refuses_binary() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", [0, 1, 2, 3]).unwrap();
        let err = ReadFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "bin.dat" }))
        .await
        .unwrap_err();
        assert!(err.contains("binary"));
        drop(dir);
    }

    #[tokio::test]
    async fn write_creates_parents() {
        let (dir, _guard) = tempdir();
        let out = WriteFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "sub/deep/f.txt", "content": "hello" }))
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("sub/deep/f.txt").unwrap(), "hello");
        assert!(matches!(
            out.file_change,
            Some(FileChange::Write { ref path, .. }) if path == "sub/deep/f.txt"
        ));
        drop(dir);
    }

    #[tokio::test]
    async fn edit_replaces_and_detects_ambiguity() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "a b a").unwrap();
        let err = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "e.txt", "old": "a", "new": "x" }))
        .await
        .unwrap_err();
        assert!(err.contains("occurrence"));
        let out = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "e.txt", "old": "a", "new": "x", "occurrence": 2 }))
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "a b x");
        assert!(matches!(
            out.file_change,
            Some(FileChange::Edit { ref diff, .. }) if !diff.is_empty()
        ));
        let err = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(json!({ "path": "e.txt", "old": "zzz", "new": "x" }))
        .await
        .unwrap_err();
        assert!(err.contains("not found"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_success_and_timeout() {
        let (dir, _guard) = tempdir();
        let out = RunShell
            .call(json!({ "command": "echo hello" }))
            .await
            .unwrap();
        assert!(out.text.contains("hello"));
        let err = RunShell
            .call(json!({ "command": "sleep 5", "timeout_secs": 1 }))
            .await
            .unwrap_err();
        assert!(err.contains("timed out"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_failure_returns_error() {
        let (dir, _guard) = tempdir();
        let err = RunShell
            .call(json!({ "command": "exit 3" }))
            .await
            .unwrap_err();
        assert!(err.contains("3"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_supports_pipes() {
        let (dir, _guard) = tempdir();
        let out = RunShell
            .call(json!({ "command": "echo hello world | grep world" }))
            .await
            .unwrap();
        assert!(out.text.contains("world"), "{}", out.text);
        drop(dir);
    }

    #[tokio::test]
    async fn list_dir_sorted() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("zdir").unwrap();
        std::fs::write("afile", "").unwrap();
        let out = ListDir { gate: gate() }.call(json!({})).await.unwrap();
        let lines: Vec<&str> = out.text.lines().collect();
        assert_eq!(lines, vec!["afile", "zdir/"]);
        drop(dir);
    }

    #[tokio::test]
    async fn grep_finds_and_caps() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep { gate: gate() }
            .call(json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        assert!(out.text.contains("r.rs:1"));
        assert!(!out.text.contains("r.txt"));
        let none = Grep { gate: gate() }
            .call(json!({ "pattern": "zzzz" }))
            .await
            .unwrap();
        assert!(none.text.contains("no matches"));
        drop(dir);
    }

    #[test]
    fn diff_computes_line_numbers_and_ellipsis() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let new = "one\ntwo\nTHREE\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Remove));
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Add));
        let remove = diff
            .iter()
            .find(|l| l.kind == DiffLineKind::Remove)
            .unwrap();
        assert_eq!(remove.old_line, Some(3));
        assert_eq!(remove.text, "three\n");
        let add = diff.iter().find(|l| l.kind == DiffLineKind::Add).unwrap();
        assert_eq!(add.new_line, Some(3));
        assert_eq!(add.text, "THREE\n");
    }

    #[test]
    fn diff_inserts_ellipsis_between_hunks() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
        let new = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
        let diff = compute_diff(old, new);
        assert!(diff.iter().any(|l| l.kind == DiffLineKind::Ellipsis));
    }

    #[test]
    fn resolve_checked_classifies_hidden_and_outside() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".git").unwrap();
        std::fs::write(".git/config", "x").unwrap();
        let (_, reason) = resolve_checked(".git/config").unwrap();
        assert_eq!(reason, Some(ApprovalReason::HiddenPath));
        let (_, reason) = resolve_checked(".").unwrap();
        assert_eq!(reason, None);

        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        let (_, reason) = resolve_checked(&rel).unwrap();
        assert_eq!(reason, Some(ApprovalReason::OutsideWorkspace));
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }
}
