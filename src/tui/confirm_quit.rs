use ratatui::layout::Alignment;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::{add_provider::centered_rect, popup, theme};

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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(45, 25, area);
        popup::dialog(frame, popup, "Quit?", dimmed, |inner, buf| {
            let [body_area, help_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);

            let lines = vec![
                Line::from(""),
                Line::from("Are you sure you want to quit?").style(Style::new().fg(theme::text())),
                Line::from(""),
            ];
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .render(body_area, buf);

            Paragraph::new(theme::help_line(&[
                ("Enter", "confirm"),
                ("Ctrl+C", "confirm"),
                ("Esc", "cancel"),
            ]))
            .fg(theme::text_muted())
            .alignment(Alignment::Center)
            .render(help_area, buf);
        });
    }
}

impl Default for ConfirmQuit {
    fn default() -> Self {
        Self::new()
    }
}
