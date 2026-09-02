use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shuvarie_llm::{
    DiffLine, DiffLineKind, FileChange, ShellStreams, Tool, ToolContext, ToolExecutionError,
    ToolOutput,
};

use crate::lsp_manager::SharedManager;
use crate::permissions::{resolve_read, resolve_write};
use crate::question::{QuestionGate, QuestionOption, QuestionPrompt};

const MAX_READ_BYTES: usize = 64 * 1024;
const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const BINARY_SAMPLE_BYTES: usize = 4096;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const SHELL_CAPTURE_BYTES: usize = 64 * 1024;
const SHELL_STREAM_TAIL_CHARS: usize = 2048;
const SHELL_STREAM_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct ShellChunk {
    pub worker: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

/// Bounded channel sender carrying live `run_shell` output to the core task.
/// Each clone is tagged with the owning worker so the TUI can attach the
/// streamed output to the right tool activity entry.
#[derive(Clone)]
pub struct ShellOutputTx {
    tx: tokio::sync::mpsc::Sender<ShellChunk>,
    worker: Option<String>,
}

impl ShellOutputTx {
    pub fn new(tx: tokio::sync::mpsc::Sender<ShellChunk>) -> Self {
        Self { tx, worker: None }
    }

    pub fn tagged(&self, worker: &str) -> Self {
        Self {
            tx: self.tx.clone(),
            worker: Some(worker.to_string()),
        }
    }

    async fn send_streams(&self, stdout: &[u8], stderr: &[u8]) {
        let _ = self
            .tx
            .send(ShellChunk {
                worker: self.worker.clone(),
                stdout: stream_tail(stdout),
                stderr: stream_tail(stderr),
            })
            .await;
    }
}

fn stream_tail(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .rev()
        .take(SHELL_STREAM_TAIL_CHARS)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn display_stream(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(MAX_COMMAND_OUTPUT)
        .collect()
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {}

/// Kills the shell's process group if the tool future is cancelled mid-run
/// (turn abort / stream cancel), so the child cannot outlive the request.
struct KillGuard(Option<u32>);

impl KillGuard {
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_process_group(pid);
        }
    }
}

type ReadKey = (String, Option<u64>, Option<u64>);

/// Per-turn dedupe cache for `read_file`: tracks `(path, offset, limit)` keys
/// that have already been returned to the model this turn, plus a set of all
/// paths read this turn (any range) used by `write_file`'s read-first check
/// for overwrites. Repeated identical reads get a short note instead of
/// re-sending file content, which keeps the agent loop from blowing up the
/// context by re-reading the same large file.
#[derive(Clone, Default)]
pub struct ReadCache {
    seen: Arc<Mutex<std::collections::HashSet<ReadKey>>>,
    read_paths: Arc<Mutex<std::collections::HashSet<String>>>,
}

impl ReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns `true` if this exact `(path, offset, limit)` was already read
    /// this turn, otherwise records it and returns `false`.
    fn mark(&self, path: &str, offset: Option<u64>, limit: Option<u64>) -> bool {
        let key = (path.to_string(), offset, limit);
        let mut seen = self.seen.lock().unwrap();
        let fresh = seen.insert(key);
        if fresh {
            self.read_paths.lock().unwrap().insert(path.to_string());
        }
        !fresh
    }

    /// Returns `true` if the path was read (any range) earlier this turn.
    pub(crate) fn was_read(&self, path: &str) -> bool {
        self.read_paths.lock().unwrap().contains(path)
    }
}

/// Per-target-file mutation locks shared by `write_file`, `edit_file`, and
/// `apply_patch`, so worker agents and the manager never interleave
/// read-modify-write spans on the same file. Different files stay parallel.
#[derive(Clone, Default)]
pub struct FileLocks {
    locks: Arc<tokio::sync::Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl FileLocks {
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) async fn lock(&self, path: &Path) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = self
            .locks
            .lock()
            .await
            .entry(path.to_owned())
            .or_default()
            .clone();
        entry.lock_owned().await
    }
}

pub(crate) fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

struct ReadFile {
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
}

