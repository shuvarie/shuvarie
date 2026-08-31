use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shuvarie_llm::{
    DiffLine, DiffLineKind, FileChange, TodoItem, TodoUpdate, Tool, ToolContext,
    ToolExecutionError, ToolOutput,
};

use crate::approval::{ApprovalGate, ApprovalReason};
use crate::lsp_manager::SharedManager;
use crate::question::{QuestionGate, QuestionOption, QuestionPrompt};

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const SHELL_CAPTURE_BYTES: usize = 64 * 1024;
const SHELL_STREAM_TAIL_CHARS: usize = 2048;
const SHELL_STREAM_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct ShellChunk {
    pub worker: Option<String>,
    pub content: String,
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

    async fn send_tail(&self, captured: &[u8]) {
        let content: String = String::from_utf8_lossy(captured)
            .chars()
            .rev()
            .take(SHELL_STREAM_TAIL_CHARS)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let _ = self
            .tx
            .send(ShellChunk {
                worker: self.worker.clone(),
                content,
            })
            .await;
    }
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
/// that have already been returned to the model this turn. Repeated identical
/// reads get a short note instead of re-sending file content, which keeps the
/// agent loop from blowing up the context by re-reading the same large file.
#[derive(Clone, Default)]
pub struct ReadCache {
    seen: Arc<Mutex<std::collections::HashSet<ReadKey>>>,
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
        !seen.insert(key)
    }
}

