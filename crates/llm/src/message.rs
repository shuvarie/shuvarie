use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One message of the chat history sent to the LLM. `attachments` carries
/// the message's attachment metadata (empty for plain text turns); the rig
/// conversion turns them into multimodal parts when their blob content is
/// supplied alongside (see [`to_rig_message`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMsg {
    pub role: Role,
    pub content: String,
    #[serde(default)]
    pub attachments: Vec<crate::attachment::Attachment>,
}

impl ChatMsg {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            attachments: Vec::new(),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            attachments: Vec::new(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            attachments: Vec::new(),
        }
    }
}

/// The multimodal renderer: a chat message becomes a rig message whose user
/// variant is a list of content parts — the prompt text, one image part per
/// image attachment (base64 data from `blobs`, media type from metadata), and
/// one text part per document attachment: documents are pre-converted to
/// markdown when attached, so their blob content is markdown text wrapped in
/// a `<document>` tag. Attachments whose blob is missing (pruned, or a
/// metadata-only import) degrade to an `[image … content unavailable]`
/// note — never silently omitted.
pub fn to_rig_message(
    msg: ChatMsg,
    blobs: &crate::attachment::Blobs,
) -> rig_core::message::Message {
    match msg.role {
        Role::System => rig_core::message::Message::System {
            content: msg.content,
        },
        Role::User => rig_core::message::Message::User {
            content: user_content_parts(&msg, blobs),
        },
        Role::Assistant => rig_core::message::Message::assistant(msg.content),
    }
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
                            additional_params: None,
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
                        rig_core::message::Text {
                            text: format!(
                                "<document name=\"{}\">\n{text}\n</document>",
                                attachment.name
                            ),
                            additional_params: None,
                        },
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
