use futures_core::Stream;
use serde_json::Value;
use std::pin::Pin;

use crate::usage::TokenUsage;

#[derive(Debug, Clone)]
pub enum StreamItem {
    Delta {
        text: String,
    },
    ToolStart {
        name: String,
        args: Value,
    },
    ToolResult {
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
