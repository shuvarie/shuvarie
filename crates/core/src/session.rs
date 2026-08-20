use shuvarie_catalog::TokenUsage;
use shuvarie_db::StoredSession;
use shuvarie_llm::ChatMsg;

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub id: Option<u64>,
    pub title: Option<String>,
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

    pub fn from_stored(stored: StoredSession) -> Self {
        let mut s = Self {
            id: Some(stored.id),
            title: Some(stored.title),
            ..Self::default()
        };
        for m in &stored.messages {
            s.input_tokens = s.input_tokens.saturating_add(m.input_tokens);
            s.output_tokens = s.output_tokens.saturating_add(m.output_tokens);
            s.tokens = s.tokens.saturating_add(m.total_tokens);
            s.reasoning_tokens = s.reasoning_tokens.saturating_add(m.reasoning_tokens);
            s.cached_tokens = s.cached_tokens.saturating_add(m.cached_input_tokens);
            s.cost += m.cost;
        }
        s.messages = stored
            .messages
            .into_iter()
            .map(|m| ChatMsg {
                role: m.role.into(),
                content: m.content,
            })
            .collect();
        s
    }

    pub fn clear(&mut self) {
        self.id = None;
        self.title = None;
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
