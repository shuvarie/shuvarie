//! The headless `run` subcommand: drives the core with a single prompt and no
//! TUI, streaming the assistant reply to stdout and status lines to stderr.

use std::io::Write;

use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::JoinHandle;

use shuvarie_core::{Command, Event, PermissionAnswer};

use crate::cli::RunArgs;

/// Runs one prompt to completion. Assistant text goes to stdout — flushed as
/// it arrives, so it pipes cleanly — while tool activity, permission decisions,
/// retry notices, and the resume hint go to stderr. Returns an error, and so a
/// non-zero exit, when the turn fails.
pub async fn run_cli(
    args: RunArgs,
    cmd_tx: Sender<Command>,
    mut event_rx: Receiver<Event>,
    core: JoinHandle<()>,
) -> color_eyre::Result<()> {
    // Enter headless mode before the prompt, so the turn's roster and preamble
    // are built without the `question` tool and with the non-interactive note.
    let _ = cmd_tx.send(Command::SetHeadless).await;
    if cmd_tx
        .send(Command::SendMessage {
            content: args.prompt.join(" "),
            attachments: Vec::new(),
            model: None,
        })
        .await
        .is_err()
    {
        return Err(color_eyre::eyre::eyre!("core task is not running"));
    }

    let mut session_id = None;
    let mut stdout_used = false;
    let mut trailing_newline = true;
    let mut result: Option<Outcome> = None;

    while let Some(event) = event_rx.recv().await {
        if let Some(terminal) = terminal_outcome(&event) {
            result = Some(terminal);
            break;
        }
        match event {
            Event::SessionCreated { id, .. } => session_id = Some(id),
            Event::TokenReceived { content } => {
                stdout_used = true;
                trailing_newline = content.ends_with('\n');
                print!("{content}");
                let _ = std::io::stdout().flush();
            }
            Event::ToolStarted {
                name,
                worker,
                waiting,
                ..
            } => {
                eprintln!("{}", tool_started_line(&name, worker.as_deref(), waiting));
            }
            // A queued call's turn came: it is starting now, so it gets the same
            // line any other call that begins executing gets.
            Event::ToolRunning { name, worker, .. } => {
                eprintln!("{}", tool_started_line(&name, worker.as_deref(), false));
            }
            Event::ToolFinished {
                name,
                ok,
                worker,
                duration_ms,
                ..
            } => {
                eprintln!(
                    "{}",
                    tool_finished_line(&name, worker.as_deref(), ok, duration_ms)
                );
            }
            Event::WorkerStarted { name, waiting, .. } => {
                let suffix = if waiting { ", queued" } else { "" };
                eprintln!("  ▸ {name} (worker{suffix})");
            }
            Event::WorkerFinished {
                name,
                ok,
                duration_ms,
                ..
            } => {
                let mark = if ok { "✓" } else { "✗" };
                eprintln!("  {mark} {name} (worker, {duration_ms}ms)");
            }
            Event::PermissionRequested {
                id,
                description,
                allow_dir,
                ..
            } => {
                let decision = args.permission_handle.decision(allow_dir);
                eprintln!("permission: {description} -> {}", decision_label(decision));
                let _ = cmd_tx
                    .send(Command::PermissionDecide { id, decision })
                    .await;
            }
            Event::QuestionAsked { id, .. } => {
                // The headless roster omits the `question` tool, so this should
                // not happen; answer defensively so a stray ask cannot hang.
                eprintln!("warning: a question was asked in run mode; dismissing it");
                let _ = cmd_tx
                    .send(Command::AnswerQuestion { id, answers: None })
                    .await;
            }
            Event::RetryScheduled {
                reason,
                attempt,
                max_attempts,
                delay_ms,
                ..
            } => {
                eprintln!("retrying ({reason}): attempt {attempt}/{max_attempts} in {delay_ms}ms");
            }
            Event::CompactionStarted => eprintln!("compacting context…"),
            Event::ShellWarning { message } => eprintln!("warning: {message}"),
            Event::ConfigError { error } => eprintln!("config error: {error}"),
            Event::McpError { error } => eprintln!("MCP error: {error}"),
            Event::AttachmentNotice { text } => eprintln!("{text}"),
            _ => {}
        }
    }

    // Drop our end of the command channel so the core's select loop breaks and
    // it shuts down (releasing the session lock, stopping LSP/MCP) before we
    // await it.
    drop(cmd_tx);
    core.await
        .map_err(|e| color_eyre::eyre::eyre!("core task panicked: {e}"))?;

    if stdout_used && !trailing_newline {
        println!();
    }

    match result {
        Some(Outcome::Done) => {
            if let Some(id) = session_id
                && !args.transient
            {
                for line in crate::cli::resume_hint_lines(id) {
                    eprintln!("{line}");
                }
            }
            Ok(())
        }
        Some(Outcome::Cancelled) => {
            eprintln!("run cancelled");
            Ok(())
        }
        Some(Outcome::Fatal(error)) => Err(color_eyre::eyre::eyre!("{error}")),
        None => Err(color_eyre::eyre::eyre!(
            "core task exited before the run completed"
        )),
    }
}

