use ratatui::{
    layout::Rect,
    prelude::*,
    text::{Line, Span},
    Frame,
};

use crate::tui::theme;

pub struct VersionBar {
    line: Line<'static>,
}

impl VersionBar {
    pub fn new(alignment: HorizontalAlignment) -> Self {
        const VERSION: &str = env!("CARGO_PKG_VERSION");

        Self {
            line: Line::from(vec![
                Span::raw("⚔️ Shuvarie ").fg(theme::ACCENT).bold(),
                Span::raw(format!("v{}", VERSION)).fg(theme::TEXT_DIM),
            ])
            .alignment(alignment),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(&self.line, area);
    }
}
