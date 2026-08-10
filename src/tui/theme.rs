use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};

pub const BG: Color = Color::Rgb(22, 24, 33);
pub const SURFACE: Color = Color::Rgb(30, 33, 46);
pub const SURFACE_FOCUSED: Color = Color::Rgb(38, 42, 60);
pub const OVERLAY: Color = Color::Rgb(34, 38, 55);

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const ACCENT_BG: Color = Color::Rgb(44, 54, 82);

pub const TEXT: Color = Color::Rgb(205, 214, 244);
pub const TEXT_DIM: Color = Color::Rgb(108, 112, 134);
pub const TEXT_MUTED: Color = Color::Rgb(88, 91, 112);

#[allow(dead_code)]
pub const SUCCESS: Color = Color::Rgb(166, 209, 137);
pub const WARNING: Color = Color::Rgb(245, 194, 99);
pub const ERROR: Color = Color::Rgb(237, 135, 150);

pub fn section_block<'a>(title: &str, focused: bool) -> Block<'a> {
    let bg = if focused { SURFACE_FOCUSED } else { SURFACE };
    let title_style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(bg)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::horizontal(1))
}

pub fn overlay_block<'a>(title: &str) -> Block<'a> {
    let title_style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(OVERLAY)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::uniform(1))
}

pub fn active_marker(is_active: bool) -> &'static str {
    if is_active { "● " } else { "  " }
}

pub fn help_line(bindings: &[(&'static str, &'static str)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in bindings.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("    ").fg(TEXT_MUTED));
        }
        spans.push(Span::raw(*key).fg(ACCENT));
        spans.push(Span::raw(format!(" {label}")).fg(TEXT_MUTED));
    }
    Line::from(spans)
}

pub fn title_bar(
    app_name: &'static str,
    route: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> Line<'static> {
    let mut spans = vec![
        Span::raw(format!(" {app_name}")).fg(ACCENT).bold(),
        Span::raw("  ").fg(TEXT_MUTED),
        Span::raw(route.to_string()).fg(TEXT_DIM),
    ];
    if let Some(p) = provider {
        spans.push(Span::raw("  ").fg(TEXT_MUTED));
        spans.push(Span::raw(p.to_string()).fg(TEXT));
        if let Some(m) = model {
            spans.push(Span::raw(":").fg(TEXT_MUTED));
            spans.push(Span::raw(m.to_string()).fg(TEXT));
        }
    }
    Line::from(spans)
}
