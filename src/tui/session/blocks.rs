mod context;
mod media;
mod reasoning;
mod text;
mod tool;

pub use context::ContextBlock;
pub use media::MediaBlock;
pub use reasoning::{ReasoningBlock, ReasoningMessage};
pub use text::{SteeredPrompt, SystemText, TextBlock, TextMessage, UserPrompt};
pub use tool::{ToolBlock, ToolMessage};

use std::collections::BTreeMap;

use ratatui::prelude::*;
use shuvarie_core::DiagnosticInfo;

use super::media::MediaStore;
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
    matches!(name, "run_shell" | "explore_workspace")
}

/// Tool blocks whose successful output is not previewed when collapsed: file
/// contents, directory listings, and loaded skill instructions the header
/// already summarizes. Failed calls keep their (short) error message visible.
pub(super) fn hides_output_when_collapsed(name: &str) -> bool {
    matches!(name, "read_file" | "list_dir" | "skill")
}

/// Title lines a tool header shows before collapsing the rest: a `run_shell`
/// command with more line feeds than this renders its leading lines plus a
/// `… +N more lines` hint until its title is expanded on its own control.
pub(super) const TITLE_COLLAPSE_LINES: usize = 10;

/// Title rows a `run_shell` command feeds its header onto: one per line feed,
/// and at least one so the `$ ` prompt always has a row.
pub(super) fn shell_command_rows(args: &serde_json::Value) -> usize {
    let cmd = args.get("command").and_then(serde_json::Value::as_str);
    cmd.unwrap_or("").lines().count().max(1)
}

/// The rows a tool header's title renders over: one for every tool but
/// `run_shell`, whose command's line feeds feed the title onto a row per line
/// — capped at [`TITLE_COLLAPSE_LINES`] plus the hint row while the title is
/// collapsed. Mirrors what `ToolBlock`'s header renders.
pub(super) fn title_rows(name: &str, args: &serde_json::Value, expanded: bool) -> u32 {
    if name != "run_shell" {
        return 1;
    }
    let lines = shell_command_rows(args) as u32;
    if expanded || lines <= TITLE_COLLAPSE_LINES as u32 {
        lines
    } else {
        TITLE_COLLAPSE_LINES as u32 + 1
    }
}

/// [`title_rows`] for a persisted call, whose args are still raw JSON.
pub(super) fn title_rows_json(name: &str, args_json: &str, expanded: bool) -> u32 {
    let args: serde_json::Value =
        serde_json::from_str(args_json).unwrap_or(serde_json::Value::Null);
    title_rows(name, &args, expanded)
}

/// Whether a tool header's title holds more lines than the collapse cap, so it
/// carries its own expand/collapse control.
pub(super) fn title_collapsible(name: &str, args: &serde_json::Value) -> bool {
    name == "run_shell" && shell_command_rows(args) > TITLE_COLLAPSE_LINES
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
    Media(Box<MediaBlock>),
    Summary,
    Interrupted,
    Working,
    ToolOnlyNote,
}

/// Messages routed into a block, either from the chat's data flow or from a
/// click hit-test (`Toggle`, `ToggleTitle`).
pub enum BlockMessage {
    /// Flip the block's body rows (the output preview).
    Toggle,
    /// Flip a tool header's title lines — the control a multi-line `run_shell`
    /// command carries independently of its output.
    ToggleTitle,
    /// Flip a tool's code listing — a `codemode` block's script, which collapses
    /// independently of the result below it.
    ToggleCode,
    Tool(ToolMessage),
    Text(TextMessage),
    Reasoning(ReasoningMessage),
}

