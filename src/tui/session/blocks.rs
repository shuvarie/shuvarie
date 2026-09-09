use std::cell::RefCell;

mod context;
mod reasoning;
mod text;
mod tool;

pub use context::ContextBlock;
pub use reasoning::{ReasoningBlock, ReasoningMessage};
pub use text::{SteeredPrompt, SystemText, TextBlock, TextMessage, UserPrompt};
pub use tool::{ToolBlock, ToolMessage};

use std::collections::BTreeMap;

use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use shuvarie_core::DiagnosticInfo;

use super::segment::Segment;
use super::virtualizer::TurnEst;
use crate::tui::{spinner, theme};

/// Format a duration in milliseconds for block display: seconds with one
/// decimal below a minute, `Xm YYs` at or above it.
pub(super) fn format_duration_ms(ms: u64) -> String {
    if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let total = ms / 1000;
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// Tool blocks that carry the elapsed/taken meta row: only the long-running
/// calls where timing is worth the extra row. Thinking blocks time themselves
/// in the reasoning header.
pub(super) fn shows_elapsed(name: &str) -> bool {
    name == "run_shell" || name == "explore_workspace"
}

/// Tool blocks whose successful output is not previewed when collapsed: file
/// contents and directory listings the header already summarizes. Failed
/// calls keep their (short) error message visible.
pub(super) fn hides_output_when_collapsed(name: &str) -> bool {
    name == "read_file" || name == "list_dir"
}

/// One chat block: a TEA model per variant for the stateful kinds, unit
/// variants for the stateless decorations the engine synthesizes around turns.
pub enum Block {
    User(UserPrompt),
    Steered(SteeredPrompt),
    Text(TextBlock),
    System(SystemText),
    Tool(Box<ToolBlock>),
    Reasoning(ReasoningBlock),
    Context(ContextBlock),
    Summary,
    Interrupted,
    Working,
    ToolOnlyNote,
}

/// Messages routed into a block, either from the chat's data flow or from a
/// click hit-test (`Toggle`).
pub enum BlockMessage {
    Toggle,
    Tool(ToolMessage),
    Text(TextMessage),
    Reasoning(ReasoningMessage),
}

/// Read-only view context handed to block `view` calls.
pub struct ChatEnv<'a> {
    pub lsp_diagnostics: &'a BTreeMap<String, Vec<DiagnosticInfo>>,
}

/// Cached body projection of one block: the lines built from the block's
/// content state, valid for one (width, content revision, expansion, env
/// revision) combination, with the height measured once at `width`.
/// Rebuilding a turn's render cache reuses these instead of re-highlighting
/// every block from scratch each spinner frame.
pub(super) struct BodyCache {
    pub(super) width: u16,
    pub(super) rev: u64,
    pub(super) expanded: bool,
    pub(super) env_rev: u64,
    pub(super) lines: Vec<Line<'static>>,
    pub(super) height: u32,
}

impl BodyCache {
    fn matches(&self, key: &BodyKey) -> bool {
        self.width == key.width
            && self.rev == key.rev
            && self.expanded == key.expanded
            && self.env_rev == key.env_rev
    }
}

/// Cache validity key of a block body: the render width, the block's content
/// revision, its expansion state, and (tool blocks only) the diagnostics
/// environment revision.
pub(super) struct BodyKey {
    pub(super) width: u16,
    pub(super) rev: u64,
    pub(super) expanded: bool,
    pub(super) env_rev: u64,
}

