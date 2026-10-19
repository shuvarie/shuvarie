use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One tool call a request's history replays for an assistant message that ran
/// tools. A session stores an assistant turn as its text; the request builder
/// splices the turn's calls back in at the positions the turn's text runs
/// recorded, so a follow-up turn (or a resume) carries the tool activity of the
/// turns before it instead of their text alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatToolCall {
    /// The call's id, echoed by the [`ChatToolResult`] answering it so the two
    /// pair up on the wire.
    pub id: String,
    /// The tool (or worker) the call named.
    pub name: String,
    /// The call's JSON arguments, as recorded.
    pub args_json: String,
}

/// One tool result answering a [`ChatToolCall`] in the same request history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatToolResult {
    /// The id of the call this answers (its [`ChatToolCall::id`]).
    pub call: String,
    pub name: String,
    /// The tool's model-facing output.
    pub output: String,
    /// Whether the call failed (or never returned).
    pub is_error: bool,
}

/// One message of the chat history sent to the LLM. `attachments` carries
/// the message's attachment metadata (empty for plain text turns); the rig
/// conversion turns them into multimodal parts when their blob content is
/// supplied alongside (see [`to_rig_message`]). `tool_calls` (assistant) and
/// `tool_results` (user) carry the replayed tool activity of earlier turns —
/// both empty on the messages a session stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<crate::attachment::Attachment>,
    /// The tool calls this assistant message requested.
    #[serde(default)]
    pub tool_calls: Vec<ChatToolCall>,
    /// The tool results this user message answers calls with.
    #[serde(default)]
    pub tool_results: Vec<ChatToolResult>,
}

impl ChatMsg {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            attachments: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }

    /// An assistant message of `content` that requested `tool_calls` (either
    /// may be empty on its own, but not both).
    pub fn assistant_with(content: impl Into<String>, tool_calls: Vec<ChatToolCall>) -> Self {
        Self {
            tool_calls,
            ..Self::assistant(content)
        }
    }

    /// A user message carrying `results` — the answers to the tool calls of
    /// the assistant message before it.
    pub fn tool_results(results: Vec<ChatToolResult>) -> Self {
        Self {
            tool_results: results,
            ..Self::user("")
        }
    }

    /// Whether the message is a user prompt rather than a user message
    /// carrying the tool results of a replayed turn.
    pub fn is_user_prompt(&self) -> bool {
        self.role == Role::User && self.tool_results.is_empty()
    }
}

/// The multimodal renderer: a chat message becomes a rig message whose user
/// variant is a list of content parts — the prompt text, one image part per
/// image attachment (base64 data from `blobs`, media type from metadata), and
/// one text part per document attachment: documents are pre-converted to
/// markdown when attached, so their blob content is markdown text wrapped in
/// a `<document>` tag. Attachments whose blob is missing (pruned, or a
/// metadata-only import) degrade to an `[image … content unavailable]`
/// note — never silently omitted. A message carrying replayed tool activity
/// renders as the rig tool structures the target providers expect: an
/// assistant turn of its text plus one block per tool call, and a user message
/// of the tool results answering the preceding turn's calls.
pub fn to_rig_message(
    msg: ChatMsg,
    blobs: &crate::attachment::Blobs,
) -> rig_core::message::Message {
    match msg.role {
        Role::System => rig_core::message::Message::System {
            content: msg.content,
        },
        Role::User if msg.tool_results.is_empty() => rig_core::message::Message::User {
            content: user_content_parts(&msg, blobs),
        },
        Role::User => rig_core::message::Message::User {
            content: tool_result_parts(&msg),
        },
        Role::Assistant => rig_core::message::Message::Assistant(
            rig_core::message::AssistantMessage::new(assistant_parts(&msg)),
        ),
    }
}

/// The blocks of one assistant message: its text, then one call per replayed
/// tool call. A message with neither still sends a blank text block, so a row
/// with no content keeps the shape it had before tool replay existed.
fn assistant_parts(msg: &ChatMsg) -> Vec<rig_core::message::AssistantContent> {
    use rig_core::message::AssistantContent;

    let mut parts = Vec::with_capacity(1 + msg.tool_calls.len());
    if !msg.content.is_empty() {
        parts.push(AssistantContent::text(msg.content.clone()));
    }
    for call in &msg.tool_calls {
        parts.push(AssistantContent::tool_call(
            call.id.clone(),
            tool_name(&call.name),
            arguments_value(&call.args_json),
        ));
    }
    if parts.is_empty() {
        parts.push(AssistantContent::text(String::new()));
    }
    parts
}