pub(crate) fn arg_value(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

struct ReadFile {
    gate: ApprovalGate,
    read_cache: ReadCache,
    max_output_chars: usize,
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
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum number of lines to read. Defaults to all lines" }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let gate = self.gate.clone();
        let read_cache = self.read_cache.clone();
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            if read_cache.mark(&path, offset, limit) {
                return Ok(ToolOutput::text(format!(
                    "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                )));
            }
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
            if max_output_chars > 0 && out.chars().count() > max_output_chars {
                let omitted = out.chars().count() - max_output_chars;
                let truncated: String = out.chars().take(max_output_chars).collect();
                return Ok(ToolOutput::text(format!(
                    "{truncated}… (output truncated: {omitted} chars omitted — use offset/limit to read more of {path})"
                )));
            }
            Ok(ToolOutput::text(out))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct WriteFile {
    gate: ApprovalGate,
    lsp: Option<SharedManager>,
}

impl Tool for WriteFile {
    const NAME: &'static str = "write_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Create or overwrite a file in the working directory, creating parent directories as needed."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to write" },
                "content": { "type": "string", "description": "Full new contents of the file" }
            },
            "required": ["path", "content"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let gate = self.gate.clone();
        let lsp = self.lsp.clone();
        let result: Result<ToolOutput, String> = async move {
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
            let summary = format!("wrote {} bytes to {path}", content.len());
            ctx.insert_result(FileChange::Write {
                path,
                content,
                original,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct EditFile {
    gate: ApprovalGate,
    lsp: Option<SharedManager>,
}

impl Tool for EditFile {
    const NAME: &'static str = "edit_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Replace text in an existing file with a string replacement. When `old` appears more than once and `occurrence` is unset the edit is rejected; pass `occurrence` to pick the nth match (1-based)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to edit" },
                "old": { "type": "string", "description": "Exact text to find (must appear in the file)" },
                "new": { "type": "string", "description": "Replacement text" },
                "occurrence": { "type": "integer", "minimum": 1, "description": "Which match to replace (1-based). Required when `old` appears multiple times" }
            },
            "required": ["path", "old", "new"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let gate = self.gate.clone();
        let lsp = self.lsp.clone();
        let result: Result<ToolOutput, String> = async move {
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
            let summary = format!("edited {path}: replaced 1 of {} occurrences", matches.len());
            ctx.insert_result(FileChange::Edit {
                path,
                diff,
                original: content,
                new: edited,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct RunShell {
    gate: ApprovalGate,
    shell_tx: ShellOutputTx,
}

impl Tool for RunShell {
    const NAME: &'static str = "run_shell";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        #[cfg(unix)]
        const DESCRIPTION: &str = "Run a shell command line in the workspace, executed through the system's Bourne shell (`sh -c`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`; a directory outside the workspace requires user approval.";
        #[cfg(windows)]
        const DESCRIPTION: &str = "Run a shell command line in the workspace, executed through the system's PowerShell (`powershell -NoProfile -Command`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`; a directory outside the workspace requires user approval.";

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
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let command = arg_value(&args, "command")?;
            let cwd = args.get("cwd").and_then(Value::as_str);
            let timeout_secs = args
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(DEFAULT_TIMEOUT_SECS);
            let (cwd_abs, gate_reason) = match cwd {
                Some(c) => {
                    let root = workspace_root()?;
                    let joined = root.join(c);
                    let resolved = joined
                        .canonicalize()
                        .map_err(|e| format!("cwd '{c}': {e}"))?;
                    if !resolved.is_dir() {
                        return Err(format!("cwd '{c}' is not a directory"));
                    }
                    let reason = (!resolved.starts_with(&root)).then_some(ApprovalReason::OutsideWorkspace);
                    (resolved, reason)
                }
                None => (workspace_root()?, None),
            };
            if let Some(reason) = gate_reason {
                self.gate
                    .request(Self::NAME, &cwd_abs.to_string_lossy(), reason)
                    .await?;
            }
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

            let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
            let mut readers = Vec::new();
            if let Some(pipe) = child.stdout.take() {
                readers.push(spawn_pipe_reader(pipe, chunk_tx.clone()));
            }
            if let Some(pipe) = child.stderr.take() {
                readers.push(spawn_pipe_reader(pipe, chunk_tx.clone()));
            }
            drop(chunk_tx);

            let mut captured: Vec<u8> = Vec::new();
            let mut last_emit = std::time::Instant::now() - std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
            let interval = std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
            let status = loop {
                tokio::select! {
                    maybe_chunk = chunk_rx.recv() => {
                        match maybe_chunk {
                            Some(chunk) => {
                                captured.extend_from_slice(&chunk);
                                if captured.len() > SHELL_CAPTURE_BYTES {
                                    let drop = captured.len() - SHELL_CAPTURE_BYTES;
                                    captured.drain(..drop);
                                }
                                if last_emit.elapsed() >= interval {
                                    last_emit = std::time::Instant::now();
                                    self.shell_tx.send_tail(&captured).await;
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
                        while let Some(chunk) = chunk_rx.recv().await {
                            captured.extend_from_slice(&chunk);
                            if captured.len() > SHELL_CAPTURE_BYTES {
                                let drop = captured.len() - SHELL_CAPTURE_BYTES;
                                captured.drain(..drop);
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
                        self.shell_tx.send_tail(&captured).await;
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
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
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
                    if tx.send(buf[..n].to_vec()).await.is_err() {
                        break;
                    }
                }
            }
        }
    })
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
    gate: ApprovalGate,
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
        let gate = self.gate.clone();
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

            gate.request("webfetch", &url, ApprovalReason::Network)
                .await?;

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

    if max_output_chars > 0 && out.chars().count() > max_output_chars {
        let omitted = out.chars().count() - max_output_chars;
        let truncated: String = out.chars().take(max_output_chars).collect();
        return Ok(ToolOutput::text(format!(
            "{truncated}… (output truncated: {omitted} chars omitted — fetch a narrower URL if needed)"
        )));
    }
    Ok(ToolOutput::text(format!("{url} ({content_type})\n\n{out}")))
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

struct ListDir {
    gate: ApprovalGate,
}

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
        let gate = self.gate.clone();
        let result: Result<ToolOutput, String> = async move {
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
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct Grep {
    gate: ApprovalGate,
}

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
        let gate = self.gate.clone();
        let result: Result<ToolOutput, String> = async move {
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
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

const GLOB_MAX_RESULTS: usize = 100;

struct Glob {
    gate: ApprovalGate,
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
        let gate = self.gate.clone();
        let result: Result<ToolOutput, String> = async move {
            let pattern = arg_value(&args, "pattern")?;
            let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
            let (abs, reason) = resolve_checked(path)?;
            if let Some(reason) = reason {
                gate.request("glob", path, reason).await?;
            }
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

pub(crate) fn hidden_reason(path: &str) -> Option<ApprovalReason> {
    let has_hidden = path
        .split(['/', '\\'])
        .any(|c| c.starts_with('.') && c != "." && c != "..");
    has_hidden.then_some(ApprovalReason::HiddenPath)
}

pub(crate) fn resolve_checked(path: &str) -> Result<(PathBuf, Option<ApprovalReason>), String> {
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

pub(crate) fn resolve_for_write_checked(
    path: &str,
) -> Result<(PathBuf, Option<ApprovalReason>), String> {
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

/// Maintains the session-scoped task list. The model sends the full updated
/// list on every call (mirroring OpenCode's `todowrite`); the new state is
/// attached to the `ToolContext` as a [`TodoUpdate`] so the core task can
/// persist it and the TUI can render it.
struct Todo;

impl Tool for Todo {
    const NAME: &'static str = "todo";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Maintain a session-scoped task list. Use it for multi-step work: create the list \
         up front, then update it as you make progress. Each call replaces the whole list, \
         so always send every item. Each item has a `content` (brief description), a `status` \
         of `pending`, `in_progress`, `completed`, or `cancelled`, and a `priority` of `high`, \
         `medium`, or `low`. Keep exactly one item `in_progress` at a time, and mark an item \
         `completed` only after you have actually verified the work is done."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The updated todo list",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "Brief description of the task" },
                            "status": { "type": "string", "description": "pending, in_progress, completed, or cancelled" },
                            "priority": { "type": "string", "description": "high, medium, or low" }
                        },
                        "required": ["content", "status", "priority"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let todos: Vec<TodoItem> = args
            .get("todos")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let content = item.get("content").and_then(Value::as_str)?;
                        let status = item.get("status").and_then(Value::as_str)?;
                        let priority = item.get("priority").and_then(Value::as_str)?;
                        Some(TodoItem {
                            content: content.to_string(),
                            status: status.to_string(),
                            priority: priority.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ctx.insert_result(TodoUpdate {
            todos: todos.clone(),
        });
        let remaining = todos.iter().filter(|t| t.status != "completed").count();
        Ok(ToolOutput::text(format!("{remaining} todos")))
    }
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

pub fn all_tools(
    gate: ApprovalGate,
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
    question_gate: QuestionGate,
    shell_tx: ShellOutputTx,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                gate: gate.clone(),
                read_cache: read_cache.clone(),
                max_output_chars,
            },
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile {
                gate: gate.clone(),
                lsp: Some(lsp.clone()),
            },
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile {
                gate: gate.clone(),
                lsp: Some(lsp.clone()),
            },
        ),
        shuvarie_llm::into_dynamic(
            "apply_patch",
            crate::apply_patch::ApplyPatch::new(gate.clone(), Some(lsp.clone())),
        ),
        shuvarie_llm::into_dynamic(
            "run_shell",
            RunShell {
                gate: gate.clone(),
                shell_tx: shell_tx.clone(),
            },
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("grep", Grep { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("glob", Glob { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("lsp", Lsp { lsp }),
        shuvarie_llm::into_dynamic("todo", Todo),
        shuvarie_llm::into_dynamic(
            "webfetch",
            WebFetch {
                gate,
                max_output_chars,
            },
        ),
        shuvarie_llm::into_dynamic(
            "question",
            Question {
                gate: question_gate,
            },
        ),
    ]
}

pub fn read_tools(
    gate: ApprovalGate,
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                gate: gate.clone(),
                read_cache,
                max_output_chars,
            },
        ),
        shuvarie_llm::into_dynamic("list_dir", ListDir { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("grep", Grep { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("glob", Glob { gate: gate.clone() }),
        shuvarie_llm::into_dynamic("lsp", Lsp { lsp: lsp.clone() }),
        shuvarie_llm::into_dynamic(
            "webfetch",
            WebFetch {
                gate,
                max_output_chars,
            },
        ),
    ]
}

pub fn command_tools(
    gate: ApprovalGate,
    shell_tx: ShellOutputTx,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![shuvarie_llm::into_dynamic(
        "run_shell",
        RunShell { gate, shell_tx },
    )]
}

pub fn edit_tools(
    gate: ApprovalGate,
    lsp: SharedManager,
    read_cache: ReadCache,
    max_output_chars: usize,
) -> Vec<shuvarie_llm::DynamicTool> {
    vec![
        shuvarie_llm::into_dynamic(
            "read_file",
            ReadFile {
                gate: gate.clone(),
                read_cache,
                max_output_chars,
            },
        ),
        shuvarie_llm::into_dynamic(
            "write_file",
            WriteFile {
                gate: gate.clone(),
                lsp: Some(lsp.clone()),
            },
        ),
        shuvarie_llm::into_dynamic(
            "edit_file",
            EditFile {
                gate: gate.clone(),
                lsp: Some(lsp.clone()),
            },
        ),
        shuvarie_llm::into_dynamic(
            "apply_patch",
            crate::apply_patch::ApplyPatch::new(gate, Some(lsp.clone())),
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

    fn gate() -> ApprovalGate {
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        ApprovalGate::new(tx)
    }

    fn no_lsp() -> Option<SharedManager> {
        None
    }

    fn new_ctx() -> ToolContext {
        ToolContext::new()
    }

    fn read_file_tool() -> ReadFile {
        ReadFile {
            gate: gate(),
            read_cache: ReadCache::new(),
            max_output_chars: 0,
        }
    }

    fn run_shell_tool() -> RunShell {
        RunShell {
            gate: gate(),
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
            gate: gate(),
            lsp: no_lsp(),
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

    #[tokio::test]
    async fn edit_replaces_and_detects_ambiguity() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "a b a").unwrap();
        let err = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "old": "a", "new": "x" }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("occurrence"));
        let mut ctx = new_ctx();
        let _out = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(
            &mut ctx,
            json!({ "path": "e.txt", "old": "a", "new": "x", "occurrence": 2 }),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "a b x");
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Edit { diff, .. }) if !diff.is_empty()
        ));
        let err = EditFile {
            gate: gate(),
            lsp: no_lsp(),
        }
        .call(
            &mut new_ctx(),
            json!({ "path": "e.txt", "old": "zzz", "new": "x" }),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("not found"));
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
            gate: gate(),
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
            if chunk.content.contains("streamed-line") {
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
            gate: gate(),
            shell_tx: ShellOutputTx::new(tx).tagged("run_tests"),
        };
        tool.call(&mut new_ctx(), json!({ "command": "echo tagged" }))
            .await
            .unwrap();
        let mut tagged = false;
        while let Ok(chunk) = rx.try_recv() {
            if chunk.worker.as_deref() == Some("run_tests") && chunk.content.contains("tagged") {
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
    async fn run_shell_gates_outside_workspace_cwd() {
        let (dir, _guard) = tempdir();
        let outside = TempDir::new().unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::channel::<crate::approval::ApprovalRequest>(8);
        let tool = RunShell {
            gate: ApprovalGate::new(tx),
            shell_tx: ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
        };
        let cwd = outside.path().to_string_lossy().into_owned();
        let call = tokio::spawn({
            let tool = tool;
            async move {
                tool.call(
                    &mut new_ctx(),
                    json!({ "command": "echo gated", "cwd": cwd }),
                )
                .await
            }
        });
        let req = rx.recv().await.unwrap();
        assert_eq!(req.reason, ApprovalReason::OutsideWorkspace);
        req.respond.send(true).unwrap();
        let out = call.await.unwrap().unwrap();
        assert!(out.as_text().unwrap().contains("gated"));
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
        let out = ListDir { gate: gate() }
            .call(&mut new_ctx(), json!({}))
            .await
            .unwrap();
        let lines: Vec<&str> = out.as_text().unwrap().lines().collect();
        assert_eq!(lines, vec!["afile", "zdir/"]);
        drop(dir);
    }

    #[tokio::test]
    async fn grep_finds_and_caps() {
        let (dir, _guard) = tempdir();
        std::fs::write("r.rs", "fn main() {}\n").unwrap();
        std::fs::write("r.txt", "hello fn world\n").unwrap();
        let out = Grep { gate: gate() }
            .call(&mut new_ctx(), json!({ "pattern": "fn", "include": ".rs" }))
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("r.rs:1"));
        assert!(!out.as_text().unwrap().contains("r.txt"));
        let none = Grep { gate: gate() }
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
        let out = Glob { gate: gate() }
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
        let out = Glob { gate: gate() }
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
        let out = Glob { gate: gate() }
            .call(&mut new_ctx(), json!({ "pattern": "*.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("truncated"), "{text}");
        let none = Glob { gate: gate() }
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
        let err = Glob { gate: gate() }
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
            gate: gate(),
            read_cache: cache.clone(),
            max_output_chars: 0,
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
            gate: gate(),
            read_cache: ReadCache::new(),
            max_output_chars: 100,
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

    #[tokio::test]
    async fn webfetch_rejects_non_http_urls() {
        for url in ["ftp://example.com/x", "file:///etc/passwd", "example.com"] {
            let err = WebFetch {
                gate: gate(),
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
            gate: gate(),
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