/// Wrapped row count of `lines` at `text_width` — the per-line part of
/// [`Segment::measure`] minus the block padding.
pub(super) fn measure_lines(lines: &[Line<'static>], text_width: u16, trim: bool) -> u32 {
    if lines.is_empty() {
        return 0;
    }
    u32::try_from(
        Paragraph::new(lines.to_vec())
            .wrap(Wrap { trim })
            .line_count(text_width),
    )
    .unwrap_or(u32::MAX)
}

/// Consult a block's body cache, rebuilding it through `build` when stale.
/// Returns cloned lines plus the cached measured height.
pub(super) fn cached_body(
    cache: &RefCell<Option<BodyCache>>,
    key: BodyKey,
    text_width: u16,
    trim: bool,
    build: impl FnOnce() -> Vec<Line<'static>>,
) -> (Vec<Line<'static>>, u32) {
    let mut slot = cache.borrow_mut();
    if !slot.as_ref().is_some_and(|c| c.matches(&key)) {
        let lines = build();
        let height = measure_lines(&lines, text_width, trim);
        *slot = Some(BodyCache {
            width: key.width,
            rev: key.rev,
            expanded: key.expanded,
            env_rev: key.env_rev,
            lines,
            height,
        });
    }
    let cached = slot.as_ref().expect("body cache just built");
    (cached.lines.clone(), cached.height)
}

impl Block {
    /// Apply a message to the block, returning whether its state changed.
    pub fn update(&mut self, msg: BlockMessage) -> bool {
        match msg {
            BlockMessage::Toggle => match self {
                Block::Tool(tool) => {
                    tool.toggle();
                    true
                }
                Block::Reasoning(reasoning) => {
                    reasoning.toggle();
                    true
                }
                _ => false,
            },
            BlockMessage::Tool(msg) => match self {
                Block::Tool(tool) => tool.update(msg),
                _ => false,
            },
            BlockMessage::Text(msg) => match self {
                Block::Text(text) => text.update(msg),
                _ => false,
            },
            BlockMessage::Reasoning(msg) => match self {
                Block::Reasoning(reasoning) => reasoning.update(msg),
                _ => false,
            },
        }
    }

    /// Estimated row counters contributed by this block, mirroring what
    /// [`Block::view`] will produce (width-independent; the virtualizer folds
    /// them into an O(1) height estimate per width).
    pub fn est(&self) -> TurnEst {
        match self {
            Block::User(block) => block.est(),
            Block::Steered(block) => block.est(),
            Block::Text(block) => block.est(),
            Block::System(block) => block.est(),
            Block::Tool(block) => block.est(),
            Block::Reasoning(block) => block.est(),
            Block::Context(block) => block.est(),
            Block::Summary | Block::Interrupted | Block::Working | Block::ToolOnlyNote => {
                TurnEst::deco(1)
            }
        }
    }

    /// Project the block into its renderable segment with the measured
    /// height (without hit addresses — the chat engine stamps those by
    /// turn/block position). `None` for blocks with no content; heights are
    /// served from the per-block body cache when unchanged.
    pub fn view(&self, width: u16, env: &ChatEnv, env_rev: u64) -> Option<(Segment, u32)> {
        match self {
            Block::User(block) => block.view(width),
            Block::Steered(block) => block.view(width),
            Block::Text(block) => block.view(width),
            Block::System(block) => block.view(width),
            Block::Tool(block) => Some(block.view(width, env, env_rev)),
            Block::Reasoning(block) => Some(block.view(width)),
            Block::Context(block) => block.view(width),
            Block::Summary => Some(Self::measured(
                Segment::plain(vec![Line::from(
                    Span::raw("◈ summary of earlier conversation")
                        .fg(theme::ACCENT)
                        .italic(),
                )]),
                width,
            )),
            Block::Interrupted => Some(Self::measured(
                Segment::plain(vec![Line::from(
                    Span::raw("(interrupted)").fg(theme::WARNING).italic(),
                )]),
                width,
            )),
            Block::Working => Some(Self::measured(
                Segment::plain(vec![Line::from(vec![
                    spinner::spinner(),
                    Span::raw(" "),
                    Span::raw("(Working...)").fg(theme::TEXT_MUTED),
                ])]),
                width,
            )),
            Block::ToolOnlyNote => Some(Self::measured(
                Segment::plain(vec![Line::from(
                    Span::raw("(tool output only — no text reply)").fg(theme::TEXT_MUTED),
                )]),
                width,
            )),
        }
    }

    fn measured(segment: Segment, width: u16) -> (Segment, u32) {
        let height = segment.measure(width);
        (segment, height)
    }

    /// Terminate a still-running tool call as failed. A turn that reaches
    /// `Done` with a call never returning (a worker that died mid-run) must
    /// not keep its spinner animating forever.
    pub fn finalize_running(&mut self) -> bool {
        match self {
            Block::Tool(tool) => tool.finish_unreturned(),
            _ => false,
        }
    }

    pub fn is_tool(&self) -> bool {
        matches!(self, Block::Tool(_))
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Block::Text(_))
    }

    pub fn tool_matches(&self, name: &str, worker: &Option<String>) -> bool {
        matches!(self, Block::Tool(tool) if tool.matches(name, worker))
    }

    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Block::Tool(tool) => tool.call_id(),
            _ => None,
        }
    }

    pub fn tool_is_running(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.is_running())
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self, Block::Reasoning(reasoning) if reasoning.is_thinking())
    }

    pub fn set_expanded(&mut self, expanded: bool) {
        match self {
            Block::Tool(tool) => tool.set_expanded(expanded),
            Block::Reasoning(reasoning) => reasoning.set_expanded(expanded),
            _ => {}
        }
    }

    pub fn is_expanded(&self) -> bool {
        match self {
            Block::Tool(tool) => tool.is_expanded(),
            Block::Reasoning(reasoning) => reasoning.is_expanded(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration_ms;
    use super::*;
    use shuvarie_core::tool_record::ToolRecord;

    fn shell_record(output: &str) -> ToolRecord {
        ToolRecord {
            name: "run_shell".to_string(),
            args_json: r#"{"command":"ls -la"}"#.to_string(),
            output: output.to_string(),
            stderr: String::new(),
            ok: true,
            worker: None,
            message_id: 1,
            message_seq: 0,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 900,
        }
    }

    #[test]
    fn duration_formats_seconds_and_minutes() {
        assert_eq!(format_duration_ms(0), "0.0s");
        assert_eq!(format_duration_ms(4500), "4.5s");
        assert_eq!(format_duration_ms(10_300), "10.3s");
        assert_eq!(format_duration_ms(59_900), "59.9s");
        assert_eq!(format_duration_ms(60_000), "1m 00s");
        assert_eq!(format_duration_ms(65_400), "1m 05s");
        assert_eq!(format_duration_ms(133_000), "2m 13s");
    }

    #[test]
    fn block_view_heights_match_segment_measure() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let markdown = "# Heading\n\nSome prose with `code` and a list:\n\n```rust\nfn main() { println!(\"a rather long line that wraps at narrow widths\"); }\n```\n";
        let text = TextBlock::new(markdown);
        let prompt = UserPrompt::new("a user request that spans a few lines\nsecond line");
        let reasoning = ReasoningBlock::finished("thought about it\nmore thinking", 1200);
        let mut tool = ToolBlock::from_record(&shell_record(&"output line\n".repeat(40)));
        tool.update(ToolMessage::Finish {
            ok: true,
            output: "output line\n".repeat(40),
            stderr: String::new(),
            file_change: None,
            duration_ms: 900,
        });
        for width in [24u16, 41, 80, 140] {
            let (text_seg, text_h) = text.view(width).expect("text segment");
            assert_eq!(text_seg.measure(width), text_h, "text at {width}");
            let (prompt_seg, prompt_h) = prompt.view(width).expect("prompt segment");
            assert_eq!(prompt_seg.measure(width), prompt_h, "prompt at {width}");
            let (reasoning_seg, reasoning_h) = reasoning.view(width);
            assert_eq!(
                reasoning_seg.measure(width),
                reasoning_h,
                "reasoning at {width}"
            );
            let (tool_seg, tool_h) = tool.view(width, &env, 0);
            assert_eq!(tool_seg.measure(width), tool_h, "tool at {width}");
        }
        // The cached second pass stays consistent with a fresh measure.
        let (_, cached_h) = tool.view(41, &env, 0);
        let (seg, _) = tool.view(41, &env, 0);
        assert_eq!(seg.measure(41), cached_h);
    }
}
