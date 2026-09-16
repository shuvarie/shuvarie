use ratatui::prelude::*;

use crate::tui::session::segment::Segment;
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::theme;

/// Context files loaded for the session (announced once per chat, not per
/// request), rendered as one `◈ Loaded <path>` line per path (contents go
/// to the agent preamble).
pub struct ContextBlock {
    paths: Vec<String>,
}

impl ContextBlock {
    pub fn new(paths: Vec<String>) -> Self {
        Self { paths }
    }

    pub fn view(&self) -> Vec<Segment> {
        if self.paths.is_empty() {
            return Vec::new();
        }
        let lines = self
            .paths
            .iter()
            .map(|path| {
                Line::from(vec![
                    Span::raw("◈").fg(theme::accent()).bold(),
                    Span::raw(format!(" Loaded {path}")).fg(theme::text()),
                ])
            })
            .collect();
        vec![Segment::plain(lines)]
    }

    pub(super) fn est(&self) -> TurnEst {
        TurnEst::deco(self.paths.len() as u32)
    }
}