impl Tool for ReadFile {
    const NAME: &'static str = "read_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Read a text file from the workspace, optionally restricted to a line range. Returns the requested lines or an error when the path does not exist, is a directory, or contains binary data."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the file, relative to the workspace root" },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Defaults to 1" },
                "limit": { "type": "integer", "minimum": 1, "description": format!("Maximum number of lines to read. Defaults to {DEFAULT_READ_LIMIT}") }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let read_cache = self.read_cache.clone();
        let max_output_chars = self.max_output_chars;
        let max_output_bytes = self.max_output_bytes;
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            if read_cache.mark(&path, offset, limit) {
                return Ok(ToolOutput::text(format!(
                    "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                )));
            }
            let abs = resolve_read(&path)?;
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a file"));
            }
            let data = tokio::fs::read(&abs).await.map_err(|e| format!("read {path}: {e}"))?;
            if is_binary_file(&data) {
                return Err(format!("'{path}' appears to be binary; refusing to read"));
            }
            let content_owned = String::from_utf8_lossy(&data).into_owned();
            let lines: Vec<&str> = content_owned.lines().collect();
            let offset = offset.unwrap_or(1).max(1) as usize;
            if offset > lines.len() && !(offset == 1 && lines.is_empty()) {
                return Err(format!(
                    "Offset {offset} is out of range for this file ({} lines)",
                    lines.len()
                ));
            }
            let limit = limit.map(|n| n as usize).unwrap_or(DEFAULT_READ_LIMIT);
            let start = offset - 1;
            let end = (start + limit).min(lines.len());
            let mut out = String::new();
            let mut long_lines = false;
            for (i, line) in lines[start..end].iter().enumerate() {
                let line = if line.chars().count() > MAX_LINE_LENGTH {
                    long_lines = true;
                    let head: String = line.chars().take(MAX_LINE_LENGTH).collect();
                    format!("{head}{MAX_LINE_SUFFIX}")
                } else {
                    line.to_string()
                };
                out.push_str(&format!("{:>6} | {line}\n", start + i + 1));
            }
            let last = start + (end - start);
            let truncated = limit < lines.len() - start;
            if truncated {
                out.push_str(&format!(
                    "\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
                    offset,
                    last,
                    lines.len(),
                    last + 1
                ));
            } else {
                out.push_str(&format!("\n(End of file - total {} lines)", lines.len()));
            }
            if long_lines {
                out.push_str(
                    "\n(long lines truncated; use run_shell, e.g. `sed -n 'Np' file | cut -c1-2000`, to read one exactly)",
                );
            }
            let hint = format!("use offset/limit to read more of {path}");
            if let Some(capped) = crate::truncate::truncate_output(&out, max_output_chars, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            if let Some(capped) = crate::truncate::truncate_bytes(&out, max_output_bytes, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            Ok(ToolOutput::text(out))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

fn is_binary_file(data: &[u8]) -> bool {
    if data.contains(&0) {
        return true;
    }
    let sample = &data[..data.len().min(BINARY_SAMPLE_BYTES)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();
    non_printable * 10 > sample.len() * 3
}

struct WriteFile {
    read_cache: ReadCache,
    lsp: Option<SharedManager>,
    locks: FileLocks,
}

const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

impl Tool for WriteFile {
    const NAME: &'static str = "write_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Create or overwrite a file in the working directory, creating parent directories as needed. \
         Passing mode=\"create\" fails when the file already exists; mode=\"overwrite\" requires reading \
         the file with read_file first. When mode is omitted, new files are created and existing files \
         are overwritten (also requiring a prior read)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to write" },
                "content": { "type": "string", "description": "Full new contents of the file" },
                "mode": { "type": "string", "enum": ["create", "overwrite"], "description": "Explicit write mode: 'create' fails if the file exists; 'overwrite' replaces an existing file (must be read first). Omit to auto-detect" }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let read_cache = self.read_cache.clone();
        let lsp = self.lsp.clone();
        let locks = self.locks.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let content = arg_value(&args, "content")?;
            let mode = args.get("mode").and_then(Value::as_str);
            match mode {
                Some("create") | Some("overwrite") | None => {}
                Some(other) => return Err(format!("invalid mode '{other}' (expected 'create' or 'overwrite')")),
            }
            let abs = resolve_write(&path)?;
            let _file_lock = locks.lock(&abs).await;
            let exists = abs.exists();
            match mode {
                Some("create") if exists => {
                    return Err(format!(
                        "'{path}' already exists; use mode 'overwrite' (after reading it) to replace it"
                    ));
                }
                Some("overwrite") | None if exists && !read_cache.was_read(&path) => {
                    return Err(format!(
                        "'{path}' exists but was not read this turn; read it with read_file before overwriting, or pass mode 'create' for a new file"
                    ));
                }
                _ => {}
            }
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
            }
            let original = tokio::fs::read(&abs).await.ok();
            let original_text = original
                .as_deref()
                .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
            let had_bom = original.as_deref().is_some_and(|b| b.starts_with(UTF8_BOM));
            let content_bare = content.trim_start_matches('\u{FEFF}');
            let content = if had_bom || content_bare.len() != content.len() {
                format!("\u{FEFF}{content_bare}")
            } else {
                content_bare.to_string()
            };
            let tmp_path = abs.with_extension(format!(
                "{}tmp",
                abs.extension()
                    .map(|e| format!("{e}.", e = e.to_string_lossy()))
                    .unwrap_or_default()
            ));
            tokio::fs::write(&tmp_path, &content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            if let Err(e) = tokio::fs::rename(&tmp_path, &abs).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(format!("write {path}: {e}"));
            }
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &content)
                    .await;
            }
            let summary = format!("wrote {} bytes to {path}", content.len());
            ctx.insert_result(FileChange::Write {
                path,
                content,
                original: original_text,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct EditFile {
    lsp: Option<SharedManager>,
    locks: FileLocks,
}

impl Tool for EditFile {
    const NAME: &'static str = "edit_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, \
         non-overlapping region of the original file; all oldTexts are matched against the file as it \
         was before the call, not after earlier edits. If two changes affect the same block or nearby \
         lines, merge them into one edit instead of emitting overlapping edits. Do not include large \
         unchanged regions just to connect distant changes."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to edit" },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement; must be unique in the original file" },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit" }
                        },
                        "required": ["oldText", "newText"]
                    },
                    "description": "One or more targeted replacements, each matched against the original file. Do not include overlapping or nested edits."
                }
            },
            "required": ["path", "edits"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let lsp = self.lsp.clone();
        let locks = self.locks.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let edits = parse_edits(&args)?;
            let abs = resolve_read(&path)?;
            let _file_lock = locks.lock(&abs).await;
            let raw = tokio::fs::read_to_string(&abs)
                .await
                .map_err(|e| format!("read {path}: {e}"))?;
            let (had_bom, content) = split_bom(&raw);
            let ending = detect_line_ending(content);
            let base = normalize_lf(content);
            let edited = apply_edits(&base, &edits, &path)?;
            let mut final_content = String::with_capacity(raw.len() + 16);
            if had_bom {
                final_content.push('\u{FEFF}');
            }
            final_content.push_str(&restore_line_endings(&edited, ending));
            let diff = compute_diff(&raw, &final_content);
            tokio::fs::write(&abs, &final_content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &final_content)
                    .await;
            }
            let summary = match edits.len() {
                1 => format!("edited {path}: 1 edit applied"),
                n => format!("edited {path}: {n} edits applied"),
            };
            ctx.insert_result(FileChange::Edit {
                path,
                diff,
                original: raw,
                new: final_content,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct RunShell {
    shell_tx: ShellOutputTx,
}

impl Tool for RunShell {
    const NAME: &'static str = "run_shell";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        #[cfg(unix)]
        const DESCRIPTION: &str = "Run a shell command line in the workspace, executed through the system's Bourne shell (`sh -c`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`.";
        #[cfg(windows)]
        const DESCRIPTION: &str = "Run a shell command line in the workspace, executed through the system's PowerShell (`powershell -NoProfile -Command`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`.";

        DESCRIPTION.to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command line to run" },
                "cwd": { "type": "string", "description": "Working directory, relative to the workspace root. Defaults to the workspace root. Use this instead of 'cd' commands" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Timeout in seconds (default 30). The command and its children are killed when it expires" }
            },
            "required": ["command"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let command = arg_value(&args, "command")?;
            let cwd = args.get("cwd").and_then(Value::as_str);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS);
            let cwd_abs = match cwd {
                Some(c) => {
                    let root = workspace_root()?;
                    let resolved = root
                        .join(c)
                        .canonicalize()
                        .map_err(|e| format!("cwd '{c}': {e}"))?;
                    if !resolved.is_dir() {
                        return Err(format!("cwd '{c}' is not a directory"));
                    }
                    resolved
                }
                None => workspace_root()?,
            };
            let mut builder = tokio::process::Command::new(shell_bin());
            shell_args(&mut builder, &command);
            builder
                .current_dir(&cwd_abs)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .stdin(std::process::Stdio::null());
            #[cfg(unix)]
            builder.process_group(0);
            let mut child = builder.spawn().map_err(|e| format!("spawn shell: {e}"))?;
            let pgid = child.id();
            let mut guard = KillGuard(pgid);

            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<(bool, Vec<u8>)>(64);
            let mut readers = Vec::new();
            if let Some(pipe) = child.stdout.take() {
                readers.push(spawn_pipe_reader(pipe, chunk_tx.clone(), false));
            }
            if let Some(pipe) = child.stderr.take() {
                readers.push(spawn_pipe_reader(pipe, chunk_tx.clone(), true));
            }
            drop(chunk_tx);

            let mut captured: Vec<u8> = Vec::new();
            let mut out: Vec<u8> = Vec::new();
            let mut err: Vec<u8> = Vec::new();
            let mut last_emit = std::time::Instant::now() - std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
            let interval = std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
            let status = loop {
                tokio::select! {
                    maybe_chunk = chunk_rx.recv() => {
                        match maybe_chunk {
                            Some((is_stderr, chunk)) => {
                                captured.extend_from_slice(&chunk);
                                cap_buffer(&mut captured);
                                if is_stderr {
                                    err.extend_from_slice(&chunk);
                                    cap_buffer(&mut err);
                                } else {
                                    out.extend_from_slice(&chunk);
                                    cap_buffer(&mut out);
                                }
                                if last_emit.elapsed() >= interval {
                                    last_emit = std::time::Instant::now();
                                    self.shell_tx.send_streams(&out, &err).await;
                                }
                            }
                            None => {
                                let status = child
                                    .wait()
                                    .await
                                    .map_err(|e| format!("wait shell: {e}"))?;
                                break status;
                            }
                        }
                    }
                    status = child.wait() => {
                        // Drain remaining output so it is not lost with the pipes.
                        while let Some((is_stderr, chunk)) = chunk_rx.recv().await {
                            captured.extend_from_slice(&chunk);
                            cap_buffer(&mut captured);
                            if is_stderr {
                                err.extend_from_slice(&chunk);
                                cap_buffer(&mut err);
                            } else {
                                out.extend_from_slice(&chunk);
                                cap_buffer(&mut out);
                            }
                        }
                        let status = status.map_err(|e| format!("wait shell: {e}"))?;
                        break status;
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        if let Some(pgid) = pgid {
                            kill_process_group(pgid);
                        }
                        let _ = child.kill().await;
                        let _ = child.wait().await;
                        self.shell_tx.send_streams(&out, &err).await;
                        let partial = String::from_utf8_lossy(&captured).trim().to_string();
                        let mut message = format!(
                            "shell command timed out after {timeout_secs}s (killed)"
                        );
                        if !partial.is_empty() {
                            message.push_str(&format!("\n{partial}"));
                        }
                        message.push_str(&format!(
                            "\n\n<shell_metadata>\nshell tool terminated the command after exceeding the {timeout_secs}s timeout. If this command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value.\n</shell_metadata>"
                        ));
                        let stdout =
                            format!("timeout {timeout_secs}s:\n{}", display_stream(&out));
                        ctx.insert_result(ShellStreams {
                            stdout,
                            stderr: display_stream(&err),
                        });
                        return Err(message);
                    }
                }
            };

            for reader in readers {
                let _ = reader.await;
            }
            guard.disarm();

            let trimmed = String::from_utf8_lossy(&captured)
                .trim_end()
                .to_string();
            let trimmed = trimmed.trim();
            let capped: String = trimmed.chars().take(MAX_COMMAND_OUTPUT).collect();
            let status_line = format!("exit {status}:");
            let stdout = format!("{status_line}\n{}", display_stream(&out));
            ctx.insert_result(ShellStreams {
                stdout,
                stderr: display_stream(&err),
            });
            if !status.success() {
                return Err(format!("shell exited with {status}:\n{capped}"));
            }
            if trimmed.is_empty() {
                Ok(ToolOutput::text(format!(
                    "shell exited with {status} (no output)"
                )))
            } else {
                Ok(ToolOutput::text(format!("{status_line}\n{capped}")))
            }
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

#[cfg(unix)]
#[inline]
fn shell_bin() -> &'static str {
    "sh"
}

fn spawn_pipe_reader<R>(
    mut pipe: R,
    tx: tokio::sync::mpsc::Sender<(bool, Vec<u8>)>,
    is_stderr: bool,
) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send((is_stderr, buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    })
}

fn cap_buffer(buf: &mut Vec<u8>) {
    if buf.len() > SHELL_CAPTURE_BYTES {
        let drop = buf.len() - SHELL_CAPTURE_BYTES;
        buf.drain(..drop);
    }
}

#[cfg(unix)]
#[inline]
fn shell_args(cmd: &mut tokio::process::Command, command: &str) {
    cmd.arg("-c").arg(command);
}

#[cfg(windows)]
#[inline]
fn shell_bin() -> &'static str {
    "powershell"
}

#[cfg(windows)]
#[inline]
fn shell_args(cmd: &mut tokio::process::Command, command: &str) {
    cmd.arg("-NoProfile").arg("-Command").arg(command);
}

const WEBFETCH_MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const WEBFETCH_MAX_TIMEOUT_SECS: u64 = 120;
const WEBFETCH_BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/143.0.0.0 Safari/537.36";

fn webfetch_accept_header(format: &str) -> &'static str {
    match format {
        "text" => "text/plain;q=1.0, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        "html" => {
            "text/html;q=1.0, application/xhtml+xml;q=0.9, text/plain;q=0.8, text/markdown;q=0.7, */*;q=0.1"
        }
        _ => {
            "text/markdown;q=1.0, text/x-markdown;q=0.9, text/plain;q=0.8, text/html;q=0.7, */*;q=0.1"
        }
    }
}

struct WebFetch {
    max_output_chars: usize,
}

impl Tool for WebFetch {
    const NAME: &'static str = "webfetch";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Fetch content from a specified URL and return it as text or markdown. \
         HTML pages are converted to the requested format (markdown by default); \
         non-HTML content (plain text, JSON, XML, source code) is returned as-is. \
         Use this to retrieve web pages, API responses, and online documentation."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "The URL to fetch content from" },
                "format": { "type": "string", "enum": ["text", "markdown", "html"], "description": "The format to return the content in. Defaults to markdown" },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Optional timeout in seconds (max 120, default 30)" }
            },
            "required": ["url"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let url = arg_value(&args, "url")?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err("URL must start with http:// or https://".to_string());
            }
            let format = args
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("markdown")
                .to_string();
            if !matches!(format.as_str(), "text" | "markdown" | "html") {
                return Err(format!(
                    "invalid format '{format}' (expected text, markdown, or html)"
                ));
            }
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, WEBFETCH_MAX_TIMEOUT_SECS);
            let timeout = std::time::Duration::from_secs(timeout_secs);

            let client = reqwest::Client::builder()
                .user_agent(WEBFETCH_BROWSER_UA)
                .connect_timeout(timeout)
                .timeout(timeout)
                .build()
                .map_err(|e| format!("build http client: {e}"))?;
            let response = client
                .get(&url)
                .header("Accept", webfetch_accept_header(&format))
                .header("Accept-Language", "en-US,en;q=0.9")
                .send()
                .await
                .map_err(|e| format!("request {url}: {e}"))?;

            let status = response.status();
            if status.as_u16() == 403
                && response
                    .headers()
                    .get("cf-mitigated")
                    .and_then(|v| v.to_str().ok())
                    == Some("challenge")
            {
                let retry = client
                    .get(&url)
                    .header("Accept", webfetch_accept_header(&format))
                    .header("User-Agent", "shuvarie")
                    .send()
                    .await
                    .map_err(|e| format!("request {url}: {e}"))?;
                return webfetch_finish(retry, &url, &format, max_output_chars).await;
            }
            if status.is_client_error() || status.is_server_error() {
                return Err(format!("request {url}: HTTP {status}"));
            }
            webfetch_finish(response, &url, &format, max_output_chars).await
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

