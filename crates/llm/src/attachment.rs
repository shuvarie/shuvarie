use serde::{Deserialize, Serialize};

/// The media category of an attachment: images ride inside the LLM request as
/// multimodal parts; documents are converted to markdown text before the
/// request. Persisted (snake_case) and mirrored by
/// `shuvarie_db::StoredAttachmentKind` in the DB columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    Image,
    Document,
}

impl std::fmt::Display for AttachmentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image => f.write_str("image"),
            Self::Document => f.write_str("document"),
        }
    }
}

/// Metadata of one attachment on a user message. The content bytes live in
/// the store's content-addressed blob table (`attachment_blobs`), keyed by
/// `sha256` — the same image attached to three messages is stored once. No
/// bytes are carried here, so every in-memory history carrier (the session
/// tree, the chat pane, compaction transcripts) stays byte-free and cheap to
/// clone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub kind: AttachmentKind,
    /// Display file name (a basename, not a filesystem path).
    pub name: String,
    /// IANA media type (`image/png`, `application/pdf`, …).
    pub media_type: String,
    /// Original content size in bytes.
    pub size: u64,
    /// SHA-256 of the content, lowercase hex — the blob lookup key.
    pub sha256: String,
}

impl Attachment {
    /// `image "screenshot.png" (image/png, 1.1 MiB)` — the one-line form used
    /// in compaction transcripts and debug output.
    pub fn describe(&self) -> String {
        format!(
            "{} {:?} ({}, {})",
            self.kind,
            self.name,
            self.media_type,
            format_size(self.size)
        )
    }
}

/// Compact human byte size (binary units: B, KiB, MiB, GiB, TiB).
pub fn format_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if size < 1024 {
        return format!("{size} B");
    }
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(kind: AttachmentKind, name: &str, media_type: &str, size: u64) -> Attachment {
        Attachment {
            kind,
            name: name.to_string(),
            media_type: media_type.to_string(),
            size,
            sha256: "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90".into(),
        }
    }

    #[test]
    fn describe_matches_shape() {
        let image = attachment(AttachmentKind::Image, "screenshot.png", "image/png", 2_000);
        assert_eq!(
            image.describe(),
            "image \"screenshot.png\" (image/png, 2.0 KiB)"
        );
        let document = attachment(
            AttachmentKind::Document,
            "report.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            512,
        );
        assert_eq!(
            document.describe(),
            "document \"report.docx\" (application/vnd.openxmlformats-officedocument.wordprocessingml.document, 512 B)"
        );
    }

    #[test]
    fn format_size_rounds_and_switches_units() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(5_529_600), "5.3 MiB");
        assert_eq!(format_size(1_073_741_824), "1.0 GiB");
        assert_eq!(format_size(3_221_225_472), "3.0 GiB");
        assert_eq!(format_size(1_099_511_627_776), "1.0 TiB");
        assert_eq!(format_size(u64::MAX), "16777216 TiB", "caps at TiB");
    }

    #[test]
    fn attachment_kind_serializes_snake_case() {
        assert_eq!(
            serde_json::to_value(AttachmentKind::Document).unwrap(),
            serde_json::json!("document")
        );
        assert_eq!(
            serde_json::from_value::<AttachmentKind>(serde_json::json!("image")).unwrap(),
            AttachmentKind::Image
        );
        assert_eq!(AttachmentKind::Image.to_string(), "image");
    }

    #[test]
    fn attachment_round_trips_through_json() {
        let item = attachment(AttachmentKind::Image, "photo.png", "image/png", 2_000);
        let json = serde_json::to_string(&item).unwrap();
        assert_eq!(serde_json::from_str::<Attachment>(&json).unwrap(), item);
    }
}
