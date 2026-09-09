use std::cell::{Cell, RefCell};
use std::time::Instant;

use ratatui::prelude::*;

use super::format_duration_ms;
use crate::tui::session::blocks::{BodyCache, BodyKey, cached_body, measure_lines};
use crate::tui::session::segment::{BLOCK_PADDING, Segment};
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
    rev: Cell<u64>,
    cache: RefCell<Option<BodyCache>>,
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
            rev: Cell::new(0),
            cache: RefCell::new(None),
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
            rev: Cell::new(0),
            cache: RefCell::new(None),
        }
    }

    pub fn update(&mut self, msg: ReasoningMessage) -> bool {
        match msg {
            ReasoningMessage::Append(chunk) => {
                self.text.push_str(&chunk);
                self.rev.set(self.rev.get() + 1);
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
    }

    pub(super) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn is_thinking(&self) -> bool {
        self.thinking
    }

    /// Vertical padding, one collapsed header row, plus the body rows when
    /// expanded.
    pub(super) fn est(&self) -> TurnEst {
        TurnEst {
            reasoning_rows: 2 * u32::from(BLOCK_PADDING.1)
                + 1
                + u32::from(self.expanded) * self.text.lines().count() as u32,
            ..TurnEst::default()
        }
    }

    /// One padded segment: the header line (rebuilt every frame — it carries
    /// the spinner and the live elapsed time while thinking), plus the body
    /// lines from the cache when expanded (the engine stamps the hit address
    /// on the segment, so the whole block toggles on click).
    pub fn view(&self, width: u16) -> (Segment, u32) {
        let text_width = width.max(1);
        let header = self.header_line();
        let header_h = measure_lines(std::slice::from_ref(&header), text_width, true);
        let (body, body_h) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: self.rev.get(),
                expanded: self.expanded,
                env_rev: 0,
            },
            text_width,
            true,
            || {
                self.text
                    .lines()
                    .map(|l| Line::from(Span::raw(format!("  {l}")).fg(theme::TEXT_DIM).italic()))
                    .collect()
            },
        );
        let mut lines = Vec::with_capacity(1 + body.len());
        lines.push(header);
        lines.extend(body);
        (
            Segment {
                lines,
                bg: None,
                padding: (0, BLOCK_PADDING.1),
                hit: None,
                trim: true,
            },
            2 * u32::from(BLOCK_PADDING.1) + header_h + body_h,
        )
    }

    fn header_line(&self) -> Line<'static> {
        if self.thinking {
            let ms = self
                .started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(self.duration_ms);
            Line::from(vec![
                spinner::spinner(),
                Span::raw(" "),
                self.label("Thinking..."),
                Span::raw(format!(" {}", format_duration_ms(ms)))
                    .fg(theme::TEXT_MUTED)
                    .italic(),
            ])
        } else {
            let arrow = if self.expanded { "v" } else { ">" };
            Line::from(vec![
                Span::raw("  ").fg(theme::TEXT_MUTED),
                self.label("Thought"),
                Span::raw(format!(" {}", format_duration_ms(self.duration_ms)))
                    .fg(theme::TEXT_MUTED)
                    .italic(),
                Span::raw(format!(" {arrow}")).fg(theme::TEXT_MUTED),
            ])
        }
    }

    fn label(&self, text: &str) -> Span<'static> {
        Span::raw(text.to_string()).fg(theme::TEXT_MUTED).italic()
    }
}

pub enum ReasoningMessage {
    Append(String),
    Finish,
}
