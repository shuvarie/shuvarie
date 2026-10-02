use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
}

/// One message of the chat history sent to the LLM. `attachments` carries
/// the message's attachment metadata (empty for plain text turns); the
/// conversion to rig is text-only until the image pipeline wires
/// multimodal parts in.
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

impl From<ChatMsg> for rig_core::message::Message {
    /// Text-only for now: messages with attachments are sent as multimodal
    /// `UserContent` parts once the image pipeline (resolve + capability
    /// gate) lands; the metadata rides along in the meantime.
    fn from(msg: ChatMsg) -> Self {
        match msg.role {
            Role::System => rig_core::message::Message::System {
                content: msg.content,
            },
            Role::User => rig_core::message::Message::user(msg.content),
            Role::Assistant => rig_core::message::Message::assistant(msg.content),
        }
    }
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
    fn message_conversion_stays_text_only_without_attachments() {
        let mut msg = ChatMsg::user("look");
        msg.attachments = vec![crate::attachment::Attachment {
            kind: crate::attachment::AttachmentKind::Image,
            name: "a.png".into(),
            media_type: "image/png".into(),
            size: 2,
            sha256: "ab".into(),
        }];
        let rig_msg = rig_core::message::Message::from(msg);
        match rig_msg {
            rig_core::message::Message::User { content } => {
                assert_eq!(content.len(), 1, "text part only until the image pipeline");
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }
}
