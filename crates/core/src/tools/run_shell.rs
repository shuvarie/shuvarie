use serde_json::{Value, json};
use shuvarie_llm::{ShellStreams, Tool, ToolContext, ToolExecutionError, ToolOutput};

use std::path::Path;

use super::{DEFAULT_TIMEOUT_SECS, arg_value, workspace_root};
use crate::shell::Shell;

const MAX_COMMAND_OUTPUT: usize = 16 * 1024;
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

/// One finished shell run: the exit status, whether the run hit its timeout,
/// and the captured streams (`out`/`err` are display-capped, `captured` is the
/// trimmed full capture for error messages).
pub(crate) struct ShellRun {
    pub status: std::process::ExitStatus,
    pub timed_out: bool,
    pub out: String,
    pub err: String,
    pub captured: String,
}

/// Spawns `command` through `shell` in `cwd`, streaming live output tails
/// through `shell_tx` as they arrive. `timeout_secs: None` runs unbounded;
/// a timeout kills the process group and reports the partial capture.
pub(crate) async fn run_shell_command(
    shell: &Shell,
    command: &str,
    cwd: &Path,
    timeout_secs: Option<u64>,
    shell_tx: &ShellOutputTx,
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
                            shell_tx.send_streams(&out, &err).await;
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
                shell_tx.send_streams(&out, &err).await;
                guard.disarm();
                return Ok(ShellRun {
                    status,
                    timed_out: true,
                    out: display_stream(&out),
                    err: display_stream(&err),
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
        out: display_stream(&out),
        err: display_stream(&err),
        captured: String::from_utf8_lossy(&captured).trim().to_string(),
    })
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

pub(crate) struct RunShell {
    shell_tx: ShellOutputTx,
    shell: Shell,
}

impl RunShell {
    pub(crate) fn new(shell_tx: ShellOutputTx, shell: Shell) -> Self {
        Self { shell_tx, shell }
    }
}

impl Tool for RunShell {
    const NAME: &'static str = "run_shell";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        format!(
            "Run a shell command line in the workspace, executed through the resolved shell (`{}`). Pipes, redirects, and shell operators work naturally. Output streams live to the user while the command runs. Captured stdout and stderr (combined) are returned, capped at 16 KB. The command and its children are killed when it exceeds the timeout; if the command is expected to take longer and is not waiting for interactive input, retry with a larger timeout_secs value. The working directory can be set with `cwd`.",
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
            let run = run_shell_command(
                &self.shell,
                &command,
                &cwd_abs,
                Some(timeout_secs),
                &self.shell_tx,
            )
            .await?;

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

fn cap_buffer(buf: &mut Vec<u8>) {
    if buf.len() > SHELL_CAPTURE_BYTES {
        let drop = buf.len() - SHELL_CAPTURE_BYTES;
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
        let tool = RunShell::new(ShellOutputTx::new(tx), crate::shell::resolve(None).shell);
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
    async fn run_shell_command_reports_failure_status() {
        let (dir, _guard) = tempdir();
        let shell = crate::shell::resolve(None).shell;
        let run = run_shell_command(
            &shell,
            "exit 7",
            dir.path(),
            None,
            &ShellOutputTx::new(tokio::sync::mpsc::channel(64).0),
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
}
