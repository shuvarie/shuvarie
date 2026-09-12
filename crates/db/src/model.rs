use shuvarie_llm::Role;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningSegment {
    #[serde(default)]
    pub after_tool: u64,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub duration_ms: u64,
}

pub(crate) fn encode_reasoning(segments: &[ReasoningSegment]) -> String {
    if segments.is_empty() {
        return String::new();
    }
    serde_json::to_string(segments).unwrap_or_default()
}

pub(crate) fn parse_reasoning(raw: &str) -> Vec<ReasoningSegment> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    if let Ok(segments) = serde_json::from_str::<Vec<ReasoningSegment>>(raw) {
        return segments;
    }
    vec![ReasoningSegment {
        after_tool: 0,
        text: raw.to_string(),
        duration_ms: 0,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reasoning_round_trips_segments() {
        let segments = vec![
            ReasoningSegment {
                after_tool: 0,
                text: "before".to_string(),
                duration_ms: 0,
            },
            ReasoningSegment {
                after_tool: 2,
                text: "after".to_string(),
                duration_ms: 0,
            },
        ];
        let raw = encode_reasoning(&segments);
        assert_eq!(parse_reasoning(&raw), segments);
    }

    #[test]
    fn parse_reasoning_falls_back_to_single_start_segment() {
        let raw = "plain streamed thinking";
        assert_eq!(
            parse_reasoning(raw),
            vec![ReasoningSegment {
                after_tool: 0,
                text: raw.to_string(),
                duration_ms: 0,
            }]
        );
    }

    #[test]
    fn parse_reasoning_empty_is_empty() {
        assert!(parse_reasoning("").is_empty());
        assert!(parse_reasoning("   ").is_empty());
        assert_eq!(encode_reasoning(&[]), "");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, toasty::Embed)]
#[column(rename_all = "snake_case")]
pub enum SessionType {
    Main,
    Worker,
}

#[derive(Debug, toasty::Model)]
pub struct Session {
    #[key]
    #[auto(uuid(v7))]
    pub id: uuid::Uuid,
    pub title: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub session_type: SessionType,
    #[index]
    pub parent_id: Option<uuid::Uuid>,
    #[belongs_to(key = parent_id, references = id)]
    pub parent: toasty::Deferred<Session>,
    #[auto]
    pub created_at: jiff::Timestamp,
    #[auto]
    pub updated_at: jiff::Timestamp,
    /// Chat pane was pinned to the bottom of the history when the session was
    /// last left.
    #[default(false)]
    pub scroll_sticky: bool,
    /// Scroll anchor turn (dense message index) the viewport top was held at
    /// when released from the bottom.
    pub scroll_turn: Option<u64>,
    /// Scroll anchor wrapped row within `scroll_turn`.
    pub scroll_row: Option<u64>,
    #[has_many]
    pub messages: toasty::Deferred<Vec<Message>>,
}

#[derive(Debug, toasty::Model)]
pub struct Message {
    #[key]
    #[auto]
    pub id: u64,
    #[index]
    pub session_id: uuid::Uuid,
    #[belongs_to(key = session_id, references = id)]
    pub session: toasty::Deferred<Session>,
    pub seq: u64,
    pub role: MsgRole,
    pub content: String,
    pub reasoning: String,
    pub interrupted: bool,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub reasoning_tokens: u64,
    pub cost: f64,
    pub summary: bool,
    /// Usage of the turn's last main-stream request (JSON), kept apart from
    /// the combined per-row totals so a loaded session can restore the
    /// main-request context footprint and cache metrics.
    pub request_json: String,
    #[has_many]
    pub embeddings: toasty::Deferred<Vec<MessageEmbedding>>,
    #[has_many]
    pub tool_calls: toasty::Deferred<Vec<ToolCall>>,
}

#[derive(Debug, toasty::Model)]
pub struct ToolCall {
    #[key]
    #[auto]
    pub id: u64,
    #[index]
    pub session_id: uuid::Uuid,
    #[index]
    pub message_id: u64,
    #[belongs_to(key = message_id, references = id)]
    pub message: toasty::Deferred<Message>,
    pub seq: u64,
    pub name: String,
    pub args_json: String,
    pub output: String,
    pub ok: bool,
    /// The call was cut off before it could finish (turn interrupted).
    pub killed: bool,
    pub worker: Option<String>,
    pub file_change_json: String,
    pub original_content: Option<String>,
    pub new_content: Option<String>,
    pub stderr: String,
    pub duration_ms: u64,
}

#[derive(Debug, toasty::Model)]
pub struct UndoLog {
    #[key]
    #[auto]
    pub id: u64,
    #[index]
    pub session_id: uuid::Uuid,
    pub turn_seq: u64,
    pub user_content: String,
    pub assistant_content: String,
    pub reasoning: String,
    pub usage_json: String,
    pub tool_calls_json: String,
    pub file_changes_json: String,
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
    pub session_id: uuid::Uuid,
    pub seq: u64,
    pub content: String,
    pub vec: Vec<u8>,
}
