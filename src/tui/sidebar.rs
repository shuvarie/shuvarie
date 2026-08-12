use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};

use super::theme;

pub struct SidebarData<'a> {
    pub version: &'a str,
    pub tokens: u64,
    pub cost: f64,
    pub provider: Option<&'a str>,
    pub model: Option<&'a str>,
}

pub fn render(frame: &mut Frame<'_>, area: Rect, data: SidebarData<'_>) {
    let block = Block::new()
        .bg(theme::SURFACE)
        .padding(Padding::new(2, 2, 1, 1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(vec![
        Span::raw("Shuvarie ").fg(theme::ACCENT).bold(),
        Span::raw(format!("v{}", data.version)).fg(theme::TEXT_DIM),
    ]));
    lines.push(Line::from(""));

    if let Some(p) = data.provider {
        let mut line = vec![Span::raw(p.to_string()).fg(theme::TEXT)];
        if let Some(m) = data.model {
            line.push(Span::raw(":").fg(theme::TEXT_MUTED));
            line.push(Span::raw(m.to_string()).fg(theme::TEXT_DIM));
        }
        lines.push(Line::from(line));
        lines.push(Line::from(""));
    }

    lines.push(Line::from("Context").fg(theme::ACCENT).bold());
    lines.push(Line::from(format!("  {} tokens", data.tokens)).fg(theme::TEXT_DIM));
    lines.push(Line::from(format!("  ${:.2}", data.cost)).fg(theme::TEXT_DIM));
    lines.push(Line::from(""));

    lines.push(Line::from("LSP").fg(theme::ACCENT).bold());
    lines.push(Line::from("  inactive").fg(theme::TEXT_MUTED));
    lines.push(Line::from(""));

    lines.push(Line::from("Skills").fg(theme::ACCENT).bold());
    lines.push(Line::from("  inactive").fg(theme::TEXT_MUTED));

    let para = Paragraph::new(lines).alignment(Alignment::Left);
    frame.render_widget(para, inner);
}
