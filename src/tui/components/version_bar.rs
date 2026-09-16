use ratatui::{
    Frame,
    layout::Rect,
    prelude::*,
    text::{Line, Span},
};

use crate::tui::theme;

pub struct VersionBar {
    line: Line<'static>,
}

impl VersionBar {
    pub fn new(alignment: HorizontalAlignment) -> Self {
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        let version = match option_env!("GIT_COMMIT_SHORT_HASH") {
            Some(hash) => format!("v{VERSION}-{hash}"),
            None => format!("v{VERSION}"),
        };

        Self {
            line: Line::from(vec![
                Span::raw("⚔️ Shuvarie ").fg(theme::accent()).bold(),
                Span::raw(version).fg(theme::text_dim()),
            ])
            .alignment(alignment),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(&self.line, area);
    }
}
