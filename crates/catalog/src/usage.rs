use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl TokenUsage {
    pub fn has_values(&self) -> bool {
        self.total_tokens != 0
            || self.input_tokens != 0
            || self.output_tokens != 0
            || self.cached_input_tokens != 0
            || self.reasoning_tokens != 0
    }
}
