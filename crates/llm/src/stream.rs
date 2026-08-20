use futures_core::Stream;
use serde_json::Value;
use shuvarie_catalog::TokenUsage;
use std::pin::Pin;

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
