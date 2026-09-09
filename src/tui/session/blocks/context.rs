use std::cell::RefCell;

use ratatui::prelude::*;

use crate::tui::session::blocks::{BodyCache, BodyKey, cached_body};
use crate::tui::session::segment::Segment;
use crate::tui::session::virtualizer::TurnEst;
use crate::tui::theme;

/// Context files loaded for the session (announced once per chat, not per
/// request), rendered as one `◈ Loaded <path>` line per path (contents go
/// to the agent preamble).
pub struct ContextBlock {
    paths: Vec<String>,
    cache: RefCell<Option<BodyCache>>,
}

impl ContextBlock {
    pub fn new(paths: Vec<String>) -> Self {
        Self {
            paths,
            cache: RefCell::new(None),
        }
    }

    pub fn view(&self, width: u16) -> Option<(Segment, u32)> {
        let (lines, height) = cached_body(
            &self.cache,
            BodyKey {
                width,
                rev: 0,
                expanded: false,
                env_rev: 0,
            },
            width.max(1),
            true,
            || {
                self.paths
                    .iter()
                    .map(|path| {
                        Line::from(vec![
                            Span::raw("◈").fg(theme::ACCENT).bold(),
                            Span::raw(format!(" Loaded {path}")).fg(theme::TEXT),
                        ])
                    })
                    .collect()
            },
        );
        if lines.is_empty() {
            return None;
        }
        Some((Segment::plain(lines), height))
    }

    pub(super) fn est(&self) -> TurnEst {
        TurnEst::deco(self.paths.len() as u32)
    }
}
