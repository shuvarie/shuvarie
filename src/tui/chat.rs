use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use shuvarie_core::Config;

use super::theme;

pub struct ChatScreen;

impl ChatScreen {
    pub fn new() -> Self {
        Self
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, config: &Config) {
        let block = theme::section_block("Chat", true);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let provider = config.active_provider.as_deref().unwrap_or("(no provider)");
        let model = config.active_model.as_deref().unwrap_or("(no model)");
        let status = Line::from(vec![
            Span::raw("Provider: ").fg(theme::TEXT_DIM),
            Span::raw(provider.to_string()).fg(theme::ACCENT),
            Span::raw("    Model: ").fg(theme::TEXT_DIM),
            Span::raw(model.to_string()).fg(theme::ACCENT),
        ]);
        let [status_area, body_area] = Layout::vertical([Length(1), Min(0)]).areas(inner);
        frame.render_widget(Paragraph::new(status), status_area);
        frame.render_widget(
            Paragraph::new("Chat view — coming in M4")
                .fg(theme::TEXT_MUTED)
                .alignment(Alignment::Center),
            body_area,
        );
    }
}

impl Default for ChatScreen {
    fn default() -> Self {
        Self::new()
    }
}