/// The parts of one user message carrying replayed tool results: the results
/// first (Anthropic requires them directly after the calling turn), then any
/// text the message holds.
fn tool_result_parts(msg: &ChatMsg) -> Vec<rig_core::message::UserContent> {
    use rig_core::message::{CallId, ToolResult, ToolResultContent, UserContent};

    let mut parts = Vec::with_capacity(msg.tool_results.len() + 1);
    for result in &msg.tool_results {
        parts.push(UserContent::ToolResult(ToolResult {
            call: CallId::from_wire(result.call.clone()),
            name: tool_name(&result.name),
            content: vec![ToolResultContent::text(result.output.clone())],
            is_error: result.is_error,
        }));
    }
    if !msg.content.is_empty() {
        parts.push(UserContent::text(msg.content.clone()));
    }
    parts
}

/// A recorded tool name as rig's [`ToolName`](rig_core::message::ToolName). A
/// blank or unusable name degrades to a placeholder: a replayed turn must
/// never fail the request it rides in.
fn tool_name(name: &str) -> rig_core::message::ToolName {
    rig_core::message::ToolName::new(name)
        .unwrap_or_else(|_| rig_core::message::ToolName::new("unknown").expect("non-empty"))
}

/// A call's recorded JSON arguments as rig expects them. Malformed or missing
/// arguments become an empty object, which rig accepts, rather than failing the
/// request.
fn arguments_value(args_json: &str) -> serde_json::Value {
    serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null)
}

impl From<ChatMsg> for rig_core::message::Message {
    /// Same conversion with no blob content available: every image and
    /// document attachment degrades to a content-unavailable text note, so
    /// callers without blob access never lose the attachment context.
    fn from(msg: ChatMsg) -> Self {
        to_rig_message(msg, &crate::attachment::Blobs::new())
    }
}

/// The user content parts of one message: the prompt text plus one
/// multimodal part per attachment.
fn user_content_parts(
    msg: &ChatMsg,
    blobs: &crate::attachment::Blobs,
) -> Vec<rig_core::message::UserContent> {
    use base64::Engine as _;
    use rig_core::message::MimeType;
    use rig_core::message::{Image, ImageDetail, UserContent};

    let mut parts = Vec::with_capacity(1 + msg.attachments.len());
    if !msg.content.is_empty() || msg.attachments.is_empty() {
        parts.push(rig_core::message::UserContent::text(msg.content.clone()));
    }
    for attachment in &msg.attachments {
        match attachment.kind {
            crate::attachment::AttachmentKind::Image => match blobs.get(&attachment.sha256) {
                Some(bytes) => {
                    match rig_core::message::ImageMediaType::from_mime_type(&attachment.media_type)
                    {
                        Some(media_type) => parts.push(UserContent::Image(Image {
                            data: rig_core::message::DocumentSourceKind::Base64(
                                base64::engine::general_purpose::STANDARD.encode(bytes),
                            ),
                            media_type: Some(media_type),
                            detail: Some(ImageDetail::Auto),
                            native: None,
                        })),
                        // An image attachment with a media type rig does not
                        // classify as a recognized image is a data bug; degrade
                        // to the note rather than sending a mislabeled part.
                        None => parts.push(unavailable_note(attachment, "unrecognized media type")),
                    }
                }
                None => parts.push(unavailable_note(attachment, "content unavailable")),
            },
            crate::attachment::AttachmentKind::Document => {
                match blobs.get(&attachment.sha256).and_then(|bytes| {
                    String::from_utf8(bytes.clone())
                        .ok()
                        .filter(|text| !text.is_empty())
                }) {
                    Some(text) => parts.push(rig_core::message::UserContent::Text(
                        rig_core::message::Text::new(format!(
                            "<document name=\"{}\">\n{text}\n</document>",
                            attachment.name
                        )),
                    )),
                    None => parts.push(unavailable_note(attachment, "content unavailable")),
                }
            }
        }
    }
    parts
}

