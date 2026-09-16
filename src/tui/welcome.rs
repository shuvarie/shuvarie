use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::add_provider::centered_rect;
use super::theme;

pub enum WelcomeMessage {
    AddProvider,
}

pub enum WelcomeEffect {
    AddProvider,
}

pub struct Welcome {
    pub open: bool,
}

impl Welcome {
    pub fn new() -> Self {
        Self { open: false }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<WelcomeMessage> {
        match key.code {
            KeyCode::Enter => Some(WelcomeMessage::AddProvider),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: WelcomeMessage) -> Option<WelcomeEffect> {
        match msg {
            WelcomeMessage::AddProvider => Some(WelcomeEffect::AddProvider),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 30, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Welcome to Shuvarie");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let lines = vec![
            Line::from(""),
            Line::from("Connect an LLM provider to begin.").style(Style::new().fg(theme::text())),
            Line::from(""),
            Line::from(""),
        ];

        let content_width = lines
            .iter()
            .map(|l| l.width() as u16)
            .max()
            .unwrap_or(0)
            .min(inner.width);
        let x = inner.x + (inner.width.saturating_sub(content_width)) / 2;
        let text_area = Rect::new(x, inner.y, content_width, inner.height.saturating_sub(1));

        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), text_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Enter", "add provider"),
                ("Ctrl+C", "quit"),
            ]))
            .fg(theme::text_muted()),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

impl Default for Welcome {
    fn default() -> Self {
        Self::new()
    }
}
