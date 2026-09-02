use ratatui::prelude::*;

use crate::tui::session::segment::{BLOCK_PADDING, Segment};
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
        }]
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
        vec![Segment::plain(shuvarie_highlight::render(&self.content))]
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
}
