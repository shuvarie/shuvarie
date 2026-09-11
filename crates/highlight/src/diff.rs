use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::{code, theme};

fn line_style(line: &str) -> Style {
    let c = line.chars().next().unwrap_or(' ');
    match c {
        '+' => Style::new().fg(theme::SUCCESS),
        '-' => Style::new().fg(theme::ERROR),
        '@' => Style::new()
            .fg(theme::ACCENT)
            .add_modifier(ratatui::style::Modifier::BOLD),
        '\\' => Style::new().fg(theme::TEXT_MUTED),
        _ => Style::new().fg(theme::TEXT_DIM),
    }
}

pub fn highlight_diff(code_text: &str) -> Vec<Line<'static>> {
    code_text
        .lines()
        .map(|line| {
            code::keep_indent(Line::from(
                Span::raw(code::expand_tabs(line)).style(line_style(line)),
            ))
        })
        .collect()
}