/// The content note a non-sent attachment leaves behind in the message.
fn unavailable_note(
    attachment: &crate::attachment::Attachment,
    reason: &str,
) -> rig_core::message::UserContent {
    rig_core::message::UserContent::text(format!(
        "[{} {:?} — {}]",
        attachment.kind, attachment.name, reason
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_msg_deserializes_without_an_attachments_key() {
        let json = r#"{"role": "user", "content": "hi"}"#;
        let msg: ChatMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.content, "hi");
        assert!(msg.attachments.is_empty());
    }

    #[test]
    fn chat_msg_deserializes_attachments() {
        let json = r#"{"role": "user", "content": "hi", "attachments": [{"kind": "image", "name": "a.png", "media_type": "image/png", "size": 2, "sha256": "ab"}]}"#;
        let msg: ChatMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.attachments.len(), 1);
        assert_eq!(
            msg.attachments[0].kind,
            crate::attachment::AttachmentKind::Image
        );
        assert_eq!(msg.attachments[0].sha256, "ab");
    }

    #[test]
    fn prompt_image_renders_a_multimodal_part() {
        let mut msg = ChatMsg::user("what is this?");
        msg.attachments = vec![crate::attachment::Attachment {
            kind: crate::attachment::AttachmentKind::Image,
            name: "a.png".into(),
            media_type: "image/png".into(),
            size: 2,
            sha256: "ab".into(),
        }];
        let blobs = crate::attachment::Blobs::from([("ab".to_string(), b"png bytes".to_vec())]);
        let rig_msg = crate::message::to_rig_message(msg, &blobs);
        match rig_msg {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 2, "text + image");
                assert_eq!(
                    content[0],
                    rig_core::message::UserContent::text("what is this?")
                );
                match &content[1] {
                    rig_core::message::UserContent::Image(image) => {
                        let rig_core::message::DocumentSourceKind::Base64(data) = &image.data
                        else {
                            panic!("base64 source");
                        };
                        assert_eq!(
                            *data,
                            base64::engine::general_purpose::STANDARD.encode(b"png bytes")
                        );
                        use base64::Engine as _;
                        use rig_core::message::MimeType;
                        assert_eq!(
                            image.media_type.as_ref().map(|mt| mt.to_mime_type()),
                            Some("image/png")
                        );
                        assert!(matches!(
                            image.detail,
                            Some(rig_core::message::ImageDetail::Auto)
                        ));
                    }
                    other => panic!("expected image part, got {other:?}"),
                }
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn document_attachment_renders_as_tagged_document_text() {
        let mut msg = ChatMsg::user("summarize");
        msg.attachments = vec![crate::attachment::Attachment {
            kind: crate::attachment::AttachmentKind::Document,
            name: "spec.pdf".into(),
            media_type: "application/pdf".into(),
            size: 700,
            sha256: "cd".into(),
        }];
        let blobs = crate::attachment::Blobs::from([
            ("cd".to_string(), b"# Report\nbody".to_vec()),
            ("zz".to_string(), b"ignored".to_vec()),
        ]);
        match crate::message::to_rig_message(msg, &blobs) {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 2, "text + document");
                match &content[1] {
                    rig_core::message::UserContent::Text(text) => {
                        assert_eq!(
                            text.text,
                            "<document name=\"spec.pdf\">\n# Report\nbody\n</document>"
                        );
                    }
                    other => panic!("expected text part, got {other:?}"),
                }
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn missing_blob_degrades_to_a_note() {
        let mut msg = ChatMsg::user("again");
        msg.attachments = vec![
            crate::attachment::Attachment {
                kind: crate::attachment::AttachmentKind::Image,
                name: "shot.png".into(),
                media_type: "image/png".into(),
                size: 2,
                sha256: "missing".into(),
            },
            crate::attachment::Attachment {
                kind: crate::attachment::AttachmentKind::Image,
                name: "odd.png".into(),
                media_type: "weird/unknown".into(),
                size: 2,
                sha256: "zz".into(),
            },
        ];
        let blobs = crate::attachment::Blobs::from([("zz".to_string(), b"bytes".to_vec())]);
        match crate::message::to_rig_message(msg, &blobs) {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 3, "text + two notes (no image parts)");
                assert_eq!(
                    content[1],
                    rig_core::message::UserContent::text(
                        "[image \"shot.png\" \u{2014} content unavailable]"
                    )
                );
                assert_eq!(
                    content[2],
                    rig_core::message::UserContent::text(
                        "[image \"odd.png\" \u{2014} unrecognized media type]"
                    )
                );
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    #[test]
    fn replayed_tool_calls_and_results_render_as_rig_structures() {
        use rig_core::message::{AssistantContent, UserContent};

        let mut call = crate::message::ChatMsg::assistant_with(
            "reading it",
            vec![crate::message::ChatToolCall {
                id: "call_1_0".into(),
                name: "read_file".into(),
                args_json: r#"{"path":"src/main.rs"}"#.into(),
            }],
        );
        call.attachments = Vec::new();
        let blobs = crate::attachment::Blobs::new();
        match crate::message::to_rig_message(call, &blobs) {
            rig_core::message::Message::Assistant(turn) => {
                assert_eq!(turn.content.len(), 2, "text + call");
                assert_eq!(turn.content[0], AssistantContent::text("reading it"));
                match &turn.content[1] {
                    AssistantContent::ToolCall(tc) => {
                        assert_eq!(tc.id.to_string(), "call_1_0");
                        assert_eq!(tc.function.name, "read_file");
                        assert_eq!(tc.function.arguments["path"], "src/main.rs");
                    }
                    other => panic!("expected a tool call, got {other:?}"),
                }
            }
            other => panic!("expected an assistant turn, got {other:?}"),
        }

        let results = crate::message::ChatMsg::tool_results(vec![
            crate::message::ChatToolResult {
                call: "call_1_0".into(),
                name: "read_file".into(),
                output: "fn main() {}".into(),
                is_error: false,
            },
            crate::message::ChatToolResult {
                call: "call_1_1".into(),
                name: "run_shell".into(),
                output: "(no result)".into(),
                is_error: true,
            },
        ]);
        match crate::message::to_rig_message(results, &blobs) {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 2, "one part per result");
                let UserContent::ToolResult(first) = &content[0] else {
                    panic!("expected a tool result, got {:?}", content[0]);
                };
                assert_eq!(first.call.to_string(), "call_1_0");
                assert_eq!(first.name, "read_file");
                assert!(!first.is_error);
                assert_eq!(first.content[0].as_text(), Some("fn main() {}"));
                let UserContent::ToolResult(second) = &content[1] else {
                    panic!("expected a tool result, got {:?}", content[1]);
                };
                assert!(second.is_error, "the failed call reports its error");
            }
            other => panic!("expected a user message, got {other:?}"),
        }
    }

    /// A call whose recorded arguments never parsed still reaches the wire: rig
    /// takes an empty object over a failed request.
    #[test]
    fn a_malformed_replayed_call_degrades_to_empty_arguments() {
        let msg = crate::message::ChatMsg::assistant_with(
            "",
            vec![crate::message::ChatToolCall {
                id: "call_0_0".into(),
                name: "".into(),
                args_json: "{not json".into(),
            }],
        );
        match crate::message::to_rig_message(msg, &crate::attachment::Blobs::new()) {
            rig_core::message::Message::Assistant(turn) => match &turn.content[0] {
                rig_core::message::AssistantContent::ToolCall(tc) => {
                    assert!(tc.function.arguments.is_empty(), "{}", tc.function.name);
                }
                other => panic!("expected a tool call, got {other:?}"),
            },
            other => panic!("expected an assistant turn, got {other:?}"),
        }
    }

    #[test]
    fn an_empty_text_with_only_images_drops_the_empty_text_part() {
        let mut msg = ChatMsg::user("");
        msg.attachments = vec![crate::attachment::Attachment {
            kind: crate::attachment::AttachmentKind::Image,
            name: "a.png".into(),
            media_type: "image/png".into(),
            size: 2,
            sha256: "ab".into(),
        }];
        let blobs = crate::attachment::Blobs::from([("ab".to_string(), b"png".to_vec())]);
        match crate::message::to_rig_message(msg, &blobs) {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 1, "just the image");
                assert!(matches!(
                    content[0],
                    rig_core::message::UserContent::Image(_)
                ));
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
