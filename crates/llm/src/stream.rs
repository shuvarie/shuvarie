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
    Done {
        text: String,
        usage: TokenUsage,
    },
    /// The run was stopped because the context budget was exceeded. The
    /// caller should compact the session history before continuing.
    Overflow,
    Error {
        message: String,
    },
}

pub type StreamStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;
