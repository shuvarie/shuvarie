use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::add_provider::centered_rect;
use super::theme;

pub enum ConfirmQuitMessage {
    Confirm,
    Cancel,
}

pub enum ConfirmQuitEffect {
    Confirm,
    Cancel,
}

pub struct ConfirmQuit {
    pub open: bool,
}

impl ConfirmQuit {
    pub fn new() -> Self {
        Self { open: false }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ConfirmQuitMessage> {
        if key.modifiers.contains(Modifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(ConfirmQuitMessage::Confirm);
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(ConfirmQuitMessage::Confirm)
            }
            KeyCode::Escape | KeyCode::Char('n') | KeyCode::Char('N') => {
                Some(ConfirmQuitMessage::Cancel)
            }
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ConfirmQuitMessage) -> Option<ConfirmQuitEffect> {
        match msg {
            ConfirmQuitMessage::Confirm => {
                self.close();
                Some(ConfirmQuitEffect::Confirm)
            }
            ConfirmQuitMessage::Cancel => {
                self.close();
                Some(ConfirmQuitEffect::Cancel)
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(45, 25, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Quit?");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let lines = vec![
            Line::from(""),
            Line::from("Are you sure you want to quit?")
                .style(Style::new().fg(theme::TEXT))
                .alignment(Alignment::Center),
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
                ("Enter", "confirm"),
                ("Ctrl+C", "confirm"),
                ("Esc", "cancel"),
            ]))
            .fg(theme::TEXT_MUTED)
            .alignment(Alignment::Center),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

impl Default for ConfirmQuit {
    fn default() -> Self {
        Self::new()
    }
}
