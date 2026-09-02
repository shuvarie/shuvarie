use ratatui::prelude::*;

use crate::tui::session::segment::Segment;
use crate::tui::{spinner, theme};

/// Thinking/reasoning text streamed during a turn — it may appear at any
/// position (before the reply, between tool calls). While chunks are still
/// arriving the header shows a spinner; once thinking ends it becomes
/// `⌥ Thought ▸`. Collapsed by default; clicking the header toggles expansion.
pub struct ReasoningBlock {
    text: String,
    expanded: bool,
    thinking: bool,
}

impl ReasoningBlock {
    /// A block that is actively receiving streamed chunks.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
            thinking: true,
        }
    }

    /// A block whose stream already ended (session reload/undo rebuilds).
    pub fn finished(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
            thinking: false,
        }
    }

    pub fn update(&mut self, msg: ReasoningMessage) -> bool {
        match msg {
            ReasoningMessage::Append(chunk) => {
                self.text.push_str(&chunk);
                true
            }
            ReasoningMessage::Finish => {
                let changed = self.thinking;
                self.thinking = false;
                changed
            }
        }
    }

    /// Flip the collapse state of the reasoning body.
    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub fn is_thinking(&self) -> bool {
        self.thinking
    }

    /// Two segments: the header (the click target — the engine stamps the hit
    /// address on the first segment only) and, when expanded, the body.
    pub fn view(&self) -> Vec<Segment> {
        let header = if self.thinking {
            vec![spinner::spinner(), Span::raw(" "), self.label("Thinking...")]
        } else {
            let arrow = if self.expanded { "▾" } else { "▸" };
            vec![
                Span::raw("  ").fg(theme::TEXT_MUTED),
                self.label("Thought"),
                Span::raw(format!(" {arrow}")).fg(theme::TEXT_MUTED),
            ]
        };
        let mut segments = vec![Segment::plain(vec![Line::from(header)])];
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

    fn label(&self, text: &str) -> Span<'static> {
        Span::raw(text.to_string()).fg(theme::TEXT_MUTED).italic()
    }
}

pub enum ReasoningMessage {
    Append(String),
    Finish,
}
