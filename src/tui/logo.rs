use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::theme;

const LOGO: &[&str] = &[
    r"     /\      ",
    r"    // \     ",
    r"    || |     ",
    r"    || |     ",
    r"    || |     ",
    r"    || |     ",
    r"    || |     ",
    r"    || |     ",
    r" __ || | __  ",
    r"/___||_|___\ ",
    r"     ww      ",
    r"     MM      ",
    r"    _MM_     ",
    r"   (&<>&)    ",
    r"    ~~~~     ",
];

pub struct Logo;

impl Logo {
    pub fn new() -> Self {
        Self
    }

    pub fn height(&self) -> usize {
        LOGO.len()
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let lines: Vec<Line> = LOGO
            .iter()
            .map(|l| Line::from(*l).fg(theme::ACCENT))
            .collect();
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
    }
}

impl Default for Logo {
    fn default() -> Self {
        Self::new()
    }
}
