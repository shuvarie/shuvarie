#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
}

impl TokenUsage {
    pub fn from_rig(usage: rig::completion::Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage.cached_input_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }

    pub fn has_values(&self) -> bool {
        self.total_tokens != 0
            || self.input_tokens != 0
            || self.output_tokens != 0
            || self.cached_input_tokens != 0
            || self.reasoning_tokens != 0
    }
}
