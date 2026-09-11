use std::cell::RefCell;
use std::time::Instant;

use ratatui::prelude::*;

use super::format_duration_ms;
use crate::tui::session::segment::{BLOCK_PADDING, BodyChunk, BodySource, Segment, TextRows};
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::{spinner, theme};

/// Thinking/reasoning text streamed during a turn — it may appear at any
/// position (before the reply, between tool calls). While chunks are still
/// arriving the header shows a spinner plus a live elapsed time; once thinking
/// ends it becomes `⌥ Thought 10.3s ▸`. Collapsed by default; clicking the
/// header toggles expansion.
pub struct ReasoningBlock {
    text: String,
    expanded: bool,
    thinking: bool,
    started_at: Option<Instant>,
    duration_ms: u64,
    /// Bumped on append/toggle/finish: keys the cached estimate so streaming
    /// appends do not rescan the whole body per token.
    rev: u64,
    est_cache: RefCell<Option<(u64, bool, TurnEst)>>,
}

impl ReasoningBlock {
    /// A block that is actively receiving streamed chunks.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            expanded: false,
            thinking: true,
            started_at: Some(Instant::now()),
            duration_ms: 0,
            rev: 0,
            est_cache: RefCell::new(None),
        }
    }

    /// A block whose stream already ended (session reload/undo rebuilds).
    pub fn finished(text: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            text: text.into(),
            expanded: false,
            thinking: false,
            started_at: None,
            duration_ms,
            rev: 0,
            est_cache: RefCell::new(None),
        }
    }

    pub fn update(&mut self, msg: ReasoningMessage) -> bool {
        match msg {
            ReasoningMessage::Append(chunk) => {
                self.text.push_str(&chunk);
                self.rev += 1;
                true
            }
            ReasoningMessage::Finish => {
                let changed = self.thinking;
                if changed && let Some(started) = self.started_at.take() {
                    self.duration_ms = started.elapsed().as_millis() as u64;
                }
                self.thinking = false;
                changed
            }
        }
    }

    /// Flip the collapse state of the reasoning body.
    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.rev += 1;
    }

    pub(super) fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.rev += 1;
        }
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn is_thinking(&self) -> bool {
        self.thinking
    }

    /// Vertical padding, one collapsed header row, plus the body rows when
    /// expanded. Cached per `(rev, expanded)`: streaming appends and toggles
    /// are the only bumps.
    pub(super) fn est(&self) -> TurnEst {
        if let Some((rev, expanded, est)) = self.est_cache.borrow().as_ref()
            && *rev == self.rev
            && *expanded == self.expanded
        {
            return *est;
        }
        let est = TurnEst {
            reasoning_rows: 2 * u32::from(BLOCK_PADDING.1)
                + 1
                + u32::from(self.expanded) * self.text.lines().count() as u32,
            ..TurnEst::default()
        };
        *self.est_cache.borrow_mut() = Some((self.rev, self.expanded, est));
        est
    }

    /// One padded segment: the header line, plus the body rows when
    /// expanded (the engine stamps the hit address on the first segment, so
    /// the whole block toggles on click). Long bodies project their rows from
    /// the shared text so only viewport rows materialize.
    pub fn view(&self, width: u16) -> Vec<Segment> {
        let header = if self.thinking {
            let ms = self
                .started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(self.duration_ms);
            vec![
                spinner::spinner(),
                Span::raw(" "),
                self.label("Thinking..."),
                Span::raw(format!(" {}", format_duration_ms(ms)))
                    .fg(theme::TEXT_MUTED)
                    .italic(),
            ]
        } else {
            let arrow = if self.expanded { "v" } else { ">" };
            vec![
                Span::raw("  ").fg(theme::TEXT_MUTED),
                self.label("Thought"),
                Span::raw(format!(" {}", format_duration_ms(self.duration_ms)))
                    .fg(theme::TEXT_MUTED)
                    .italic(),
                Span::raw(format!(" {arrow}")).fg(theme::TEXT_MUTED),
            ]
        };
        let mut chunks = vec![BodyChunk::fixed(vec![Line::from(header)])];
        if self.expanded {
            chunks.push(BodyChunk::rows(
                BodySource::Text {
                    rows: TextRows::new(self.text.as_str()),
                    prefix: "  ",
                    style: Style::new()
                        .fg(theme::TEXT_DIM)
                        .add_modifier(Modifier::ITALIC),
                },
                0,
                width,
                true,
            ));
        }
        vec![Segment {
            chunks,
            bg: None,
            padding: (0, BLOCK_PADDING.1),
            hit: None,
            trim: true,
        }]
    }

    fn label(&self, text: &str) -> Span<'static> {
        Span::raw(text.to_string()).fg(theme::TEXT_MUTED).italic()
    }
}

pub enum ReasoningMessage {
    Append(String),
    Finish,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(count: usize) -> String {
        (0..count)
            .map(|i| format!("thinking line {i} with a bit of filler to make it wrap sometimes"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn expanded_big_body_renders_sliced_and_matches_materialized() {
        let mut block = ReasoningBlock::finished(body(300), 1_000);
        block.toggle();
        let width = 80u16;
        let seg = &block.view(width)[0];
        assert!(
            matches!(seg.chunks[1], BodyChunk::Sliced(_)),
            "big body must render sliced"
        );
        let text: Vec<Line<'static>> = body(300)
            .lines()
            .map(|row| Line::from(Span::raw(format!("  {row}")).fg(theme::TEXT_DIM).italic()))
            .collect();
        let materialized = Segment::materialized(
            std::iter::once(Line::from(vec![
                Span::raw("  ").fg(theme::TEXT_MUTED),
                Span::raw("Thought").fg(theme::TEXT_MUTED).italic(),
                Span::raw(" 1.0s").fg(theme::TEXT_MUTED).italic(),
                Span::raw(" v").fg(theme::TEXT_MUTED),
            ]))
            .chain(text)
            .collect(),
            None,
            (0, BLOCK_PADDING.1),
            true,
        );
        assert_eq!(seg.measure(width), materialized.measure(width));
        let full = crate::tui::session::segment::tests::full_render(&materialized, width);
        crate::tui::session::segment::tests::assert_windows_match(
            seg,
            width,
            &full,
            &[0, 50, 200, u32::from(full.area().height) - 1],
        );
    }

    #[test]
    fn collapsed_body_stays_materialized() {
        let block = ReasoningBlock::finished(body(300), 1_000);
        let width = 80u16;
        let seg = &block.view(width)[0];
        assert_eq!(seg.chunks.len(), 1);
        assert!(matches!(seg.chunks[0], BodyChunk::Fixed(_)));
        assert_eq!(seg.measure(width), 1 + 2 * u32::from(BLOCK_PADDING.1));
    }
}
