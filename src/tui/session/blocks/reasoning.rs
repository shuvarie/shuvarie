use ratatui::prelude::*;

use crate::tui::session::segment::Segment;
use crate::tui::theme;

/// Thinking/reasoning text streamed before (or alongside) the assistant's
/// reply. Collapsed to a `⌥ thinking ▸` header by default; clicking the
/// header toggles expansion.
pub struct ReasoningBlock {
    text: String,
    expanded: bool,
}

impl ReasoningBlock {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
        }
    }

    pub fn update(&mut self, msg: ReasoningMessage) -> bool {
        match msg {
            ReasoningMessage::Append(chunk) => {
                self.text.push_str(&chunk);
                true
            }
        }
    }

    /// Flip the collapse state of the reasoning body.
    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    /// Two segments: the header (the click target — the engine stamps the hit
    /// address on the first segment only) and, when expanded, the body.
    pub fn view(&self) -> Vec<Segment> {
        let mut segments = vec![Segment::plain(vec![Line::from(vec![
            Span::raw("⌥ ").fg(theme::TEXT_MUTED),
            Span::raw(if self.expanded {
                "thinking ▾"
            } else {
                "thinking ▸"
            })
            .fg(theme::TEXT_MUTED)
            .italic(),
        ])])];
        if self.expanded {
            segments.push(Segment::plain(
                self.text
                    .lines()
                    .map(|l| Line::from(Span::raw(format!("  {l}")).fg(theme::TEXT_DIM).italic()))
                    .collect(),
            ));
        }
        segments
    }
}

pub enum ReasoningMessage {
    Append(String),
}