async fn webfetch_finish(
    response: reqwest::Response,
    url: &str,
    format: &str,
    max_output_chars: usize,
) -> Result<ToolOutput, String> {
    if let Some(len) = response.content_length()
        && len as usize > WEBFETCH_MAX_RESPONSE_BYTES
    {
        return Err(format!(
            "response too large ({len} bytes, exceeds 5 MB limit)"
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();

    let mut body = Vec::new();
    let mut stream = response;
    while let Some(chunk) = stream
        .chunk()
        .await
        .map_err(|e| format!("read {url}: {e}"))?
    {
        if body.len() + chunk.len() > WEBFETCH_MAX_RESPONSE_BYTES {
            return Err(format!(
                "response too large (exceeds 5 MB limit) while streaming {url}"
            ));
        }
        body.extend_from_slice(&chunk);
    }

    if mime.starts_with("image/") {
        return Err(format!(
            "{url} returned an image ({mime}); webfetch can only return text content"
        ));
    }

    let looks_html = webfetch_looks_html(&mime, url);
    let text = String::from_utf8_lossy(&body).into_owned();
    let out = if looks_html {
        match format {
            "html" => text,
            _ => webfetch_convert(&text, format == "text"),
        }
    } else {
        text
    };

    let out = out.trim();
    if out.is_empty() {
        return Ok(ToolOutput::text(
            "(empty response — the page may require JavaScript to render)",
        ));
    }

    let body = format!("{url} ({content_type})\n\n{out}");
    if let Some(capped) =
        crate::truncate::truncate_output(&body, max_output_chars, "fetch a narrower URL if needed")
    {
        return Ok(ToolOutput::text(capped));
    }
    Ok(ToolOutput::text(body))
}

fn webfetch_looks_html(mime: &str, url: &str) -> bool {
    if mime.contains("text/html") || mime.contains("application/xhtml") {
        return true;
    }
    if !mime.is_empty() {
        return false;
    }
    url.split(['?', '#'])
        .next()
        .and_then(|path| path.rsplit('.').next())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml"))
}

fn webfetch_convert(html: &str, plain: bool) -> String {
    let options = html_to_markdown_rs::ConversionOptions::builder()
        .bullets("-".to_string())
        .skip_images(true)
        .extract_metadata(false)
        .extract_images(false)
        .compact_tables(true)
        .include_document_structure(false)
        .output_format(if plain {
            html_to_markdown_rs::OutputFormat::Plain
        } else {
            html_to_markdown_rs::OutputFormat::Markdown
        })
        .build();
    html_to_markdown_rs::convert(html, options)
        .map(|result| result.content.unwrap_or_default())
        .unwrap_or_else(|e| format!("(html conversion failed: {e})\n\n{html}"))
}

struct ListDir {}

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

struct Grep {}

impl Tool for Grep {
    const NAME: &'static str = "grep";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Recursively search text files for a regular expression, printing `path:line: content`. Results are capped (default 200)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression to search for" },
                "path": { "type": "string", "description": "Directory or file to search, relative to the workspace root (defaults to the workspace root)" },
                "include": { "type": "string", "description": "Only search files whose name contains this string (e.g. '.rs')" },
                "max_results": { "type": "integer", "minimum": 1, "description": "Maximum number of matches (default 200)" }
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
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let include = args.get("include").and_then(Value::as_str);
            let max = args
                .get("max_results")
                .and_then(Value::as_u64)
                .unwrap_or(200) as usize;
            let regex = regex::Regex::new(&pattern).map_err(|e| format!("bad pattern: {e}"))?;
            let abs = resolve_read(path)?;
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
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

const GLOB_MAX_RESULTS: usize = 100;

struct Glob {}

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
            let pattern = arg_value(&args, "pattern")?;
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let abs = resolve_read(path)?;
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

struct TextEdit {
    old_text: String,
    new_text: String,
}

struct MatchedEdit {
    index: usize,
    start: usize,
    len: usize,
    new_text: String,
}

fn edit_from_value(value: &Value) -> Option<TextEdit> {
    let old = value
        .get("oldText")
        .or_else(|| value.get("old"))
        .and_then(Value::as_str)?;
    let new = value
        .get("newText")
        .or_else(|| value.get("new"))
        .and_then(Value::as_str)?;
    Some(TextEdit {
        old_text: old.to_string(),
        new_text: new.to_string(),
    })
}

fn parse_edits(args: &Value) -> Result<Vec<TextEdit>, String> {
    let mut edits: Vec<TextEdit> = Vec::new();
    match args.get("edits") {
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                edits.push(edit_from_value(item).ok_or_else(|| {
                    format!("edits[{i}] must be an object with string oldText and newText")
                })?);
            }
        }
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Array(items)) => {
                return parse_edits(&json!({ "edits": items }));
            }
            Ok(single) => {
                return edit_from_value(&single)
                    .map(|edit| vec![edit])
                    .ok_or_else(|| {
                        "'edits' string did not contain string oldText/newText".to_string()
                    });
            }
            _ => return Err("'edits' string did not parse as a JSON array".into()),
        },
        Some(single @ Value::Object(_)) => {
            edits
                .push(edit_from_value(single).ok_or_else(|| {
                    "'edits' object needs string oldText and newText".to_string()
                })?);
        }
        _ => {}
    }
    if edits.is_empty()
        && let Some(edit) = edit_from_value(args)
    {
        edits.push(edit);
    }
    if edits.is_empty() {
        return Err("edit_file requires at least one targeted replacement in 'edits'".into());
    }
    Ok(edits)
}

