use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolDefinition};

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;

fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn args_str_vec(args: &Value, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .ok_or_else(|| format!("missing string-array argument '{key}'"))
}

struct ReadFile;

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

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            let abs = resolve(&path)?;
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a file"));
            }
            let data = std::fs::read(&abs).map_err(|e| format!("read {path}: {e}"))?;
            if data.contains(&0) {
                return Err(format!("'{path}' appears to be binary; refusing to read"));
            }
            let content = String::from_utf8_lossy(&data[..data.len().min(MAX_READ_BYTES)]);
            let lines: Vec<&str> = content.lines().collect();
            let start = offset.unwrap_or(1).max(1) as usize - 1;
            let end = match limit {
                Some(n) => (start + n as usize).min(lines.len()),
                None => lines.len(),
            };
            if start >= lines.len() {
                return Ok(String::from("(empty)"));
            }
            let mut out = String::new();
            for (i, line) in lines[start..end].iter().enumerate() {
                out.push_str(&format!("{:>6} | {line}\n", start + i + 1));
            }
            Ok(out)
        })
    }
}

struct WriteFile;

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

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let content = arg_value(&args, "content")?;
            let abs = resolve_for_write(&path)?;
            if let Some(parent) = abs.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
            }
            std::fs::write(&abs, &content).map_err(|e| format!("write {path}: {e}"))?;
            Ok(format!("wrote {} bytes to {path}", content.len()))
        })
    }
}

struct EditFile;

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

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let path = arg_value(&args, "path")?;
            let old = arg_value(&args, "old")?;
            let new = arg_value(&args, "new")?;
            let occurrence = args.get("occurrence").and_then(Value::as_u64);
            let abs = resolve(&path)?;
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
            std::fs::write(&abs, edited).map_err(|e| format!("write {path}: {e}"))?;
            Ok(format!(
                "edited {path}: replaced 1 of {} occurrences",
                matches.len()
            ))
        })
    }
}

struct RunCommand;

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_command".into(),
            description: "Run a shell command in the workspace. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command is killed when it exceeds the timeout."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "description": "Executable or script to run" },
                    "args": { "type": "array", "items": { "type": "string" }, "description": "Command-line arguments" },
                    "cwd": { "type": "string", "description": "Working directory, relative to the workspace root. Defaults to the workspace root" },
                    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Timeout in seconds (default 30)" }
                },
                "required": ["cmd"]
            }),
        }
    }

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let cmd = arg_value(&args, "cmd")?;
            let cmd_args = args_str_vec(&args, "args").unwrap_or_default();
            let cwd = args.get("cwd").and_then(Value::as_str);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS);
            let cwd_abs = match cwd {
                Some(c) => resolve(c)?,
                None => workspace_root()?,
            };
            let mut child = tokio::process::Command::new(&cmd)
                .args(&cmd_args)
                .current_dir(&cwd_abs)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| format!("spawn {cmd}: {e}"))?;
            let status = match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                child.wait(),
            )
            .await
            {
                Ok(Ok(status)) => status,
                Ok(Err(e)) => return Err(format!("wait {cmd}: {e}")),
                Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(format!("{cmd} timed out after {timeout_secs}s (killed)"));
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
                return Err(format!("{cmd} exited with {status}:\n{capped}"));
            }
            if trimmed.is_empty() {
                Ok(format!("{cmd} exited with {status} (no output)"))
            } else {
                Ok(format!("exit {status}:\n{capped}"))
            }
        })
    }
}

struct ListDir;

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

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let abs = resolve(path)?;
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
                Ok(format!("{path} is empty"))
            } else {
                Ok(names.join("\n"))
            }
        })
    }
}

