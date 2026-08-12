use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use super::theme;

pub const LOGO: &[&str] = &[
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

pub fn render(frame: &mut Frame<'_>, area: Rect) {
    let lines: Vec<Line> = LOGO
        .iter()
        .map(|l| Line::from(*l).fg(theme::ACCENT))
        .collect();
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), area);
}

pub fn height() -> usize {
    LOGO.len()
}
