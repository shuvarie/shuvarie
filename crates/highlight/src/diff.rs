use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme;

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

pub fn highlight_diff(code: &str) -> Vec<Line<'static>> {
    let block_bg = theme::SURFACE;
    code.lines()
        .map(|line| {
            let text = if line.is_empty() { " " } else { line };
            Line::from(Span::raw(text.to_string()).style(line_style(line)))
                .style(Style::new().bg(block_bg))
        })
        .collect()
}
