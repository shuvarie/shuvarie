use rig_core::message::{Image, ImageDetail, ImageMediaType, MimeType, Text, ToolResultContent};
use rig_core::tool::ToolExecutionError;

pub use rig_core::message::{DocumentSourceKind, ToolResultContent as ToolResultContentAlias};

/// Builds the model-visible content blocks for a tool result: a text note
/// followed by one image. Multimodal tools return their images this way —
/// rig forwards the blocks into the provider's tool-result rendering (text is
/// universal; the image rides natively on providers that support images in
/// tool results).
///
/// The note should describe what the image is (name, media type, size),
/// because activity display collects text blocks only.
pub fn tool_content_with_image(
    note: String,
    base64_data: String,
    media_type: &str,
) -> Result<Vec<ToolResultContent>, ToolExecutionError> {
    let media_type = ImageMediaType::from_mime_type(media_type).ok_or_else(|| {
        ToolExecutionError::other(format!(
            "unrecognized image media type '{media_type}' for tool output"
        ))
    })?;
    Ok(vec![
        ToolResultContent::Text(Text {
            text: note,
            additional_params: None,
        }),
        ToolResultContent::Image(Image {
            data: DocumentSourceKind::Base64(base64_data),
            media_type: Some(media_type),
            detail: Some(ImageDetail::Auto),
            additional_params: None,
        }),
    ])
}
