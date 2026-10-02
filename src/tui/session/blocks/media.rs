//! A user turn's attachment strip: one chip row per document, and one
//! halfblock image region per image whose media has arrived from core.
//! Rendered as ordinary styled cells so the chat virtualizer treats it like
//! any other block (clipping, diffing, caching all keep working).

use std::cell::RefCell;

use ratatui::prelude::*;
use shuvarie_llm::{Attachment, AttachmentKind};

use super::super::media;
use super::super::media::MediaStore;
use super::super::segment::{BLOCK_PADDING, BodyChunk, Segment};
use super::super::virtualizer::TurnEst;
use crate::tui::theme;

/// Rows an image slot ests for before a render measures the real height: one
/// chip row (the unloaded state). Images replace it with their aspect-derived
/// height on the first materialized paint.
const IMAGE_EST_ROWS: u32 = media::CHIP_ROWS;

const CHIP_ICON_DOC: &str = "▤";
const CHIP_ICON_IMAGE: &str = "🖼";

/// One attachment slot: metadata only. Content lives in the chat's media
/// store (documents were converted at attach time; images are the media
/// bytes).
#[derive(Debug, Clone)]
pub struct MediaSlot {
    attachment: Attachment,
}

/// A cache key: image regions change when the width, the which-images-loaded
/// mask, or the configured cell size change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaCacheKey {
    width: u16,
    media_mask: Vec<u8>,
    cell_gen: u64,
}

/// A user message's attachments, projected below the prompt text.
pub struct MediaBlock {
    slots: Vec<MediaSlot>,
    cache: RefCell<Option<(MediaCacheKey, Vec<Line<'static>>)>>,
}

impl MediaBlock {
    /// Build from the message's attachment metadata. Returns `None` when the
    /// message carries nothing (no block is pushed then).
    pub fn new(attachments: Vec<Attachment>) -> Option<Self> {
        if attachments.is_empty() {
            return None;
        }
        Some(Self {
            slots: attachments
                .into_iter()
                .map(|attachment| MediaSlot { attachment })
                .collect(),
            cache: RefCell::new(None),
        })
    }

    /// The image slots' shas (documents render chips without media).
    pub(crate) fn image_shas(&self) -> impl Iterator<Item = &str> + '_ {
        self.slots
            .iter()
            .filter(|slot| slot.attachment.kind == AttachmentKind::Image)
            .map(|slot| slot.attachment.sha256.as_str())
    }

    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst {
            padding_rows: 2,
            ..TurnEst::default()
        };
        for slot in &self.slots {
            est.deco_rows += match slot.attachment.kind {
                AttachmentKind::Image => IMAGE_EST_ROWS,
                AttachmentKind::Document => 1,
            };
        }
        est
    }

    pub(super) fn view(&self, width: u16, media_store: &MediaStore) -> Vec<Segment> {
        let key = MediaCacheKey {
            width,
            media_mask: self
                .slots
                .iter()
                .filter(|slot| slot.attachment.kind == AttachmentKind::Image)
                .map(|slot| u8::from(media_store.has(&slot.attachment.sha256)))
                .collect(),
            cell_gen: media_store.cell_gen(),
        };
        let mut cache = self.cache.borrow_mut();
        let needs_build = cache
            .as_ref()
            .is_none_or(|(cached_key, _)| *cached_key != key);
        if needs_build {
            *cache = Some((key, self.build_lines(width, media_store)));
        }
        let Some((_, lines)) = &*cache else {
            return Vec::new();
        };
        if lines.is_empty() {
            return Vec::new();
        }
        vec![Segment {
            chunks: vec![BodyChunk::fixed(lines.clone())],
            bg: Some(theme::prompt_bg()),
            padding: (BLOCK_PADDING.0, 1),
            hit: None,
            trim: true,
        }]
    }

    fn build_lines(&self, width: u16, media_store: &MediaStore) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for slot in &self.slots {
            match slot.attachment.kind {
                AttachmentKind::Document => {
                    lines.push(self.chip_line(
                        slot,
                        CHIP_ICON_DOC,
                        if slot.attachment.media_type == "text/plain" {
                            "text"
                        } else {
                            "document"
                        },
                    ));
                }
                AttachmentKind::Image => {
                    match media::image_lines(media_store, &slot.attachment.sha256, width) {
                        Some(image) => lines.extend(image),
                        None => lines.push(self.chip_line(slot, CHIP_ICON_IMAGE, "not loaded")),
                    }
                }
            }
        }
        lines
    }

    fn chip_line(&self, slot: &MediaSlot, icon: &str, kind: &str) -> Line<'static> {
        Line::from(vec![
            Span::raw(format!("{icon} ")).fg(theme::text_muted()),
            Span::raw(slot.attachment.name.clone()).fg(theme::text_dim()),
            Span::raw(format!(
                " — {kind}, {}",
                shuvarie_llm::format_size(slot.attachment.size)
            ))
            .fg(theme::text_muted()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::media::MediaStore;
    use super::*;
    use ratatui_image::FontSize;

    fn image_attachment(sha: &str) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            name: "shot.png".into(),
            media_type: "image/png".into(),
            size: 2048,
            sha256: sha.into(),
        }
    }

    fn document_attachment(sha: &str) -> Attachment {
        Attachment {
            kind: AttachmentKind::Document,
            name: "notes.csv".into(),
            media_type: "text/csv".into(),
            size: 4096,
            sha256: sha.into(),
        }
    }

    fn fresh_store() -> MediaStore {
        MediaStore::new(FontSize::new(8, 16))
    }

    #[test]
    fn new_skips_empty_attachments() {
        assert!(MediaBlock::new(Vec::new()).is_none());
    }

    #[test]
    fn est_counts_chip_rows_per_attachment() {
        let block = MediaBlock::new(vec![image_attachment("a"), document_attachment("b")]).unwrap();
        let est = block.est();
        assert_eq!(est.deco_rows, 2);
        assert_eq!(est.padding_rows, 2);
    }

    #[test]
    fn unloaded_images_render_a_chip_and_loaded_ones_render_regions() {
        let block = MediaBlock::new(vec![image_attachment("sha")]).unwrap();
        let mut store = fresh_store();
        let unloaded = block.view(40, &store);
        assert_eq!(unloaded.len(), 1);
        assert_eq!(unloaded[0].measure(40), 3, "chip row + 2 padding rows");

        store.insert("sha".into(), test_media_png());
        let loaded = block.view(40, &store);
        assert!(loaded[0].measure(40) > 3, "image rows add height");
    }

    #[test]
    fn document_slots_render_one_chip_row() {
        let block = MediaBlock::new(vec![document_attachment("sha")]).unwrap();
        let segments = block.view(40, &fresh_store());
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].measure(40), 3);
        let text = segments[0]
            .flattened()
            .into_iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(
            text.iter()
                .any(|line| line.contains("notes.csv") && line.contains("document"))
        );
    }

    fn test_media_png() -> Vec<u8> {
        let img = image::ImageBuffer::from_fn(48, 24, |x, y| {
            image::Rgb([(x * 5) as u8, 30, (y * 10) as u8])
        });
        let mut out = Vec::new();
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut out))
            .write_image(img.as_raw(), 48, 24, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }
}
