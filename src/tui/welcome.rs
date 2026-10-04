use ratatui::layout::Alignment;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::{add_provider::centered_rect, popup, theme};

pub enum WelcomeMessage {
    AddProvider,
    RequestQuit,
}

pub enum WelcomeEffect {
    AddProvider,
    RequestQuit,
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
        // Global quit handler lives behind the overlay router, so the welcome
        // screen must map Ctrl+C itself (mirrors `ConfirmQuit::map_event`).
        if key.modifiers.contains(Modifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(WelcomeMessage::RequestQuit);
        }
        match key.code {
            KeyCode::Enter => Some(WelcomeMessage::AddProvider),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: WelcomeMessage) -> Option<WelcomeEffect> {
        match msg {
            WelcomeMessage::AddProvider => Some(WelcomeEffect::AddProvider),
            WelcomeMessage::RequestQuit => Some(WelcomeEffect::RequestQuit),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 30, area);
        popup::dialog(frame, popup, "Welcome to Shuvarie", dimmed, |inner, buf| {
            let [body_area, help_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);

            let lines = vec![
                Line::from(""),
                Line::from("Connect an LLM provider to begin.")
                    .style(Style::new().fg(theme::text())),
                Line::from(""),
                Line::from(""),
            ];
            Paragraph::new(lines)
                .alignment(Alignment::Center)
                .render(body_area, buf);

            Paragraph::new(theme::help_line(&[
                ("Enter", "add provider"),
                ("Ctrl+C", "quit"),
            ]))
            .fg(theme::text_muted())
            .alignment(Alignment::Center)
            .render(help_area, buf);
        });
    }
}

impl Default for Welcome {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_event_routes_quit_and_add_provider() {
        let welcome = Welcome::new();
        assert!(matches!(
            welcome.map_event(&KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL)),
            Some(WelcomeMessage::RequestQuit)
        ));
        assert!(matches!(
            welcome.map_event(&KeyEvent::new(KeyCode::Enter, Modifiers::NONE)),
            Some(WelcomeMessage::AddProvider)
        ));
        assert!(
            welcome
                .map_event(&KeyEvent::new(KeyCode::Escape, Modifiers::NONE))
                .is_none()
        );
        assert!(
            welcome
                .map_event(&KeyEvent::new(KeyCode::Char('c'), Modifiers::NONE))
                .is_none()
        );
    }

    #[test]
    fn update_maps_messages_to_effects() {
        let mut welcome = Welcome::new();
        assert!(matches!(
            welcome.update(WelcomeMessage::RequestQuit),
            Some(WelcomeEffect::RequestQuit)
        ));
        assert!(matches!(
            welcome.update(WelcomeMessage::AddProvider),
            Some(WelcomeEffect::AddProvider)
        ));
    }
}
