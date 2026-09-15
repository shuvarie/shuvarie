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

/// One assistant text run with the number of tool calls that completed before
/// it started, so a reload can rebuild the interleave the stream showed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextSegment {
    #[serde(default)]
    pub after_tool: u64,
    #[serde(default)]
    pub text: String,
}

pub(crate) fn encode_text_segments(segments: &[TextSegment]) -> String {
    if segments.is_empty() {
        return String::new();
    }
    serde_json::to_string(segments).unwrap_or_default()
}

pub(crate) fn parse_text_segments(raw: &str) -> Vec<TextSegment> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<TextSegment>>(raw).unwrap_or_default()
}

/// The turn's text runs fused back into the stored message `content`: the
/// paragraph break between runs travels inside the stream deltas, so the
/// runs concatenate byte-identically to the joined turn text.
pub fn join_text_segments(segments: &[TextSegment]) -> String {
    let mut text = String::new();
    for seg in segments {
        text.push_str(&seg.text);
    }
    text
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
    fn parse_text_segments_round_trips_and_ignores_garbage() {
        let segments = vec![
            TextSegment {
                after_tool: 0,
                text: "before".to_string(),
            },
            TextSegment {
                after_tool: 2,
                text: "after".to_string(),
            },
        ];
        let raw = encode_text_segments(&segments);
        assert_eq!(parse_text_segments(&raw), segments);
        assert!(parse_text_segments("").is_empty());
        assert!(parse_text_segments("   ").is_empty());
        assert!(parse_text_segments("plain text").is_empty());
        assert_eq!(encode_text_segments(&[]), "");
    }

    #[test]
    fn join_text_segments_concatenates_the_separator_bearing_runs() {
        let segments = vec![
            TextSegment {
                after_tool: 0,
                text: "start".to_string(),
            },
            TextSegment {
                after_tool: 1,
                text: "\n\nresumed".to_string(),
            },
        ];
        assert_eq!(join_text_segments(&segments), "start\n\nresumed");
        assert_eq!(join_text_segments(&[]), "");
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
    /// The message id of the active branch's tip; the chain from it up to the
    /// root (via `messages.parent_id`) is the conversation's active path.
    pub leaf_id: Option<u64>,
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
    /// Parent message in the session tree: `None` for root prompts, else the
    /// id of the preceding node (user → assistant → user → …). A branch point
    /// is a node with several children; the active path runs from
    /// `sessions.leaf_id` up to the root.
    #[index]
    pub parent_id: Option<u64>,
    #[belongs_to(key = parent_id, references = id)]
    pub parent: toasty::Deferred<Message>,
    pub seq: u64,
    pub role: MsgRole,
    pub content: String,
    pub reasoning: String,
    /// JSON of [`TextSegment`]s: the turn's text runs with the tool-call
    /// positions they streamed at, so a reload rebuilds the interleave.
    pub text_segments: String,
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