fn split_bom(raw: &str) -> (bool, &str) {
    match raw.strip_prefix('\u{FEFF}') {
        Some(rest) => (true, rest),
        None => (false, raw),
    }
}

fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

fn normalize_lf(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

fn apply_edits(base: &str, edits: &[TextEdit], path: &str) -> Result<String, String> {
    let single = edits.len() == 1;
    for (i, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(match single {
                true => "oldText must not be empty.".into(),
                false => format!("edits[{i}].oldText must not be empty."),
            });
        }
    }
    let mut used_fuzzy = false;
    for edit in edits {
        let (_, _, fuzzy) = fuzzy_find(base, &edit.old_text);
        if fuzzy {
            used_fuzzy = true;
        }
    }
    let replacement_base = if used_fuzzy {
        normalize_for_fuzzy_match(base)
    } else {
        base.to_string()
    };
    let mut matched: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let (index, len, _) = fuzzy_find(&replacement_base, &edit.old_text);
        let occurrences = count_fuzzy_occurrences(&replacement_base, &edit.old_text);
        match (index, occurrences) {
            (None, _) => {
                return Err(match single {
                    true => format!(
                        "Could not find the text in {path}. It must match the file content exactly, including all whitespace and newlines."
                    ),
                    false => format!(
                        "edits[{i}]: could not find the text in {path}. It must match the file content exactly, including all whitespace and newlines."
                    ),
                });
            }
            (Some(start), 1) => matched.push(MatchedEdit {
                index: i,
                start,
                len,
                new_text: edit.new_text.clone(),
            }),
            (Some(_), n) => {
                return Err(match single {
                    true => format!(
                        "Found {n} occurrences of the text in {path}. The text must be unique; include more surrounding lines to disambiguate."
                    ),
                    false => format!(
                        "Found {n} occurrences of edits[{i}].oldText in {path}. Each oldText must be unique; include more surrounding lines to disambiguate."
                    ),
                });
            }
        }
    }
    matched.sort_by_key(|m| m.start);
    for pair in matched.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.start + previous.len > current.start {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.index, current.index
            ));
        }
    }
    let result = if used_fuzzy {
        apply_replacements_preserving_unchanged_lines(base, &replacement_base, &matched)?
    } else {
        let mut result = base.to_string();
        for m in matched.iter().rev() {
            result.replace_range(m.start..m.start + m.len, &m.new_text);
        }
        result
    };
    if result == base {
        return Err(match single {
            true => format!(
                "No changes made to {path}. The replacement produced identical content; check for special characters or a mistaken match."
            ),
            false => {
                format!("No changes made to {path}. The replacements produced identical content.")
            }
        });
    }
    Ok(result)
}

fn fuzzy_find(content: &str, old: &str) -> (Option<usize>, usize, bool) {
    if let Some(index) = content.find(old) {
        return (Some(index), old.len(), false);
    }
    let normalized_content = normalize_for_fuzzy_match(content);
    let normalized_old = normalize_for_fuzzy_match(old);
    if let Some(index) = normalized_content.find(&normalized_old) {
        return (Some(index), normalized_old.len(), true);
    }
    (None, 0, false)
}

