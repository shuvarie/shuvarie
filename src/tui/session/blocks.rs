mod context;
mod reasoning;
mod text;
mod tool;

pub use context::ContextBlock;
pub use reasoning::{ReasoningBlock, ReasoningMessage};
pub use text::{SystemText, TextBlock, TextMessage, UserPrompt};
pub use tool::{ToolBlock, ToolMessage};

use std::collections::BTreeMap;

use ratatui::prelude::*;
use shuvarie_core::DiagnosticInfo;

use super::segment::Segment;
use crate::tui::{spinner, theme};

/// One chat block: a TEA model per variant for the stateful kinds, unit
/// variants for the stateless decorations the engine synthesizes around turns.
pub enum Block {
    User(UserPrompt),
    Text(TextBlock),
    System(SystemText),
    Tool(ToolBlock),
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

    /// Project the block into renderable segments (without hit addresses —
    /// the chat engine stamps those by turn/block position).
    pub fn view(&self, width: u16, env: &ChatEnv) -> Vec<Segment> {
        match self {
            Block::User(block) => block.view(),
            Block::Text(block) => block.view(),
            Block::System(block) => block.view(),
            Block::Tool(block) => vec![block.view(width, env)],
            Block::Reasoning(block) => block.view(),
            Block::Context(block) => block.view(),
            Block::Summary => vec![Segment::plain(vec![Line::from(
                Span::raw("◈ summary of earlier conversation")
                    .fg(theme::ACCENT)
                    .italic(),
            )])],
            Block::Interrupted => vec![Segment::plain(vec![Line::from(
                Span::raw("(interrupted)").fg(theme::WARNING).italic(),
            )])],
            Block::Working => vec![Segment::plain(vec![Line::from(vec![
                spinner::spinner(),
                Span::raw(" "),
                Span::raw("(working…)").fg(theme::TEXT_MUTED),
            ])])],
            Block::ToolOnlyNote => vec![Segment::plain(vec![Line::from(
                Span::raw("(tool output only — no text reply)").fg(theme::TEXT_MUTED),
            )])],
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

    pub fn tool_is_running(&self) -> bool {
        matches!(self, Block::Tool(tool) if tool.is_running())
    }

    pub fn is_thinking(&self) -> bool {
        matches!(self, Block::Reasoning(reasoning) if reasoning.is_thinking())
    }
}
