use serde_json::{Value, json};
use shuvarie_llm::{ShellStreams, Tool, ToolContext, ToolExecutionError, ToolOutput};

use std::path::Path;

use super::{DEFAULT_TIMEOUT_SECS, arg_value, workspace_root};
use crate::permissions::{Access, OutputInterrupt};
use crate::shell::Shell;

const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
const SHELL_CAPTURE_BYTES: usize = 64 * 1024;
const SHELL_RAW_CAPTURE_BYTES: usize = 1024 * 1024;
const SHELL_STREAM_TAIL_CHARS: usize = 2048;
const SHELL_STREAM_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone)]
pub struct ShellChunk {
    pub worker: Option<String>,
    /// The exact command line of the call that produced this chunk: with
    /// several concurrent `run_shell` calls of one agent, this is what
    /// correlates a streamed chunk back to its own call.
    pub command: String,
    pub stdout: String,
    pub stderr: String,
}

/// Bounded channel sender carrying live `run_shell` output to the core task.
/// Each clone is tagged with the owning worker so the TUI can attach the
/// streamed output to the right tool activity entry. Tail mode (the tool
/// path) streams and returns trimmed display caps; raw mode (bash mode's
/// display popup) keeps the full untrimmed capture.
#[derive(Clone)]
pub struct ShellOutputTx {
    tx: tokio::sync::mpsc::Sender<ShellChunk>,
    worker: Option<String>,
    raw: bool,
}

impl ShellOutputTx {
    pub fn new(tx: tokio::sync::mpsc::Sender<ShellChunk>) -> Self {
        Self {
            tx,
            worker: None,
            raw: false,
        }
    }

    pub fn full(tx: tokio::sync::mpsc::Sender<ShellChunk>) -> Self {
        Self {
            tx,
            worker: None,
            raw: true,
        }
    }

    pub fn tagged(&self, worker: &str) -> Self {
        Self {
            tx: self.tx.clone(),
            worker: Some(worker.to_string()),
            raw: self.raw,
        }
    }

    fn capture_bytes(&self) -> usize {
        if self.raw {
            SHELL_RAW_CAPTURE_BYTES
        } else {
            SHELL_CAPTURE_BYTES
        }
    }

    async fn send_streams(&self, command: &str, stdout: &[u8], stderr: &[u8]) {
        let _ = self
            .tx
            .send(ShellChunk {
                worker: self.worker.clone(),
                command: command.to_string(),
                stdout: self.stream_text(stdout),
                stderr: self.stream_text(stderr),
            })
            .await;
    }

    fn stream_text(&self, bytes: &[u8]) -> String {
        if self.raw {
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            stream_tail(bytes)
        }
    }

