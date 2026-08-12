use shuvarie_llm::ChatMsg;

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub messages: Vec<ChatMsg>,
    pub tokens: u64,
    pub cost: f64,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.tokens = 0;
        self.cost = 0.0;
    }

    pub fn push_user(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::user(content));
    }

    pub fn push_assistant(&mut self, content: impl Into<String>) {
        self.messages.push(ChatMsg::assistant(content));
    }
}
