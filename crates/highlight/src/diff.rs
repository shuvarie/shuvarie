use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::{code, theme};

fn line_style(line: &str) -> Style {
    let c = line.chars().next().unwrap_or(' ');
    match c {
        '+' => Style::new().fg(theme::success()),
        '-' => Style::new().fg(theme::error()),
        '@' => Style::new()
            .fg(theme::accent())
            .add_modifier(ratatui::style::Modifier::BOLD),
        '\\' => Style::new().fg(theme::text_muted()),
        _ => Style::new().fg(theme::text_dim()),
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
