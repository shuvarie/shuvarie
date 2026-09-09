use std::cell::{Cell, RefCell};

use ratatui::prelude::*;

use crate::tui::session::blocks::{BodyCache, BodyKey, cached_body};
use crate::tui::session::segment::{BLOCK_PADDING, Segment, TEXT_PADDING};
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::theme;

fn text_width(width: u16, padding: (u16, u16)) -> u16 {
    width.saturating_sub(2 * padding.0).max(1)
}

/// The user's submitted message, rendered as a full-width warm block without
/// a header. The block padding provides the blank rows and the side inset.
pub struct UserPrompt {
    content: String,
    cache: RefCell<Option<BodyCache>>,
}

impl UserPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            cache: RefCell::new(None),
        }
    }

    pub fn view(&self, width: u16) -> Option<(Segment, u32)> {
        let tw = text_width(width, BLOCK_PADDING);
        let (lines, height) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: 0,
                expanded: false,
                env_rev: 0,
            },
            tw,
            true,
            || shuvarie_highlight::render(&self.content),
        );
        if lines.is_empty() {
            return None;
        }
        Some((
            Segment {
                lines,
                bg: Some(theme::PROMPT_BG),
                padding: BLOCK_PADDING,
                hit: None,
                trim: true,
            },
            height + 2 * u32::from(BLOCK_PADDING.1),
        ))
    }

    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst {
            padding_rows: 2 * u32::from(BLOCK_PADDING.1),
            ..TurnEst::default()
        };
        est.add_text(&self.content);
        est
    }
}

/// A queued (steered) user prompt: shown in the chat while the agent works,
/// to be sent as the next user turn once the agent finishes its current
/// action (a tool call, a thinking segment, or a text segment). Renders like
/// a user prompt with a muted queue header.
pub struct SteeredPrompt {
    content: String,
    cache: RefCell<Option<BodyCache>>,
}

impl SteeredPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            cache: RefCell::new(None),
        }
    }

    pub fn view(&self, width: u16) -> Option<(Segment, u32)> {
        let tw = text_width(width, BLOCK_PADDING);
        let (lines, height) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: 0,
                expanded: false,
                env_rev: 0,
            },
            tw,
            true,
            || {
                let mut lines = vec![Line::from(
                    Span::raw("↻ steered — sends after the current action")
                        .fg(theme::ACCENT)
                        .italic(),
                )];
                lines.extend(shuvarie_highlight::render(&self.content));
                lines
            },
        );
        if lines.is_empty() {
            return None;
        }
        Some((
            Segment {
                lines,
                bg: Some(theme::PROMPT_BG),
                padding: BLOCK_PADDING,
                hit: None,
                trim: true,
            },
            height + 2 * u32::from(BLOCK_PADDING.1),
        ))
    }

    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst {
            padding_rows: 2 * u32::from(BLOCK_PADDING.1),
            deco_rows: 1,
            ..TurnEst::default()
        };
        est.add_text(&self.content);
        est
    }
}

/// A chunk of assistant markdown. Text streams into the trailing chunk of the
/// in-flight turn; a tool call splits the stream into separate chunks.
pub struct TextBlock {
    content: String,
    rev: Cell<u64>,
    cache: RefCell<Option<BodyCache>>,
}

impl TextBlock {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            rev: Cell::new(0),
            cache: RefCell::new(None),
        }
    }

    pub fn update(&mut self, msg: TextMessage) -> bool {
        match msg {
            TextMessage::Append(chunk) => {
                self.content.push_str(&chunk);
                self.rev.set(self.rev.get() + 1);
                true
            }
        }
    }

    pub fn view(&self, width: u16) -> Option<(Segment, u32)> {
        let tw = text_width(width, TEXT_PADDING);
        let (lines, height) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: self.rev.get(),
                expanded: false,
                env_rev: 0,
            },
            tw,
            true,
            || shuvarie_highlight::render(&self.content),
        );
        if lines.is_empty() {
            return None;
        }
        Some((
            Segment {
                padding: TEXT_PADDING,
                ..Segment::plain(lines)
            },
            height + 2 * u32::from(TEXT_PADDING.1),
        ))
    }

    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst {
            padding_rows: 2 * u32::from(TEXT_PADDING.1),
            ..TurnEst::default()
        };
        est.add_text(&self.content);
        est
    }
}

pub enum TextMessage {
    Append(String),
}

/// A system message with its plain header line.
pub struct SystemText {
    content: String,
    cache: RefCell<Option<BodyCache>>,
}

impl SystemText {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            cache: RefCell::new(None),
        }
    }

    pub fn view(&self, width: u16) -> Option<(Segment, u32)> {
        let tw = text_width(width, (0, 0));
        let (lines, height) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: 0,
                expanded: false,
                env_rev: 0,
            },
            tw,
            true,
            || {
                let mut lines = vec![Line::from(Span::raw("System").fg(theme::TEXT_MUTED).bold())];
                lines.append(&mut shuvarie_highlight::render(&self.content));
                lines
            },
        );
        Some((Segment::plain(lines), height))
    }

    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst::deco(1);
        est.add_text(&self.content);
        est
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_pads_one_row_above_and_below() {
        let block = TextBlock::new("hello world");
        let (seg, height) = block.view(40).expect("segment");
        assert_eq!(seg.padding, (0, 1));
        assert_eq!(seg.bg, None);
        assert_eq!(height, 3);
        assert_eq!(seg.measure(40), height);
        let est = block.est();
        assert_eq!(est.padding_rows, 2);
        assert_eq!(est.height(40), seg.measure(40));

        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 3));
        seg.paint(buf.area, 0, 0, 3, 40, &mut buf);
        let row = |y| {
            (0..40)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
        };
        assert_eq!(row(0).trim_end(), "");
        assert_eq!(row(1).trim_end(), "hello world");
        assert_eq!(row(2).trim_end(), "");
    }
}
