use futures_core::Stream;
use serde_json::Value;
use shuvarie_catalog::TokenUsage;
use std::pin::Pin;

use crate::file_change::FileChange;

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
    Error {
        message: String,
    },
}

pub type StreamStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;
