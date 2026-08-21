use shuvarie_llm::Role;

#[derive(Debug, Clone, PartialEq, Eq, toasty::Embed)]
#[column(rename_all = "snake_case")]
pub enum MsgRole {
    System,
    User,
    Assistant,
}

impl From<Role> for MsgRole {
    fn from(role: Role) -> Self {
        match role {
            Role::System => Self::System,
            Role::User => Self::User,
            Role::Assistant => Self::Assistant,
        }
    }
}

impl From<MsgRole> for Role {
    fn from(role: MsgRole) -> Self {
        match role {
            MsgRole::System => Self::System,
            MsgRole::User => Self::User,
            MsgRole::Assistant => Self::Assistant,
        }
    }
}

impl MsgRole {
    pub fn from_str_loose(s: &str) -> Self {
        match s {
            "user" => Self::User,
            "assistant" => Self::Assistant,
            _ => Self::System,
        }
    }
}

#[derive(Debug, toasty::Model)]
pub struct Session {
    #[key]
    #[auto]
    pub id: u64,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    #[auto]
    pub created_at: jiff::Timestamp,
    #[auto]
    pub updated_at: jiff::Timestamp,
    #[has_many]
    pub messages: toasty::Deferred<Vec<Message>>,
}

#[derive(Debug, toasty::Model)]
pub struct Message {
    #[key]
    #[auto]
    pub id: u64,
    #[index]
    pub session_id: u64,
    #[belongs_to(key = session_id, references = id)]
    pub session: toasty::Deferred<Session>,
    pub seq: u64,
    pub role: MsgRole,
    pub content: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    #[has_many]
    pub embeddings: toasty::Deferred<Vec<MessageEmbedding>>,
}

#[derive(Debug, toasty::Model)]
pub struct MessageEmbedding {
    #[key]
    #[auto]
    pub id: u64,
    #[index]
    pub message_id: u64,
    #[belongs_to(key = message_id, references = id)]
    pub message: toasty::Deferred<Message>,
    #[index]
    pub session_id: u64,
    pub seq: u64,
    pub content: String,
    pub vec: Vec<u8>,
}