fn count_fuzzy_occurrences(content: &str, old: &str) -> usize {
    normalize_for_fuzzy_match(content)
        .matches(&normalize_for_fuzzy_match(old))
        .count()
}

fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = text.chars().nfkc().collect();
    let mut out = String::with_capacity(nfkc.len());
    for (i, line) in nfkc.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out.replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
        .replace(
            [
                '\u{00A0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
            ],
            " ",
        )
}

fn lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, _) in content.match_indices('\n') {
        lines.push(&content[start..idx + 1]);
        start = idx + 1;
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in lines_with_endings(content) {
        spans.push((offset, offset + line.len()));
        offset += line.len();
    }
    spans
}

fn replacement_line_range(
    spans: &[(usize, usize)],
    match_start: usize,
    match_end: usize,
) -> Result<(usize, usize), String> {
    let mut first = None;
    for (i, span) in spans.iter().enumerate() {
        if match_start >= span.0 && match_start < span.1 {
            first = Some(i);
            break;
        }
    }
    let Some(mut last) = first else {
        return Err("replacement range is outside the base content".into());
    };
    while last < spans.len() && spans[last].1 < match_end {
        last += 1;
    }
    if last >= spans.len() {
        return Err("replacement range is outside the base content".into());
    }
    Ok((first.unwrap(), last + 1))
}

fn apply_replacements_preserving_unchanged_lines(
    original: &str,
    base: &str,
    matched: &[MatchedEdit],
) -> Result<String, String> {
    let original_lines = lines_with_endings(original);
    let spans = line_spans(base);
    if original_lines.len() != spans.len() {
        return Err(
            "fuzzy-matched edit could not map to the original file (line count mismatch)".into(),
        );
    }
    let mut groups: Vec<(usize, usize, Vec<&MatchedEdit>)> = Vec::new();
    for m in matched {
        let (start_line, end_line) = replacement_line_range(&spans, m.start, m.start + m.len)?;
        let merged = match groups.last_mut() {
            Some((_, group_end, list)) if start_line < *group_end => {
                *group_end = (*group_end).max(end_line);
                list.push(m);
                true
            }
            _ => false,
        };
        if !merged {
            groups.push((start_line, end_line, vec![m]));
        }
    }
    let mut result = String::with_capacity(original.len());
    let mut original_index = 0;
    for (group_start, group_end, replacements) in &groups {
        result.push_str(&original_lines[original_index..*group_start].concat());
        let slice_start = spans[*group_start].0;
        let slice_end = spans[*group_end - 1].1;
        let mut group_text = base[slice_start..slice_end].to_string();
        for m in replacements.iter().rev() {
            let at = m.start - slice_start;
            group_text.replace_range(at..at + m.len, &m.new_text);
        }
        result.push_str(&group_text);
        original_index = *group_end;
    }
    result.push_str(&original_lines[original_index..].concat());
    Ok(result)
}

