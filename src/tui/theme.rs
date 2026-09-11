use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};

#[allow(dead_code)]
pub const BG: Color = Color::Rgb(18, 18, 22);
pub const SURFACE: Color = Color::Rgb(28, 28, 34);
pub const SURFACE_FOCUSED: Color = Color::Rgb(38, 38, 46);
pub const OVERLAY: Color = Color::Rgb(34, 34, 42);

pub const ACCENT: Color = Color::Rgb(212, 175, 95);
pub const ACCENT_BG: Color = Color::Rgb(52, 48, 42);

pub const TEXT: Color = Color::Rgb(224, 216, 196);
pub const TEXT_DIM: Color = Color::Rgb(128, 120, 104);
pub const TEXT_MUTED: Color = Color::Rgb(92, 86, 74);

pub const PROMPT_BG: Color = Color::Rgb(44, 38, 30);
pub const RUNNING_BG: Color = Color::Rgb(38, 38, 46);
pub const SUCCESS_BG: Color = Color::Rgb(26, 40, 30);
pub const WARNING_BG: Color = Color::Rgb(45, 37, 23);
pub const ERROR_BG: Color = Color::Rgb(46, 26, 24);

#[allow(dead_code)]
pub const SUCCESS: Color = Color::Rgb(138, 146, 90);
#[allow(dead_code)]
pub const WARNING: Color = Color::Rgb(192, 152, 72);
pub const ERROR: Color = Color::Rgb(186, 88, 72);

#[allow(dead_code)]
pub fn section_block<'a>(title: &str, focused: bool) -> Block<'a> {
    let bg = if focused { SURFACE_FOCUSED } else { SURFACE };
    let title_style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(bg)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::horizontal(1))
}

#[allow(dead_code)]
pub fn panel_block<'a>(title: &str) -> Block<'a> {
    let title_style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(SURFACE)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::new(1, 1, 0, 0))
}

#[allow(dead_code)]
pub fn transparent_block<'a>() -> Block<'a> {
    Block::new().padding(Padding::horizontal(2))
}

pub fn title_header<'a>(text: &str) -> Line<'a> {
    Line::from(format!(" {text} "))
        .style(Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))
        .centered()
}

pub fn overlay_block<'a>(title: &str) -> Block<'a> {
    let title_style = Style::new().fg(ACCENT).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(OVERLAY)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::uniform(1))
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
