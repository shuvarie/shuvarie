use ratatui::prelude::*;

use crate::tui::session::segment::{BLOCK_PADDING, Segment, TEXT_PADDING};
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::theme;

/// The user's submitted message, rendered as a full-width warm block without
/// a header. The block padding provides the blank rows and the side inset.
pub struct UserPrompt {
    content: String,
}

impl UserPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    pub fn view(&self) -> Vec<Segment> {
        if self.content.is_empty() {
            return Vec::new();
        }
        vec![Segment {
            lines: shuvarie_highlight::render(&self.content),
            bg: Some(theme::PROMPT_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: true,
        }]
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
}

impl SteeredPrompt {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    pub fn view(&self) -> Vec<Segment> {
        if self.content.is_empty() {
            return Vec::new();
        }
        let mut lines = vec![Line::from(
            Span::raw("↻ steered — sends after the current action")
                .fg(theme::ACCENT)
                .italic(),
        )];
        lines.extend(shuvarie_highlight::render(&self.content));
        vec![Segment {
            lines,
            bg: Some(theme::PROMPT_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: true,
        }]
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
}

impl TextBlock {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    pub fn update(&mut self, msg: TextMessage) -> bool {
        match msg {
            TextMessage::Append(chunk) => {
                self.content.push_str(&chunk);
                true
            }
        }
    }

    pub fn view(&self) -> Vec<Segment> {
        if self.content.is_empty() {
            return Vec::new();
        }
        vec![Segment {
            padding: TEXT_PADDING,
            ..Segment::plain(shuvarie_highlight::render(&self.content))
        }]
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
}

impl SystemText {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }

    pub fn view(&self) -> Vec<Segment> {
        let mut lines = vec![Line::from(Span::raw("System").fg(theme::TEXT_MUTED).bold())];
        lines.append(&mut shuvarie_highlight::render(&self.content));
        vec![Segment::plain(lines)]
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
        let seg = &block.view()[0];
        assert_eq!(seg.padding, (0, 1));
        assert_eq!(seg.bg, None);
        assert_eq!(seg.measure(40), 3);
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