struct Grep;

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

    fn call(&self, args: Value) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> {
        Box::pin(async move {
            let pattern = arg_value(&args, "pattern")?;
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let include = args.get("include").and_then(Value::as_str);
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let regex = regex::Regex::new(&pattern).map_err(|e| format!("bad pattern: {e}"))?;
            let abs = resolve(path)?;
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
                Ok(format!("no matches for /{pattern}/ in {path}"))
            } else if hits >= max {
                out.push_str(&format!("… ({} results, cap {max})", hits + 1));
                Ok(out)
            } else {
                Ok(out)
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

fn resolve_for_write(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(path);
    if joined.exists() {
        let abs = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
        return if abs.starts_with(&root) {
            Ok(abs)
        } else {
            Err(format!("{path} resolves outside the workspace"))
        };
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
    if !abs.starts_with(&root) {
        return Err(format!("{path} resolves outside the workspace"));
    }
    Ok(abs)
}

pub fn all_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile),
        std::sync::Arc::new(WriteFile),
        std::sync::Arc::new(EditFile),
        std::sync::Arc::new(RunCommand),
        std::sync::Arc::new(ListDir),
        std::sync::Arc::new(Grep),
    ]
}

pub fn read_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile),
        std::sync::Arc::new(ListDir),
        std::sync::Arc::new(Grep),
    ]
}

pub fn command_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![std::sync::Arc::new(RunCommand)]
}

pub fn edit_tools() -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(ReadFile),
        std::sync::Arc::new(WriteFile),
        std::sync::Arc::new(EditFile),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};
    use tempfile::TempDir;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    fn tempdir() -> (TempDir, MutexGuard<'static, ()>) {
        let guard = CWD_LOCK.lock().unwrap();
        let dir = TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    #[tokio::test]
    async fn read_file_with_range() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\nthree\n").unwrap();
        let out = ReadFile
            .call(json!({ "path": "a.txt", "offset": 2, "limit": 1 }))
            .await
            .unwrap();
        assert!(out.contains("two"), "{out}");
        let err = ReadFile
            .call(json!({ "path": "missing.txt" }))
            .await
            .unwrap_err();
        assert!(err.contains("missing.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_refuses_binary_and_escape() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", [0, 1, 2, 3]).unwrap();
        let err = ReadFile
            .call(json!({ "path": "bin.dat" }))
            .await
            .unwrap_err();
        assert!(err.contains("binary"));
        let err = ReadFile
            .call(json!({ "path": "../outside.txt" }))
            .await
            .unwrap_err();
        assert!(err.contains("outside"));
        drop(dir);
    }

    #[tokio::test]
    async fn write_creates_parents() {
        let (dir, _guard) = tempdir();
        WriteFile
            .call(json!({ "path": "sub/deep/f.txt", "content": "hello" }))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("sub/deep/f.txt").unwrap(), "hello");
        drop(dir);
    }

    #[tokio::test]
    async fn edit_replaces_and_detects_ambiguity() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "a b a").unwrap();
        let err = EditFile
            .call(json!({ "path": "e.txt", "old": "a", "new": "x" }))
            .await
            .unwrap_err();
        assert!(err.contains("occurrence"));
        EditFile
            .call(json!({ "path": "e.txt", "old": "a", "new": "x", "occurrence": 2 }))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "a b x");
        let err = EditFile
            .call(json!({ "path": "e.txt", "old": "zzz", "new": "x" }))
            .await
            .unwrap_err();
        assert!(err.contains("not found"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_command_success_and_timeout() {
        let (dir, _guard) = tempdir();
        let out = RunCommand
            .call(json!({ "cmd": "sh", "args": ["-c", "echo hello"] }))
            .await
            .unwrap();
        assert!(out.contains("hello"));
        let err = RunCommand
            .call(json!({ "cmd": "sleep", "args": ["5"], "timeout_secs": 1 }))
            .await
            .unwrap_err();
        assert!(err.contains("timed out"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_command_failure_returns_error() {
        let (dir, _guard) = tempdir();
        let err = RunCommand
            .call(json!({ "cmd": "sh", "args": ["-c", "exit 3"] }))
            .await
            .unwrap_err();
        assert!(err.contains("3"));
        drop(dir);
    }

    #[tokio::test]
    async fn list_dir_sorted() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("zdir").unwrap();
        std::fs::write("afile", "").unwrap();
        let out = ListDir.call(json!({})).await.unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines, vec!["afile", "zdir/"]);
        drop(dir);
    }

    #[tokio::test]
    async fn grep_finds_and_caps() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep
            .call(json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        assert!(out.contains("r.rs:1"));
        assert!(!out.contains("r.txt"));
        let none = Grep.call(json!({ "pattern": "zzzz" })).await.unwrap();
        assert!(none.contains("no matches"));
        drop(dir);
    }
}
