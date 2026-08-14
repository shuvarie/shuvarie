use futures_core::Stream;
use std::pin::Pin;

use crate::usage::TokenUsage;

#[derive(Debug, Clone)]
pub enum StreamItem {
    Delta { text: String },
    Done { text: String, usage: TokenUsage },
    Error { message: String },
}

pub type StreamStream = Pin<Box<dyn Stream<Item = StreamItem> + Send>>;