pub(crate) fn compute_diff(old: &str, new: &str) -> Vec<DiffLine> {
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

struct Question {
    gate: QuestionGate,
}

impl Tool for Question {
    const NAME: &'static str = "question";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Use this tool when you need to ask the user questions during execution. This allows you to:\n1. Gather user preferences or requirements\n2. Clarify ambiguous instructions\n3. Get decisions on implementation choices as you work\n4. Offer choices to the user about what direction to take.\n\nUsage notes:\n- When `custom` is enabled (default), a \"Type your own answer\" option is added automatically; don't include \"Other\" or catch-all options\n- Answers are returned as arrays of labels; set `multiple: true` to allow selecting more than one\n- If you recommend a specific option, make that the first option in the list and add \"(Recommended)\" at the end of the label".to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "description": "Questions to ask",
                    "items": {
                        "type": "object",
                        "properties": {
                            "question": { "type": "string", "description": "Complete question" },
                            "header": { "type": "string", "description": "Very short label (max 30 chars)" },
                            "options": {
                                "type": "array",
                                "description": "Available choices",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "Display text (1-5 words, concise)" },
                                        "description": { "type": "string", "description": "Explanation of choice" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiple": { "type": "boolean", "description": "Allow selecting multiple choices" },
                            "custom": { "type": "boolean", "description": "Allow typing a custom answer (default: true)" }
                        },
                        "required": ["question", "header", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let gate = self.gate.clone();
        let result: Result<ToolOutput, String> = async move {
            let raw = args
                .get("questions")
                .and_then(Value::as_array)
                .ok_or("missing 'questions' array argument")?;
            let prompts: Vec<QuestionPrompt> = raw
                .iter()
                .map(|q| {
                    let question = q
                        .get("question")
                        .and_then(Value::as_str)
                        .ok_or("missing string argument 'question'")?
                        .to_string();
                    let header = q
                        .get("header")
                        .and_then(Value::as_str)
                        .ok_or("missing string argument 'header'")?
                        .to_string();
                    let options = q
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .map(|o| QuestionOption {
                                    label: o
                                        .get("label")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    description: o
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    let multiple = q.get("multiple").and_then(Value::as_bool).unwrap_or(false);
                    let custom = q.get("custom").and_then(Value::as_bool).unwrap_or(true);
                    Ok::<QuestionPrompt, String>(QuestionPrompt {
                        question,
                        header,
                        options,
                        multiple,
                        custom,
                    })
                })
                .collect::<Result<_, String>>()?;
            let count = prompts.len();
            let answers = gate.ask(prompts.clone()).await?;
            let formatted = prompts
                .iter()
                .zip(&answers)
                .map(|(q, a)| {
                    let joined = if a.is_empty() {
                        "Unanswered".to_string()
                    } else {
                        a.join(", ")
                    };
                    format!("\"{}\"=\"{joined}\"", q.question)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let plural = if count == 1 { "" } else { "s" };
            Ok(ToolOutput::text(format!(
                "User answered {count} question{plural}: {formatted}"
            )))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn all_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    question_gate: QuestionGate,
    shell_tx: ShellOutputTx,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                read_cache: read_cache.clone(),
                max_output_chars,
                max_output_bytes,
            },
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile {
                read_cache: read_cache.clone(),
                lsp: Some(lsp.clone()),
                locks: locks.clone(),
            },
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile {
                lsp: Some(lsp.clone()),
                locks: locks.clone(),
            },
        ),
        shuvarie_llm::into_dynamic(
            "run_shell",
            RunShell {
                shell_tx: shell_tx.clone(),
            },
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir {}),
        shuvarie_llm::into_dynamic("grep", Grep {}),
        shuvarie_llm::into_dynamic("glob", Glob {}),
        shuvarie_llm::into_dynamic("lsp", Lsp { lsp }),
        shuvarie_llm::into_dynamic("webfetch", WebFetch { max_output_chars }),
        shuvarie_llm::into_dynamic(
            "question",
            Question {
                gate: question_gate,
            },
        ),
    ]
}

pub fn read_tools(
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                read_cache,
                max_output_chars,
                max_output_bytes,
            },
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir {}),
        shuvarie_llm::into_dynamic("grep", Grep {}),
        shuvarie_llm::into_dynamic("glob", Glob {}),
        shuvarie_llm::into_dynamic("lsp", Lsp { lsp: lsp.clone() }),
        shuvarie_llm::into_dynamic("webfetch", WebFetch { max_output_chars }),
    ]
}

pub fn command_tools(shell_tx: ShellOutputTx) -> Vec<shuvarie_llm::DynamicTool> {
    vec![shuvarie_llm::into_dynamic(
        "run_shell",
        RunShell { shell_tx },
    )]
}

pub fn edit_tools(
    lsp: SharedManager,
    locks: FileLocks,
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                read_cache: read_cache.clone(),
                max_output_chars,
                max_output_bytes,
            },
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile {
                read_cache,
                lsp: Some(lsp.clone()),
                locks: locks.clone(),
            },
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile {
                lsp: Some(lsp.clone()),
                locks: locks.clone(),
            },
        ),
        shuvarie_llm::into_dynamic(
            "apply_patch",
            crate::apply_patch::ApplyPatch::new(Some(lsp.clone()), locks.clone()),
        ),
        shuvarie_llm::into_dynamic("lsp", Lsp { lsp }),
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

    fn no_lsp() -> Option<SharedManager> {
        None
    }

    fn new_ctx() -> ToolContext {
        ToolContext::new()
    }

    fn read_file_tool() -> ReadFile {
        ReadFile {
            read_cache: ReadCache::new(),
            max_output_chars: 0,
            max_output_bytes: 0,
        }
    }

    fn run_shell_tool() -> RunShell {
        RunShell {
            shell_tx: ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
        }
    }

    #[tokio::test]
    async fn read_file_with_range() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\nthree\n").unwrap();
        let out = read_file_tool()
            .call(
                &mut new_ctx(),
                json!({ "path": "a.txt", "offset": 2, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("two"),
            "{}",
            out.as_text().unwrap()
        );
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "missing.txt" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_refuses_binary() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", [0, 1, 2, 3]).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "bin.dat" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"));
        drop(dir);
    }

    #[tokio::test]
    async fn write_creates_parents() {
        let (dir, _guard) = tempdir();
        let mut ctx = new_ctx();
        let _out = WriteFile {
            read_cache: ReadCache::new(),
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut ctx,
            json!({ "path": "sub/deep/f.txt", "content": "hello" }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("sub/deep/f.txt").unwrap(), "hello");
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Write { path, .. }) if path == "sub/deep/f.txt"
        ));
        drop(dir);
    }

    fn write_file_tool(read_cache: ReadCache) -> WriteFile {
        WriteFile {
            read_cache,
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
    }

    #[tokio::test]
    async fn write_create_mode_fails_on_existing() {
        let (dir, _guard) = tempdir();
        std::fs::write("exists.txt", "old").unwrap();
        let cache = ReadCache::new();
        let err = write_file_tool(cache)
            .call(
                &mut new_ctx(),
                json!({ "path": "exists.txt", "content": "new", "mode": "create" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "{}",
            err.to_string()
        );
        assert_eq!(std::fs::read_to_string("exists.txt").unwrap(), "old");
        drop(dir);
    }

    #[tokio::test]
    async fn write_overwrite_requires_read_first() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "old").unwrap();
        let cache = ReadCache::new();
        let tool = write_file_tool(cache.clone());
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "f.txt", "content": "new", "mode": "overwrite" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("was not read"),
            "{}",
            err.to_string()
        );
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "old");

        let reader = ReadFile {
            read_cache: cache,
            max_output_chars: 0,
            max_output_bytes: 0,
        };
        reader
            .call(&mut new_ctx(), json!({ "path": "f.txt" }))
            .await
            .unwrap();
        tool.call(
            &mut new_ctx(),
            json!({ "path": "f.txt", "content": "new", "mode": "overwrite" }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "new");
        drop(dir);
    }

    #[tokio::test]
    async fn write_atomic_via_temp_rename() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "v1").unwrap();
        let cache = ReadCache::new();
        let reader = ReadFile {
            read_cache: cache.clone(),
            max_output_chars: 0,
            max_output_bytes: 0,
        };
        reader
            .call(&mut new_ctx(), json!({ "path": "f.txt" }))
            .await
            .unwrap();
        write_file_tool(cache)
            .call(&mut new_ctx(), json!({ "path": "f.txt", "content": "v2" }))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("f.txt").unwrap(), "v2");
        let leftovers: Vec<_> = std::fs::read_dir(".")
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                name.contains("tmp").then_some(name)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn write_preserves_bom_and_strips_duplicate() {
        let (dir, _guard) = tempdir();
        std::fs::write("bom.txt", "\u{FEFF}original").unwrap();
        let cache = ReadCache::new();
        let reader = ReadFile {
            read_cache: cache.clone(),
            max_output_chars: 0,
            max_output_bytes: 0,
        };
        reader
            .call(&mut new_ctx(), json!({ "path": "bom.txt" }))
            .await
            .unwrap();
        write_file_tool(cache)
            .call(
                &mut new_ctx(),
                json!({ "path": "bom.txt", "content": "\u{FEFF}replaced" }),
            )
            .await
            .unwrap();
        let bytes = std::fs::read("bom.txt").unwrap();
        assert!(bytes.starts_with(UTF8_BOM));
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            "\u{FEFF}replaced"
        );
        assert_eq!(&bytes[3..], "replaced".as_bytes());
        drop(dir);
    }

    #[tokio::test]
    async fn write_rejects_invalid_mode() {
        let (dir, _guard) = tempdir();
        let err = write_file_tool(ReadCache::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "f.txt", "content": "x", "mode": "append" }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("invalid mode"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_applies_multiple_disjoint_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha beta\ngamma delta\n").unwrap();
        let mut ctx = new_ctx();
        let out = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut ctx,
            json!({
                "path": "e.txt",
                "edits": [
                    { "oldText": "gamma delta", "newText": "gamma delta epsilon" },
                    { "oldText": "alpha", "newText": "ALPHA" }
                ]
            }),
        )
        .await
        .unwrap();
        assert!(out.as_text().unwrap().contains("2 edits applied"));
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "ALPHA beta\ngamma delta epsilon\n"
        );
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Edit { diff, .. }) if !diff.is_empty()
        ));
        drop(dir);
    }

    #[tokio::test]
    async fn edit_rejects_ambiguous_missing_empty_and_noop() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "a b a").unwrap();
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": [{ "oldText": "a", "newText": "x" }] }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("must be unique"),
            "{}",
            err.to_string()
        );
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": [{ "oldText": "zzz", "newText": "x" }] }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Could not find"));
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": [{ "oldText": "", "newText": "x" }] }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "{}",
            err.to_string()
        );
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": [{ "oldText": "a b", "newText": "a b" }] }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("No changes made"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_rejects_overlapping_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "abcd").unwrap();
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({
                "path": "e.txt",
                "edits": [
                    { "oldText": "abc", "newText": "x" },
                    { "oldText": "bcd", "newText": "y" }
                ]
            }),
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("edits[0] and edits[1] overlap")
                || msg.contains("edits[1] and edits[0] overlap"),
            "{}",
            msg
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_accepts_legacy_flat_and_string_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "hello world").unwrap();
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "old": "hello", "new": "hi" }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi world");
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": { "oldText": "world", "newText": "there" } }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi there");
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": "[{\"oldText\":\"there\",\"newText\":\"you\"}]" }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi you");
        drop(dir);
    }

    #[tokio::test]
    async fn edit_preserves_bom_and_crlf() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "\u{FEFF}one\r\ntwo\r\n").unwrap();
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "edits": [{ "oldText": "one", "newText": "1\n2" }] }),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "\u{FEFF}1\r\n2\r\ntwo\r\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_fuzzy_matches_unicode_and_trailing_ws() {
        let (dir, _guard) = tempdir();
        std::fs::write(
            "fuzzy.txt",
            "say \u{201C}hello\u{201D} ok\nplain line\n\u{2014}\u{2014}\n",
        )
        .unwrap();
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({
                "path": "fuzzy.txt",
                "edits": [{ "oldText": "\"hello\" ok", "newText": "'goodbye' ok" }]
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string("fuzzy.txt").unwrap(),
            "say 'goodbye' ok\nplain line\n\u{2014}\u{2014}\n"
        );
        std::fs::write("ws.txt", "head   \nkeep  me\n").unwrap();
        let err = EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({
                "path": "ws.txt",
                "edits": [{ "oldText": "keep me", "newText": "KEEP ME" }]
            }),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("Could not find"),
            "internal double spaces are not normalized: {}",
            err
        );
        EditFile {
            lsp: no_lsp(),
            locks: FileLocks::new(),
        }
        .call(
            &mut new_ctx(),
            json!({
                "path": "ws.txt",
                "edits": [{ "oldText": "head\nkeep  me", "newText": "HEAD\nKEEP  ME" }]
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string("ws.txt").unwrap(),
            "HEAD\nKEEP  ME\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn concurrent_edits_serialize_per_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "token alpha and token beta\n").unwrap();
        let locks = FileLocks::new();
        let tool_a = EditFile {
            lsp: no_lsp(),
            locks: locks.clone(),
        };
        let tool_b = EditFile {
            lsp: no_lsp(),
            locks,
        };
        let (ra, rb) = tokio::join!(
            async {
                tool_a
                    .call(
                        &mut new_ctx(),
                        json!({ "path": "f.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                    )
                    .await
            },
            async {
                tool_b
                    .call(
                        &mut new_ctx(),
                        json!({ "path": "f.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                    )
                    .await
            }
        );
        ra.unwrap();
        rb.unwrap();
        assert_eq!(
            std::fs::read_to_string("f.txt").unwrap(),
            "token ALPHA and token BETA\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_by_bytes() {
        let (dir, _guard) = tempdir();
        std::fs::write("bytes.txt", "abcdef\n").unwrap();
        let tool = ReadFile {
            read_cache: ReadCache::new(),
            max_output_chars: 0,
            max_output_bytes: 3,
        };
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "bytes.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(
            text.contains("bytes omitted") && text.contains("use offset/limit"),
            "{text}"
        );
        drop(dir);
    }

    #[cfg(unix)]
    #[test]
    fn tilde_paths_expand() {
        let (dir, _guard) = tempdir();
        let original_home = std::env::var("HOME").ok();
        let home = TempDir::new().unwrap();
        unsafe { std::env::set_var("HOME", home.path()) };
        std::fs::write(home.path().join("homefile.txt"), "x").unwrap();
        let err = resolve_write("~/homefile.txt").unwrap_err();
        assert!(err.contains("outside the working directory"), "{err}");
        match original_home {
            Some(home_path) => unsafe { std::env::set_var("HOME", home_path) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_success_and_timeout() {
        let (dir, _guard) = tempdir();
        let out = run_shell_tool()
            .call(&mut new_ctx(), json!({ "command": "echo hello" }))
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("hello"));
        let err = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "sleep 5", "timeout_secs": 1 }),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("timed out"), "{}", message);
        assert!(
            message.contains("retry with a larger timeout_secs"),
            "{}",
            message
        );
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_timeout_returns_partial_output() {
        let (dir, _guard) = tempdir();
        let err = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "echo partial-first; sleep 5", "timeout_secs": 1 }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("partial-first"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_timeout_kills_children() {
        use std::process::Command as StdCommand;
        let (dir, _guard) = tempdir();
        let _ = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "sleep 30 & sleep 30", "timeout_secs": 1 }),
            )
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let out = StdCommand::new("sh")
            .arg("-c")
            .arg("pgrep -f '[s]leep 30' | wc -l")
            .output()
            .unwrap();
        let count: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .unwrap_or(0);
        assert_eq!(count, 0, "sleep children survived the group kill");
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_streams_output() {
        let (dir, _guard) = tempdir();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let tool = RunShell {
            shell_tx: ShellOutputTx::new(tx),
        };
        let out = tool
            .call(
                &mut new_ctx(),
                json!({ "command": "echo streamed-line; sleep 0.4" }),
            )
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("streamed-line"));
        let mut saw_stream = false;
        while let Ok(chunk) = rx.try_recv() {
            assert_eq!(chunk.worker, None);
            if chunk.stdout.contains("streamed-line") {
                saw_stream = true;
            }
        }
        assert!(saw_stream, "no streaming chunk carried the echoed line");
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_worker_chunks_are_tagged() {
        let (dir, _guard) = tempdir();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let tool = RunShell {
            shell_tx: ShellOutputTx::new(tx).tagged("run_tests"),
        };
        tool.call(&mut new_ctx(), json!({ "command": "echo tagged" }))
            .await
            .unwrap();
        let mut tagged = false;
        while let Ok(chunk) = rx.try_recv() {
            if chunk.worker.as_deref() == Some("run_tests") && chunk.stdout.contains("tagged") {
                tagged = true;
            }
        }
        assert!(tagged);
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_validates_cwd() {
        let (dir, _guard) = tempdir();
        std::fs::write("not-a-dir", "").unwrap();
        let err = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "true", "cwd": "not-a-dir" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{}", err);
        let err = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "true", "cwd": "missing-dir" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing-dir"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_allows_outside_workspace_cwd() {
        let (dir, _guard) = tempdir();
        let outside = TempDir::new().unwrap();
        let tool = run_shell_tool();
        let cwd = outside.path().to_string_lossy().into_owned();
        let out = tool
            .call(&mut new_ctx(), json!({ "command": "pwd", "cwd": cwd }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        let tail = outside
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(text.contains(&tail), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_failure_returns_error() {
        let (dir, _guard) = tempdir();
        let err = run_shell_tool()
            .call(&mut new_ctx(), json!({ "command": "exit 3" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("3"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_supports_pipes() {
        let (dir, _guard) = tempdir();
        let out = run_shell_tool()
            .call(
                &mut new_ctx(),
                json!({ "command": "echo hello world | grep world" }),
            )
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("world"),
            "{}",
            out.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn list_dir_sorted() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("zdir").unwrap();
        std::fs::write("afile", "").unwrap();
        let out = ListDir {}.call(&mut new_ctx(), json!({})).await.unwrap();
        let lines: Vec<&str> = out.as_text().unwrap().lines().collect();
        assert_eq!(lines, vec!["afile", "zdir/"]);
        drop(dir);
    }

    #[tokio::test]
    async fn grep_finds_and_caps() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep {}
            .call(&mut new_ctx(), json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("r.rs:1"));
        assert!(!out.as_text().unwrap().contains("r.txt"));
        let none = Grep {}
            .call(&mut new_ctx(), json!({ "pattern": "zzzz" }))
            .await
            .unwrap();
        assert!(none.as_text().unwrap().contains("no matches"));
        drop(dir);
    }

    #[tokio::test]
    async fn glob_matches_recursively() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all("src/sub").unwrap();
        std::fs::write("src/a.rs", "").unwrap();
        std::fs::write("src/sub/b.rs", "").unwrap();
        std::fs::write("src/c.txt", "").unwrap();
        let out = Glob {}
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
        let out = Glob {}
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
        let out = Glob {}
            .call(&mut new_ctx(), json!({ "pattern": "*.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("truncated"), "{text}");
        let none = Glob {}
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
        let err = Glob {}
            .call(
                &mut new_ctx(),
                json!({ "pattern": "*.txt", "path": "a.txt" }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("must be a directory"));
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

    #[tokio::test]
    async fn read_cache_dedupes_repeated_reads() {
        let (dir, _guard) = tempdir();
        std::fs::write("dup.txt", "line\n").unwrap();
        let cache = ReadCache::new();
        let tool = ReadFile {
            read_cache: cache.clone(),
            max_output_chars: 0,
            max_output_bytes: 0,
        };
        let first = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(first.as_text().unwrap().contains("line"));
        let second = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(
            second.as_text().unwrap().contains("already read"),
            "{}",
            second.as_text().unwrap()
        );
        assert!(!second.as_text().unwrap().contains("line |"));
        let ranged = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "dup.txt", "offset": 1, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            ranged.as_text().unwrap().contains("line"),
            "{}",
            ranged.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_large_output() {
        let (dir, _guard) = tempdir();
        let big = "x".repeat(10_000) + "\n";
        std::fs::write("big.txt", &big).unwrap();
        let tool = ReadFile {
            read_cache: ReadCache::new(),
            max_output_chars: 100,
            max_output_bytes: 0,
        };
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "big.txt" }))
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("truncated"),
            "{}",
            out.as_text().unwrap()
        );
        assert!(out.as_text().unwrap().chars().count() < big.len() + 200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_defaults_to_2000_lines_with_footers() {
        let (dir, _guard) = tempdir();
        let content: String = (1..=2100).map(|i| format!("line{i}\n")).collect();
        std::fs::write("many.txt", &content).unwrap();
        let tool = read_file_tool();
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "many.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Showing lines 1-2000 of 2100"), "{text}");
        assert!(text.contains("Use offset=2001"), "{text}");
        let rest = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "many.txt", "offset": 2001 }),
            )
            .await
            .unwrap();
        let text = rest.as_text().unwrap();
        assert!(text.contains("End of file - total 2100 lines"), "{text}");
        assert!(text.contains("2100 | line2100"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn read_offset_out_of_range_errors() {
        let (dir, _guard) = tempdir();
        std::fs::write("small.txt", "a\nb\nc\n").unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "small.txt", "offset": 10 }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "{}",
            err.to_string()
        );
        assert!(err.to_string().contains("3 lines"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_caps_long_lines() {
        let (dir, _guard) = tempdir();
        std::fs::write("long.txt", "z".repeat(3000) + "\n").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "long.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains(MAX_LINE_SUFFIX), "{text}");
        assert!(text.chars().count() < 2200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_detects_binary_by_nonprintable_ratio() {
        let (dir, _guard) = tempdir();
        let mut data = vec![b'a'; 600];
        data.extend(std::iter::repeat_n(0x07u8, 600));
        std::fs::write("weird.bin", &data).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "weird.bin" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn read_reaches_beyond_64kb() {
        let (dir, _guard) = tempdir();
        let line = "y".repeat(1000) + "\n";
        std::fs::write("wide.txt", line.repeat(100)).unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "wide.txt", "offset": 90 }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("90 |"), "{text}");
        assert!(text.contains("End of file - total 100 lines"), "{text}");
        drop(dir);
    }

    #[test]
    fn resolve_read_classification() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".git").unwrap();
        std::fs::write(".git/config", "x").unwrap();
        assert!(resolve_read(".git/config").unwrap_err().contains("hidden"));
        assert!(resolve_read(".").is_ok());

        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-outside-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(resolve_read(&rel).is_ok());
        assert!(resolve_write(&rel).is_err());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }

    #[tokio::test]
    async fn webfetch_rejects_non_http_urls() {
        for url in ["ftp://example.com/x", "file:///etc/passwd", "example.com"] {
            let err = WebFetch {
                max_output_chars: 0,
            }
            .call(&mut new_ctx(), json!({ "url": url }))
            .await
            .unwrap_err();
            assert!(err.to_string().contains("http://"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn webfetch_rejects_invalid_format() {
        let err = WebFetch {
            max_output_chars: 0,
        }
        .call(
            &mut new_ctx(),
            json!({ "url": "https://example.com", "format": "pdf" }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("invalid format"), "{err}");
    }

    #[test]
    fn webfetch_looks_html_rules() {
        assert!(webfetch_looks_html(
            "text/html; charset=utf-8",
            "https://x.com"
        ));
        assert!(webfetch_looks_html(
            "application/xhtml+xml",
            "https://x.com"
        ));
        assert!(!webfetch_looks_html("application/json", "https://x.com"));
        assert!(!webfetch_looks_html("text/plain", "https://x.com/a.html"));
        assert!(webfetch_looks_html("", "https://x.com/docs/page.html"));
        assert!(webfetch_looks_html("", "https://x.com/a/b.htm?q=1#frag"));
        assert!(webfetch_looks_html("", "https://x.com/c.XHTML"));
        assert!(!webfetch_looks_html("", "https://x.com/data.json"));
        assert!(!webfetch_looks_html("", "https://x.com/noext"));
    }

    #[test]
    fn webfetch_convert_markdown_and_plain() {
        let html = "<h1>Title</h1><p>Para with <b>bold</b> and a <a href='https://x.com'>link</a>.</p><script>alert(1)</script>";
        let md = webfetch_convert(html, false);
        assert!(md.contains("# Title"), "{md}");
        assert!(md.contains("**bold**"), "{md}");
        assert!(!md.contains("alert(1)"), "{md}");
        let plain = webfetch_convert(html, true);
        assert!(plain.contains("Title"), "{plain}");
        assert!(!plain.contains("**"), "{plain}");
        assert!(!plain.contains("alert(1)"), "{plain}");
    }

    #[tokio::test]
    async fn webfetch_finish_truncates_long_output() {
        let body = "x".repeat(4000);
        let out = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "text/plain")
                    .body(body)
                    .unwrap(),
            ),
            "https://example.com/data.txt",
            "text",
            100,
        )
        .await
        .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("output truncated"), "{text}");
        assert!(text.contains("chars omitted"), "{text}");
    }

    #[tokio::test]
    async fn webfetch_finish_rejects_images_and_empty() {
        let err = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "image/png")
                    .body(b"\x89PNG".to_vec())
                    .unwrap(),
            ),
            "https://example.com/i.png",
            "text",
            0,
        )
        .await
        .unwrap_err();
        assert!(err.contains("image"), "{err}");

        let out = webfetch_finish(
            reqwest::Response::from(
                http::Response::builder()
                    .status(200)
                    .header("content-type", "text/html")
                    .body(
                        "<html><body><script>var x=1;</script></body></html>"
                            .as_bytes()
                            .to_vec(),
                    )
                    .unwrap(),
            ),
            "https://example.com/e",
            "markdown",
            0,
        )
        .await
        .unwrap();
        assert!(
            out.as_text().unwrap().contains("empty response"),
            "{}",
            out.as_text().unwrap()
        );
    }
}
