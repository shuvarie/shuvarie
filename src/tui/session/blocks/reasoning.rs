use std::cell::RefCell;
use std::time::Instant;

use ratatui::prelude::*;

use super::super::md_cache::MdCache;
use super::format_duration_ms;
use crate::tui::session::segment::{BLOCK_PADDING, BodyChunk, Segment};
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::{spinner, theme};

/// Thinking/reasoning text streamed during a turn — it may appear at any
/// position (before the reply, between tool calls). While chunks are still
/// arriving the header shows a spinner plus a live elapsed time; once thinking
/// ends it becomes `⌥ Thought 10.3s ▸`. Collapsed by default; clicking the
/// header toggles expansion. The expanded body renders the markdown in the
/// dimmed reasoning flavor (`MdCache::new_dim`): structure carries over, prose
/// and inline code stay dim italic, code blocks keep their normal colors.
pub struct ReasoningBlock {
    text: String,
    cache: MdCache,
    expanded: bool,
    thinking: bool,
    started_at: Option<Instant>,
    duration_ms: u64,
    /// Bumped on append/toggle/finish: keys the cached estimate so streaming
    /// appends do not rescan the whole body per token.
    rev: u64,
    est_cache: RefCell<Option<(u64, bool, bool, TurnEst)>>,
}

impl ReasoningBlock {
    /// A block that is actively receiving streamed chunks.
    pub fn new(text: impl Into<String>) -> Self {
        let text: String = text.into();
        Self {
            cache: MdCache::new_dim(&text),
            text,
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
        let text: String = text.into();
        Self {
            cache: MdCache::new_dim(&text),
            text,
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
                self.cache.append(&chunk);
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
    /// expanded. Cached per `(rev, expanded, rendered)`: streaming appends,
    /// toggles, and the first body render are the only bumps. Rendered bodies
    /// report their exact display-line count through the markdown cache;
    /// before the first render the raw source lines stand in.
    pub(super) fn est(&self) -> TurnEst {
        let counters = self.cache.est_counters();
        let rendered = counters.is_some();
        if let Some((rev, expanded, was_rendered, est)) = self.est_cache.borrow().as_ref()
            && *rev == self.rev
            && *expanded == self.expanded
            && *was_rendered == rendered
        {
            return *est;
        }
        let body = match counters {
            Some((lines, _)) => lines,
            None => self.text.lines().count() as u32,
        };
        let est = TurnEst {
            reasoning_rows: 2 * u32::from(BLOCK_PADDING.1) + 1 + u32::from(self.expanded) * body,
            ..TurnEst::default()
        };
        *self.est_cache.borrow_mut() = Some((self.rev, self.expanded, rendered, est));
        est
    }

    /// One padded segment: the header line, plus the markdown body chunks
    /// when expanded (the engine stamps the hit address on the first
    /// segment, so the whole block toggles on click). Long bodies slice into
    /// committed runs shared through the markdown cache; only viewport rows
    /// materialize.
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
                    .fg(theme::text_muted())
                    .italic(),
            ]
        } else {
            let arrow = if self.expanded { "v" } else { ">" };
            vec![
                Span::raw("  ").fg(theme::text_muted()),
                self.label("Thought"),
                Span::raw(format!(" {}", format_duration_ms(self.duration_ms)))
                    .fg(theme::text_muted())
                    .italic(),
                Span::raw(format!(" {arrow}")).fg(theme::text_muted()),
            ]
        };
        let mut chunks = vec![BodyChunk::fixed(vec![Line::from(header)])];
        if self.expanded {
            chunks.extend(self.cache.chunks(width).unwrap_or_default());
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
        Span::raw(text.to_string()).fg(theme::text_muted()).italic()
    }
}

pub enum ReasoningMessage {
    Append(String),
    Finish,
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_highlight::render_dim;

    fn body(count: usize) -> String {
        (0..count)
            .map(|i| format!("thinking line {i} with a bit of filler to make it wrap sometimes"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn header_line(duration: &str) -> Line<'static> {
        Line::from(vec![
            Span::raw("  ").fg(theme::text_muted()),
            Span::raw("Thought").fg(theme::text_muted()).italic(),
            Span::raw(format!(" {duration}"))
                .fg(theme::text_muted())
                .italic(),
            Span::raw(" v").fg(theme::text_muted()),
        ])
    }

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

    fn materialized(content: &str, duration: &str) -> Segment {
        Segment::materialized(
            std::iter::once(header_line(duration))
                .chain(render_dim(content))
                .collect(),
            None,
            (0, BLOCK_PADDING.1),
            true,
        )
    }

    #[test]
    fn expanded_big_body_renders_sliced_and_matches_materialized() {
        let mut block = ReasoningBlock::finished(body(300), 1_000);
        block.toggle();
        let width = 80u16;
        let seg = &block.view(width)[0];
        assert!(
            seg.chunks
                .iter()
                .skip(1)
                .any(|chunk| matches!(chunk, BodyChunk::Sliced(_))),
            "big body must render sliced"
        );
        let materialized = materialized(&body(300), "1.0s");
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
    fn collapsed_body_renders_header_only() {
        let block = ReasoningBlock::finished(body(300), 1_000);
        let width = 80u16;
        let seg = &block.view(width)[0];
        assert_eq!(seg.chunks.len(), 1);
        assert!(matches!(seg.chunks[0], BodyChunk::Fixed(_)));
        assert_eq!(seg.measure(width), 1 + 2 * u32::from(BLOCK_PADDING.1));
    }

    #[test]
    fn expanded_body_matches_one_shot_dim_render() {
        let content = "Plan **now**:\n\n1. read the file\n2. edit it\n\n```rust\nlet x = 1;\n```";
        let mut block = ReasoningBlock::finished(content, 500);
        block.toggle();
        let width = 60u16;
        let seg = &block.view(width)[0];
        let expected = materialized(content, "0.5s");
        assert_eq!(row_strings(seg, width), row_strings(&expected, width));
        let rows = row_strings(seg, width);
        assert!(
            rows.iter().any(|row| row.contains("```rust")),
            "the body carries the markdown fence"
        );
        assert!(
            rows.iter().any(|row| row.starts_with("1. read")),
            "the body carries the ordered marker"
        );
    }

    #[test]
    fn streamed_reasoning_matches_one_shot() {
        let content = "Intro\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n\nOutro";
        let mut block = ReasoningBlock::finished("", 0);
        block.toggle();
        let mut last = 0usize;
        for (i, _) in content.char_indices().skip(1) {
            block.update(ReasoningMessage::Append(content[last..i].to_string()));
            last = i;
            let _ = block.view(80);
        }
        block.update(ReasoningMessage::Append(content[last..].to_string()));
        let seg = &block.view(80)[0];
        assert_eq!(
            row_strings(seg, 80),
            row_strings(&materialized(content, "0.0s"), 80)
        );
    }

    #[test]
    fn est_uses_rendered_lines_once_viewed() {
        let mut block = ReasoningBlock::finished("a\n\n\n\nb", 0);
        block.toggle();
        assert_eq!(
            block.est().reasoning_rows,
            2 * u32::from(BLOCK_PADDING.1) + 1 + 5,
            "raw source lines stand in before the first render"
        );
        let _ = block.view(60);
        let rendered = render_dim("a\n\n\n\nb").len() as u32;
        assert_ne!(rendered, 5);
        assert_eq!(
            block.est().reasoning_rows,
            2 * u32::from(BLOCK_PADDING.1) + 1 + rendered
        );
    }
}
