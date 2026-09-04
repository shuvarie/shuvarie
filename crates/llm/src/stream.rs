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
    },
    ToolResult {
        name: String,
        output: String,
        ok: bool,
        worker: Option<String>,
        file_change: Option<FileChange>,
        streams: Option<ShellStreams>,
    },
    WorkerStart {
        name: String,
        args: Value,
    },
    WorkerResult {
        name: String,
        output: String,
        ok: bool,
    },
    /// Usage for one completed provider request within the run. Emitted once
    /// per agent iteration (main stream and workers), so consumers can track
    /// token consumption before the run finishes.
    Usage {
        usage: TokenUsage,
    },
    Done {
        text: String,
        usage: TokenUsage,
    },
    /// The run was stopped because the context budget was exceeded. The
    /// caller should compact the session history before continuing.
    Overflow,
    /// A transport-level connection failure (timeout, reset, refused, HTTP
    /// 408/429/5xx). The caller may retry the turn after a backoff.
    ConnectionError {
        message: String,
        reason: String,
    },
    Error {
        message: String,
    },
}

pub type StreamStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;
