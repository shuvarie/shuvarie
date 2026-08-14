use shuvarie_llm::{ChatMsg, TokenUsage};

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub messages: Vec<ChatMsg>,
    pub tokens: u64,
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.tokens = 0;
        self.cost = 0.0;
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.reasoning_tokens = 0;
        self.cached_tokens = 0;
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::user(content));
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::assistant(content));
    }

    pub fn add_usage(&mut self, usage: TokenUsage, cost: f64) {
        self.tokens = self.tokens.saturating_add(usage.total_tokens);
        self.cost += cost;
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(usage.cached_input_tokens);
    }
}
