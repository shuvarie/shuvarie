use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use shuvarie_core::Config;

pub struct ChatScreen;

impl ChatScreen {
    pub fn new() -> Self {
        Self
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, config: &Config) {
        let block = Block::default().borders(Borders::ALL).title("Chat");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let provider = config.active_provider.as_deref().unwrap_or("(no provider)");
        let model = config.active_model.as_deref().unwrap_or("(no model)");
        let status = format!("Provider: {provider}    Model: {model}");
        let [status_area, body_area] = Layout::vertical([Length(1), Min(0)]).areas(inner);
        frame.render_widget(
            Paragraph::new(status).style(Style::new().dim()),
            status_area,
        );
        frame.render_widget(
            Paragraph::new("Chat view — coming in M4\n\nCtrl+P: command menu    q: quit")
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
