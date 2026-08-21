use ratatui::layout::{Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::KeyEvent;

use crate::tui::components::VersionBar;

use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::logo::Logo;
use super::theme;

pub enum HomeMessage {
    Input(TextAreaMessage),
}

pub enum HomeEffect {
    Submit { content: String },
}

pub struct HomeScreen {
    pub logo: Logo,
    pub input: TextArea,
    pub version_bar: VersionBar,
}

impl HomeScreen {
    pub fn new() -> Self {
        Self {
            logo: Logo::new(),
            input: TextArea::new("Ask anything…"),
            version_bar: VersionBar::new(HorizontalAlignment::Center),
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<HomeMessage> {
        self.input.map_event(key).map(HomeMessage::Input)
    }

    pub fn update(&mut self, msg: HomeMessage) -> Option<HomeEffect> {
        let HomeMessage::Input(m) = msg;
        if let Some(effect) = self.input.update(m) {
            match effect {
                TextAreaEffect::Submit { content } => Some(HomeEffect::Submit { content }),
            }
        } else {
            None
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let [logo_area, input_area, key_hint_area, version_area] = Layout::vertical([
            Length(self.logo.height() as u16),
            Length(3),
            Length(1),
            Length(1),
        ])
        .spacing(1)
        .areas(
            area.centered_horizontally(Max(100))
                .centered_vertically(Ratio(1, 2)),
        );

        self.logo.view(frame, logo_area);
        self.input.view(frame, input_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Enter", "send"),
                ("Ctrl+M", "commands"),
                ("Ctrl+C", "quit"),
            ]))
            .fg(theme::TEXT_MUTED),
            key_hint_area,
        );

        self.version_bar.view(frame, version_area);
    }
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
}
