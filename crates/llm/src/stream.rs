use crate::TokenUsage;
use futures_core::Stream;
use serde_json::Value;
use std::pin::Pin;

use crate::file_change::{FileChange, ShellStreams};

#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    Delta {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolStart {
        name: String,
        args: Value,
        worker: Option<String>,
        /// The spawn id ([`crate::agent::SpawnTag`]) of the worker run the call
        /// happened inside: workers run single-shot, so their whole run — the
        /// request and every activity item it emits — shares one id; `None`
        /// marks the main agent's own calls.
        spawn: Option<u64>,
        /// The provider call id (`internal_call_id`) that ties the result back
        /// to its own start when a model batches several calls of one tool.
        call_id: String,
    },
    ToolResult {
        name: String,
        output: String,
        ok: bool,
        worker: Option<String>,
        /// The spawn id of the worker run the call happened inside (`None` for
        /// the main agent's own calls).
        spawn: Option<u64>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
        call_id: String,
    },
    WorkerStart {
        name: String,
        args: Value,
        call_id: String,
    },
    WorkerResult {
        name: String,
        output: String,
        ok: bool,
        /// The spawn id the tool minted for the run — the same id every
        /// activity item of that run carries.
        spawn: Option<u64>,
        call_id: String,
    },
    /// A live `run_shell` output chunk, merged into the turn stream from the
    /// shared shell-output channel. `worker` tags the owning agent (`None` for
    /// the main agent); `command` is the exact command line of the call that
    /// produced it, so a consumer holding the pending calls' args can resolve
    /// the chunk to its own call even when an agent runs several shells
    /// concurrently. Concurrent worker spawns of the same agent running the
    /// same command are the one remaining ambiguity: the chunk then resolves
    /// to `None` and the consumer falls back to the name match.
    ShellOutput {
        worker: Option<String>,
        command: String,
        stdout: String,
        stderr: String,
    },
    /// Usage for one completed provider request within the run. Emitted once
    /// per agent iteration (main stream and workers), so consumers can track
    /// token consumption before the run finishes. `worker` is `None` for the
    /// main stream's requests — the only ones whose context is the
    /// conversation itself, so consumers can anchor context-occupancy
    /// displays on them alone.
    Usage {
        usage: TokenUsage,
        worker: Option<String>,
    },
    Done {
        text: String,
        usage: TokenUsage,
    },
    /// The run was stopped because the context budget was exceeded. The
    /// caller should compact the session history before continuing.
    Overflow,
    /// A failure from the turn: a connection loss (timeout, reset, refused,
    /// HTTP 408/429/5xx), a provider API error, a malformed tool call, a
    /// worker failure, ... Every turn error leads to the timeout-retry flow
    /// in core (the turn is resumed after a backoff); `reason` is a short
    /// label for the retry status row.
    Error {
        message: String,
        reason: String,
    },
}

pub type StreamStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;