/// How a run ended.
#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The turn finished normally.
    Done,
    /// The turn was cancelled (e.g. the user interrupted it).
    Cancelled,
    /// The turn failed; the message is reported to the user.
    Fatal(String),
}

/// The outcome an event ends the run with, or `None` when the run continues.
/// The turn's terminal events are `StreamDone` (clean), `StreamCancelled`, and
/// the `StreamError`/`SessionError` failures.
fn terminal_outcome(event: &Event) -> Option<Outcome> {
    match event {
        Event::StreamDone { .. } => Some(Outcome::Done),
        Event::StreamCancelled => Some(Outcome::Cancelled),
        Event::StreamError { error } => Some(Outcome::Fatal(error.clone())),
        Event::SessionError { error } => Some(Outcome::Fatal(error.clone())),
        _ => None,
    }
}

/// The line for a call the model emitted. A queued one has not begun — the
/// calls before it hold every concurrency slot — so it is marked instead of
/// reported as started; [`Event::ToolRunning`] reports the same line once it
/// really begins.
fn tool_started_line(name: &str, worker: Option<&str>, waiting: bool) -> String {
    let prefix = match worker {
        Some(worker) => format!("  [worker {worker}] "),
        None => "  ".to_string(),
    };
    if waiting {
        format!("{prefix}{name} (queued)")
    } else {
        format!("{prefix}{name} …")
    }
}

fn tool_finished_line(name: &str, worker: Option<&str>, ok: bool, duration_ms: u64) -> String {
    let mark = if ok { "✓" } else { "✗" };
    match worker {
        Some(worker) => format!("  {mark} [worker {worker}] {name} ({duration_ms}ms)"),
        None => format!("  {mark} {name} ({duration_ms}ms)"),
    }
}

fn decision_label(decision: PermissionAnswer) -> &'static str {
    match decision {
        PermissionAnswer::Allow => "allow",
        PermissionAnswer::AllowSession => "allow (session)",
        PermissionAnswer::AllowDirSession => "allow (folder, session)",
        PermissionAnswer::Deny => "deny",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::PermissionHandle;

    #[test]
    fn permission_handle_maps_to_the_gate_answer() {
        assert_eq!(
            PermissionHandle::DenyAsk.decision(false),
            PermissionAnswer::Deny
        );
        assert_eq!(
            PermissionHandle::AllowAsk.decision(false),
            PermissionAnswer::Allow
        );
        // `allow-all` widens path asks to their folder, and otherwise remembers
        // the ask's own scope.
        assert_eq!(
            PermissionHandle::AllowAll.decision(false),
            PermissionAnswer::AllowSession
        );
        assert_eq!(
            PermissionHandle::AllowAll.decision(true),
            PermissionAnswer::AllowDirSession
        );
    }

    #[test]
    fn terminal_events_end_the_run() {
        assert_eq!(
            terminal_outcome(&Event::TokenReceived {
                content: "hi".into()
            }),
            None
        );
        assert_eq!(
            terminal_outcome(&Event::StreamCancelled),
            Some(Outcome::Cancelled)
        );
        assert_eq!(
            terminal_outcome(&Event::StreamError {
                error: "boom".into()
            }),
            Some(Outcome::Fatal("boom".into()))
        );
        assert_eq!(
            terminal_outcome(&Event::SessionError {
                error: "nope".into()
            }),
            Some(Outcome::Fatal("nope".into()))
        );
    }

    #[test]
    fn tool_lines_name_the_worker_when_present() {
        assert_eq!(tool_started_line("read_file", None, false), "  read_file …");
        assert_eq!(
            tool_started_line("read_file", Some("explore"), false),
            "  [worker explore] read_file …"
        );
        // A queued call has not started: it is marked rather than reported as
        // running, so a piped log does not claim work the model has not reached.
        assert_eq!(
            tool_started_line("read_file", None, true),
            "  read_file (queued)"
        );
        assert_eq!(
            tool_started_line("read_file", Some("explore"), true),
            "  [worker explore] read_file (queued)"
        );
        assert_eq!(
            tool_finished_line("read_file", None, true, 12),
            "  ✓ read_file (12ms)"
        );
        assert_eq!(
            tool_finished_line("run_tests", Some("tester"), false, 340),
            "  ✗ [worker tester] run_tests (340ms)"
        );
    }
}
