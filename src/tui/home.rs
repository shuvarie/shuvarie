use ratatui::layout::{Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use termina::event::KeyEvent;

use super::logo::Logo;
use super::widgets::{TextArea, TextAreaEffect, TextAreaMessage};

pub enum HomeMessage {
    Input(TextAreaMessage),
}

pub enum HomeEffect {
    Submit { content: String },
}

pub struct HomeScreen {
    pub logo: Logo,
    pub input: TextArea,
}

impl HomeScreen {
    pub fn new() -> Self {
        Self {
            logo: Logo::new(),
            input: TextArea::new("Ask anything…"),
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
        let [top_spacer, logo_area, gap, input_area, bottom_spacer] = Layout::vertical([
            Min(0),
            Length(self.logo.height() as u16),
            Length(1),
            Length(3),
            Min(0),
        ])
        .areas(area);

        let _ = top_spacer;
        self.logo.view(frame, logo_area);
        let _ = gap;

        self.input.view(frame, input_area);

        let _ = bottom_spacer;
    }
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
}
