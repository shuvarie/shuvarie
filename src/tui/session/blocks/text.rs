use ratatui::prelude::*;

use super::super::md_cache::MdCache;
use super::super::segment::{BLOCK_PADDING, BodyChunk, Segment, TEXT_PADDING};
use super::super::virtualizer::TurnEst;
use crate::tui::theme;

/// Estimated row counters for a cached-markdown block: exact rendered line
/// counts once a view has rendered the content, the raw text line scan
/// before it.
fn est_for(cache: &MdCache, padding_rows: u32, deco_rows: u32) -> TurnEst {
    let mut est = TurnEst {
        padding_rows,
        deco_rows,
        ..TurnEst::default()
    };
    match cache.est_counters() {
        Some((lines, width)) => {
            est.text_lines = lines;
            est.text_width = width;
        }
        None => est.add_text(cache.content()),
    }
    est
}

/// The user's submitted message, rendered as a full-width warm block without
/// a header. The block padding provides the blank rows and the side inset.
pub struct UserPrompt {
    cache: MdCache,
}

impl UserPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            cache: MdCache::new(content),
        }
    }

    pub fn view(&self, width: u16) -> Vec<Segment> {
        let Some(chunks) = self.cache.chunks(width) else {
            return Vec::new();
        };
        vec![Segment {
            chunks,
            bg: Some(theme::PROMPT_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: true,
        }]
    }

    pub(super) fn est(&self) -> TurnEst {
        est_for(&self.cache, 2 * u32::from(BLOCK_PADDING.1), 0)
    }
}

/// A queued (steered) user prompt: shown in the chat while the agent works,
/// to be sent as the next user turn once the agent finishes its current
/// action (a tool call, a thinking segment, or a text segment). Renders like
/// a user prompt with a muted queue header.
pub struct SteeredPrompt {
    cache: MdCache,
}

impl SteeredPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            cache: MdCache::new(content),
        }
    }

    pub fn view(&self, width: u16) -> Vec<Segment> {
        if self.cache.content().is_empty() {
            return Vec::new();
        }
        let mut chunks = vec![BodyChunk::fixed(vec![Line::from(
            Span::raw("↻ steered — sends after the current action")
                .fg(theme::ACCENT)
                .italic(),
        )])];
        chunks.extend(self.cache.chunks(width).unwrap_or_default());
        vec![Segment {
            chunks,
            bg: Some(theme::PROMPT_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: true,
        }]
    }

    pub(super) fn est(&self) -> TurnEst {
        est_for(&self.cache, 2 * u32::from(BLOCK_PADDING.1), 1)
    }
}

/// A chunk of assistant markdown. Text streams into the trailing chunk of the
/// in-flight turn; a tool call splits the stream into separate chunks.
/// Appends cost O(1); the render catches up on the next view by re-rendering
/// only the open region after the last safe markdown boundary.
pub struct TextBlock {
    cache: MdCache,
}

impl TextBlock {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            cache: MdCache::new(content),
        }
    }

    pub fn update(&mut self, msg: TextMessage) -> bool {
        match msg {
            TextMessage::Append(chunk) => {
                self.cache.append(&chunk);
                true
            }
        }
    }

    pub fn view(&self, width: u16) -> Vec<Segment> {
        let Some(chunks) = self.cache.chunks(width) else {
            return Vec::new();
        };
        vec![Segment {
            chunks,
            bg: None,
            padding: TEXT_PADDING,
            hit: None,
            trim: true,
        }]
    }

    pub(super) fn est(&self) -> TurnEst {
        est_for(&self.cache, 2 * u32::from(TEXT_PADDING.1), 0)
    }
}

pub enum TextMessage {
    Append(String),
}

/// A system message with its plain header line.
pub struct SystemText {
    cache: MdCache,
}

impl SystemText {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            cache: MdCache::new(content),
        }
    }

    pub fn view(&self, width: u16) -> Vec<Segment> {
        let mut chunks = vec![BodyChunk::fixed(vec![Line::from(
            Span::raw("System").fg(theme::TEXT_MUTED).bold(),
        )])];
        chunks.extend(self.cache.chunks(width).unwrap_or_default());
        vec![Segment {
            chunks,
            bg: None,
            padding: (0, 0),
            hit: None,
            trim: true,
        }]
    }

    pub(super) fn est(&self) -> TurnEst {
        est_for(&self.cache, 0, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_highlight::render;

    fn row_strings(seg: &Segment, width: u16) -> Vec<String> {
        let h = seg.measure(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, h as u16));
        seg.paint(buf.area, 0, 0, h, width, &mut buf);
        (0..h)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y as u16)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn text_block_pads_one_row_above_and_below() {
        let block = TextBlock::new("hello world");
        let seg = &block.view(40)[0];
        assert_eq!(seg.padding, (0, 1));
        assert_eq!(seg.bg, None);
        assert_eq!(seg.measure(40), 3);
        let est = block.est();
        assert_eq!(est.padding_rows, 2);
        assert_eq!(est.height(40), seg.measure(40));

        let rows = row_strings(seg, 40);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], "");
        assert_eq!(rows[1], "hello world");
        assert_eq!(rows[2], "");
    }

    #[test]
    fn streamed_text_matches_one_shot_render() {
        let content =
            "Intro paragraph.\n\n- a bullet\n- another\n\n```rust\nlet x = 1;\n```\n\nTail";
        let mut block = TextBlock::new("");
        let mut last = 0usize;
        for (i, _) in content.char_indices().skip(1) {
            block.update(TextMessage::Append(content[last..i].to_string()));
            last = i;
        }
        block.update(TextMessage::Append(content[last..].to_string()));
        let streamed = row_strings(&block.view(60)[0], 60);
        let one_shot =
            Segment::chunked(BodyChunk::fixed(render(content)), None, TEXT_PADDING, true);
        assert_eq!(streamed, row_strings(&one_shot, 60));
    }

    #[test]
    fn big_streamed_text_renders_sliced() {
        let content = (0..300)
            .map(|i| format!("paragraph line {i} with enough filler to wrap around the edge"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let block = TextBlock::new(&content);
        let seg = &block.view(60)[0];
        assert!(
            seg.chunks
                .iter()
                .any(|chunk| matches!(chunk, BodyChunk::Sliced(_))),
            "long committed runs must render sliced"
        );
        let materialized =
            Segment::chunked(BodyChunk::fixed(render(&content)), None, TEXT_PADDING, true);
        assert_eq!(seg.measure(60), materialized.measure(60));
        let full = crate::tui::session::segment::tests::full_render(&materialized, 60);
        crate::tui::session::segment::tests::assert_windows_match(
            seg,
            60,
            &full,
            &[0, 40, 200, u32::from(full.area().height) - 1],
        );
    }

    #[test]
    fn user_prompt_render_matches_one_shot() {
        let prompt = UserPrompt::new("**bold** intro\n\nsecond para");
        let seg = &prompt.view(50)[0];
        assert_eq!(seg.bg, Some(theme::PROMPT_BG));
        assert_eq!(seg.padding, BLOCK_PADDING);
        let materialized = Segment::materialized(
            render("**bold** intro\n\nsecond para"),
            Some(theme::PROMPT_BG),
            BLOCK_PADDING,
            true,
        );
        assert_eq!(row_strings(seg, 50), row_strings(&materialized, 50));
    }
}