    fn display(&self, bytes: &[u8]) -> String {
        if self.raw {
            String::from_utf8_lossy(bytes).into_owned()
        } else {
            display_stream(bytes)
        }
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

/// One finished shell run: the exit status, whether the run hit its timeout,
/// and the captured streams. In tail mode `out`/`err` are trimmed and
/// display-capped; in raw mode they carry the full capture verbatim.
/// `captured` is the trimmed capture used for tool error messages.
pub(crate) struct ShellRun {
    pub status: std::process::ExitStatus,
    pub timed_out: bool,
    /// Set when a `deny` permission rule matched the captured output and the
    /// command was killed.
    pub interrupted: Option<String>,
    pub out: String,
    pub err: String,
    pub captured: String,
}

/// Spawns `command` through `shell` in `cwd`, streaming live output through
/// `shell_tx` as it arrives (the stream tail in tail mode, the full capture in
/// raw mode). `timeout_secs: None` runs unbounded; a timeout kills the process
/// group and reports the partial capture.
pub(crate) async fn run_shell_command(
    shell: &Shell,
    command: &str,
    cwd: &Path,
    timeout_secs: Option<u64>,
    shell_tx: &ShellOutputTx,
    interrupt: Option<&OutputInterrupt>,
) -> Result<ShellRun, String> {
    let mut builder = tokio::process::Command::new(&shell.path);
    shell.apply(&mut builder, command);
    builder
        .current_dir(cwd)
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
    let capture_bytes = shell_tx.capture_bytes();
    let mut last_emit =
        std::time::Instant::now() - std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
    let interval = std::time::Duration::from_millis(SHELL_STREAM_INTERVAL_MS);
    let deadline =
        timeout_secs.map(|secs| tokio::time::Instant::now() + std::time::Duration::from_secs(secs));
    let status = loop {
        tokio::select! {
            maybe_chunk = chunk_rx.recv() => {
                match maybe_chunk {
                    Some((is_stderr, chunk)) => {
                        captured.extend_from_slice(&chunk);
                        cap_buffer(&mut captured, SHELL_CAPTURE_BYTES);
                        if is_stderr {
                            err.extend_from_slice(&chunk);
                            cap_buffer(&mut err, capture_bytes);
                        } else {
                            out.extend_from_slice(&chunk);
                            cap_buffer(&mut out, capture_bytes);
                        }
                        if let Some(reason) = interrupt
                            .and_then(|watch| watch.check(&String::from_utf8_lossy(&captured)))
                        {
                            if let Some(pgid) = pgid {
                                kill_process_group(pgid);
                            }
                            let _ = child.kill().await;
                            let status =
                                child.wait().await.map_err(|e| format!("wait shell: {e}"))?;
                            shell_tx.send_streams(command, &out, &err).await;
                            guard.disarm();
                            return Ok(ShellRun {
                                status,
                                timed_out: false,
                                interrupted: Some(reason),
                                out: shell_tx.display(&out),
                                err: shell_tx.display(&err),
                                captured: String::from_utf8_lossy(&captured).trim().to_string(),
                            });
                        }
                        if last_emit.elapsed() >= interval {
                            last_emit = std::time::Instant::now();
                            shell_tx.send_streams(command, &out, &err).await;
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
                    cap_buffer(&mut captured, SHELL_CAPTURE_BYTES);
                    if is_stderr {
                        err.extend_from_slice(&chunk);
                        cap_buffer(&mut err, capture_bytes);
                    } else {
                        out.extend_from_slice(&chunk);
                        cap_buffer(&mut out, capture_bytes);
                    }
                }
                let status = status.map_err(|e| format!("wait shell: {e}"))?;
                break status;
            }
            // An absolute deadline, so recreating the sleep each loop keeps
            // the same fire time; `pending` never resolves when unbounded.
            _ = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                if let Some(pgid) = pgid {
                    kill_process_group(pgid);
                }
                let _ = child.kill().await;
                let status = child.wait().await.map_err(|e| format!("wait shell: {e}"))?;
                shell_tx.send_streams(command, &out, &err).await;
                guard.disarm();
                return Ok(ShellRun {
                    status,
                    timed_out: true,
                    interrupted: None,
                    out: shell_tx.display(&out),
                    err: shell_tx.display(&err),
                    captured: String::from_utf8_lossy(&captured).trim().to_string(),
                });
            }
        }
    };

    for reader in readers {
        let _ = reader.await;
    }
    guard.disarm();

    Ok(ShellRun {
        status,
        timed_out: false,
        interrupted: None,
        out: shell_tx.display(&out),
        err: shell_tx.display(&err),
        captured: String::from_utf8_lossy(&captured).trim().to_string(),
    })
}

#[cfg(unix)]
pub(crate) fn kill_process_group(pid: u32) {
    unsafe {
        libc::killpg(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(not(unix))]
pub(crate) fn kill_process_group(_pid: u32) {}

/// Kills the process's group if the tool future is cancelled mid-run (turn
/// abort / stream cancel), so the child cannot outlive the request. Shared by
/// `run_shell` and the configured stdio tools.
pub(crate) struct KillGuard(Option<u32>);

impl KillGuard {
    pub(crate) fn new(pid: Option<u32>) -> Self {
        Self(pid)
    }

    pub(crate) fn disarm(&mut self) {
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

pub(crate) struct RunShell {
    shell_tx: ShellOutputTx,
    shell: Shell,
    access: Access,
}

impl RunShell {
    pub(crate) fn new(shell_tx: ShellOutputTx, shell: Shell, access: Access) -> Self {
        Self {
            shell_tx,
            shell,
            access,
        }
    }
}

impl Tool for RunShell {
    const NAME: &'static str = "run_shell";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Run a shell command line in the workspace, executed through the resolved shell (`{}`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`. Commands are permission-gated: a denied command interrupts the turn, whether denied upfront by the command text or killed mid-run because its output tripped a deny rule.",
            self.shell.invocation()
        )
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
            let access = self.access.clone();
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
            access.authorize_shell(&command).await?;
            let interrupt = access.output_interrupt();
            let run = run_shell_command(
                &self.shell,
                &command,
                &cwd_abs,
                Some(timeout_secs),
                &self.shell_tx,
                interrupt.as_ref(),
            )
            .await?;

            if let Some(reason) = &run.interrupted {
                access.trigger_cut();
                ctx.insert_result(ShellStreams {
                    stdout: format!("interrupted: {reason}\n{}", run.out),
                    stderr: run.err,
                });
                return Err(format!(
                    "shell command was killed: {reason}\n{}",
                    run.captured
                ));
            }

            if run.timed_out {
                let mut message = format!(
                    "shell command timed out after {timeout_secs}s (killed)"
                );
                if !run.captured.is_empty() {
                    message.push_str(&format!("\n{}", run.captured));
                }
                message.push_str(&format!(
                    "\n\n<shell_metadata>\nshell tool terminated the command after exceeding the {timeout_secs}s timeout. If this command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value.\n</shell_metadata>"
                ));
                ctx.insert_result(ShellStreams {
                    stdout: format!("timeout {timeout_secs}s:\n{}", run.out),
                    stderr: run.err,
                });
                return Err(message);
            }

            let status = run.status;
            let status_line = match status.code() {
                Some(code) => format!("exit {code}:"),
                None => format!("exit {status}:"),
            };
            ctx.insert_result(ShellStreams {
                stdout: format!("{status_line}\n{}", run.out),
                stderr: run.err,
            });
            if !status.success() {
                return Err(format!(
                    "shell exited with {status}:\n{}",
                    run.captured.chars().take(MAX_COMMAND_OUTPUT).collect::<String>()
                ));
            }
            if run.captured.is_empty() {
                Ok(ToolOutput::text(format!(
                    "shell exited with {status} (no output)"
                )))
            } else {
                Ok(ToolOutput::text(format!(
                    "{status_line}\n{}",
                    run.captured.chars().take(MAX_COMMAND_OUTPUT).collect::<String>()
                )))
            }
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
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

fn cap_buffer(buf: &mut Vec<u8>, cap: usize) {
    if buf.len() > cap {
        let drop = buf.len() - cap;
        buf.drain(..drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};
    use tempfile::TempDir;

    fn run_shell_tool() -> RunShell {
        RunShell::new(
            ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            crate::shell::resolve(None).shell,
            crate::test_util::access(),
        )
    }

    #[test]
    fn description_names_resolved_shell() {
        let description = run_shell_tool().description();
        assert!(
            description.contains(&crate::shell::resolve(None).shell.invocation()),
            "{description}"
        );
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
        let tool = RunShell::new(
            ShellOutputTx::new(tx),
            crate::shell::resolve(None).shell,
            crate::test_util::access(),
        );
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
        let tool = RunShell::new(
            ShellOutputTx::new(tx).tagged("run_tests"),
            crate::shell::resolve(None).shell,
            crate::test_util::access(),
        );
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
    async fn run_shell_command_runs_unbounded_without_timeout() {
        let (dir, _guard) = tempdir();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let shell = crate::shell::resolve(None).shell;
        let run = run_shell_command(
            &shell,
            "echo unbounded; sleep 1.5",
            dir.path(),
            None,
            &ShellOutputTx::new(tx),
            None,
        )
        .await
        .unwrap();
        assert!(!run.timed_out);
        assert!(run.status.success());
        assert_eq!(run.status.code(), Some(0));
        assert!(run.out.contains("unbounded"), "{}", run.out);
        let mut saw_stream = false;
        while let Ok(chunk) = rx.try_recv() {
            if chunk.stdout.contains("unbounded") {
                saw_stream = true;
            }
        }
        assert!(saw_stream, "unbounded runs still stream live tails");
        drop(dir);
    }

    #[tokio::test]
    async fn raw_capture_returns_the_full_untrimmed_output() {
        let (dir, _guard) = tempdir();
        let shell = crate::shell::resolve(None).shell;
        let run = run_shell_command(
            &shell,
            "printf '  keep  \\n\\nlines  '",
            dir.path(),
            None,
            &ShellOutputTx::full(tokio::sync::mpsc::channel(64).0),
            None,
        )
        .await
        .unwrap();
        assert_eq!(run.out, "  keep  \n\nlines  ");
        drop(dir);
    }

    #[tokio::test]
    async fn tail_capture_trims_and_caps_display() {
        let (dir, _guard) = tempdir();
        let shell = crate::shell::resolve(None).shell;
        let run = run_shell_command(
            &shell,
            "printf '  keep  \\n\\nlines  '",
            dir.path(),
            None,
            &ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            None,
        )
        .await
        .unwrap();
        assert_eq!(run.out, "keep  \n\nlines");
        drop(dir);
    }

    #[tokio::test]
    async fn raw_streams_carry_the_full_buffer() {
        let (dir, _guard) = tempdir();
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let shell = crate::shell::resolve(None).shell;
        let _ = run_shell_command(
            &shell,
            "echo leading-marker; echo trailing-marker",
            dir.path(),
            None,
            &ShellOutputTx::full(tx),
            None,
        )
        .await;
        let mut full_chunk = false;
        while let Ok(chunk) = rx.try_recv() {
            if chunk.stdout.contains("leading-marker") && chunk.stdout.contains("trailing-marker") {
                full_chunk = true;
            }
        }
        assert!(full_chunk, "raw chunks carry the whole capture, not a tail");
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_command_reports_failure_status() {
        let (dir, _guard) = tempdir();
        let shell = crate::shell::resolve(None).shell;
        let run = run_shell_command(
            &shell,
            "exit 7",
            dir.path(),
            None,
            &ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            None,
        )
        .await
        .unwrap();
        assert!(!run.status.success());
        assert_eq!(run.status.code(), Some(7));
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
    async fn run_shell_denies_deny_rule() {
        let (dir, _guard) = tempdir();
        let access = crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
            default: Some(shuvarie_config::Verb::Deny),
            shell: shuvarie_config::RuleSet::default(),
            ..shuvarie_config::PermissionsConfig::builtin()
        });
        let tool = RunShell::new(
            ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            crate::shell::resolve(None).shell,
            access,
        );
        let err = tool
            .call(&mut new_ctx(), json!({ "command": "echo hi" }))
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("permission denied"), "{message}");
        assert!(
            message.contains("permissions default: deny-all"),
            "{message}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_ask_allowed_and_denied_by_user() {
        let (dir, _guard) = tempdir();
        let (access, mut rx) =
            crate::test_util::access_with_answering_gate(&shuvarie_config::PermissionsConfig {
                default: Some(shuvarie_config::Verb::Ask),
                shell: shuvarie_config::RuleSet::default(),
                ..shuvarie_config::PermissionsConfig::builtin()
            });
        let tool = RunShell::new(
            ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            crate::shell::resolve(None).shell,
            access,
        );
        let ask = tokio::spawn(async move {
            tool.call(&mut new_ctx(), json!({ "command": "echo asked" }))
                .await
        });
        let request = rx.recv().await.unwrap();
        assert!(request.description.contains("Allow running this command?"));
        assert!(request.description.contains("echo asked"));
        request
            .respond
            .send(crate::permissions::PermissionAnswer::Allow)
            .unwrap();
        let out = ask.await.unwrap().unwrap();
        assert!(out.as_text().unwrap().contains("asked"));
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_ask_denied_by_user() {
        let (dir, _guard) = tempdir();
        let (access, mut rx) =
            crate::test_util::access_with_answering_gate(&shuvarie_config::PermissionsConfig {
                default: Some(shuvarie_config::Verb::Ask),
                shell: shuvarie_config::RuleSet::default(),
                ..shuvarie_config::PermissionsConfig::builtin()
            });
        let cut = access.turn_cut().clone();
        let tool = RunShell::new(
            ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            crate::shell::resolve(None).shell,
            access,
        );
        let ask = tokio::spawn(async move {
            tool.call(&mut new_ctx(), json!({ "command": "echo asked" }))
                .await
        });
        let request = rx.recv().await.unwrap();
        request
            .respond
            .send(crate::permissions::PermissionAnswer::Deny)
            .unwrap();
        let err = ask.await.unwrap().unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("permission denied by the user"),
            "{message}"
        );
        assert!(cut.is_set(), "a user rejection must flag the turn cut");
        drop(dir);
    }

    #[tokio::test]
    async fn run_shell_interrupts_on_output_match() {
        let (dir, _guard) = tempdir();
        let access = crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
            default: Some(shuvarie_config::Verb::Allow),
            shell: shuvarie_config::RuleSet {
                default: Some(shuvarie_config::Verb::Allow),
                rules: vec![shuvarie_config::ShellRule {
                    verb: shuvarie_config::Verb::Deny,
                    pattern: "secret-output".to_string(),
                    kind: shuvarie_config::ShellPatternKind::Raw,
                }],
            },
            ..shuvarie_config::PermissionsConfig::builtin()
        });
        let cut = access.turn_cut().clone();
        let tool = RunShell::new(
            ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
            crate::shell::resolve(None).shell,
            access,
        );
        let started = std::time::Instant::now();
        let err = tool
            .call(
                &mut new_ctx(),
                json!({ "command": "printf 'secret-%s\\n' output; sleep 30", "timeout_secs": 30 }),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("killed") && message.contains("secret"),
            "{message}"
        );
        assert!(cut.is_set(), "output-match deny must flag the turn cut");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "interrupt must kill promptly"
        );
        drop(dir);
    }
}