/// Read-only view context handed to block `view` calls.
pub struct ChatEnv<'a> {
    pub lsp_diagnostics: &'a BTreeMap<String, Vec<DiagnosticInfo>>,
    /// Revision of the environment the diagnostics come from, so blocks can
    /// key environment-dependent caches (e.g. cached tool bodies) on it.
    pub rev: u64,
    /// The chat's received attachment media: image blocks render what has
    /// arrived and show unloaded chips for the rest.
    pub media: &'a MediaStore,
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
            BlockMessage::ToggleTitle => match self {
                Block::Tool(tool) => {
                    tool.toggle_title();
                    true
                }
                _ => false,
            },
            BlockMessage::ToggleCode => match self {
                Block::Tool(tool) => {
                    tool.toggle_code();
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
            Block::Media(block) => block.est(),
            Block::Summary | Block::Interrupted | Block::Working | Block::ToolOnlyNote => {
                TurnEst::deco(1)
            }
        }
    }

    /// Project the block into renderable segments (without hit addresses —
    /// the chat engine stamps those by turn/block position).
    pub fn view(&self, width: u16, env: &ChatEnv) -> Vec<Segment> {
        match self {
            Block::User(block) => block.view(width),
            Block::Steered(block) => block.view(width),
            Block::Text(block) => block.view(width),
            Block::System(block) => block.view(width),
            Block::Tool(block) => vec![block.view(width, env)],
            Block::Reasoning(block) => block.view(width),
            Block::Context(block) => block.view(),
            Block::Media(block) => block.view(width, env.media),
            Block::Summary => vec![Segment::plain(vec![Line::from(
                Span::raw("◈ summary of earlier conversation")
                    .fg(theme::accent())
                    .italic(),
            )])],
            Block::Interrupted => vec![Segment::plain(vec![Line::from(
                Span::raw("(interrupted)").fg(theme::warning()).italic(),
            )])],
            Block::Working => vec![Segment::plain(vec![Line::from(vec![
                spinner::spinner(),
                Span::raw(" "),
                Span::raw("(Working...)").fg(theme::text_muted()),
            ])])],
            Block::ToolOnlyNote => vec![Segment::plain(vec![Line::from(
                Span::raw("(tool output only — no text reply)").fg(theme::text_muted()),
            )])],
        }
    }

    pub fn is_tool(&self) -> bool {
        matches!(self, Block::Tool(_))
    }

    pub fn is_text(&self) -> bool {
        matches!(self, Block::Text(_))
    }

    pub fn tool_matches(&self, name: &str, worker: &Option<String>, spawn: Option<u64>) -> bool {
        matches!(self, Block::Tool(tool) if tool.matches(name, worker, spawn))
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

    /// Whether the block's call is still in flight: running, or queued waiting
    /// for a concurrency slot. Tool events (finishes, streamed chunks, the
    /// promotion that starts a queued call) resolve to a block by this.
    pub fn tool_is_active(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.is_active())
    }

    /// Whether the block's first segment animates per spinner frame: a
    /// thinking reasoning header or a running tool header.
    pub fn spinner_bearing(&self) -> bool {
        match self {
            Block::Reasoning(reasoning) => reasoning.is_thinking(),
            Block::Tool(tool) => tool.is_running(),
            _ => false,
        }
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

    /// Whether the block's header title holds more lines than the collapse cap
    /// and so carries its own expand/collapse control.
    pub fn title_collapsible(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.title_collapsible())
    }

    /// Restore a tool block's title expansion (the persisted side-set re-applied
    /// when an evicted turn materializes).
    pub fn set_title_expanded(&mut self, expanded: bool) {
        if let Block::Tool(tool) = self {
            tool.set_title_expanded(expanded);
        }
    }

    pub fn is_title_expanded(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.is_title_expanded())
    }

    pub fn set_code_expanded(&mut self, expanded: bool) {
        if let Block::Tool(tool) = self {
            tool.set_code_expanded(expanded);
        }
    }

    pub fn is_code_expanded(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.is_code_expanded())
    }

    /// Whether the block carries a code listing with rows hidden behind its own
    /// control (a `codemode` script past the collapse cap).
    pub fn code_collapsible(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.code_collapsible())
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration_ms;

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
}
