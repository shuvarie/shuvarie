use std::io::Write;
use std::sync::OnceLock;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};
use termina::event::Event;
use termina::{EventReader, PlatformTerminal};

use shuvarie_core::{ResolvedTheme, Rgb, ThemeColors, ThemeVariant};

use super::escape;

static ACTIVE: OnceLock<ThemeColors> = OnceLock::new();

/// Installs the resolved theme for this run: every palette access below
/// paints its colors from then on. Without a call the built-in Faerun
/// palette stays active.
pub fn init(resolved: ResolvedTheme) {
    let _ = ACTIVE.set(resolved.colors);
}

fn active() -> &'static ThemeColors {
    ACTIVE.get_or_init(ThemeColors::faerun)
}

fn paint(color: Rgb) -> Color {
    Color::Rgb(color.0, color.1, color.2)
}

#[allow(dead_code)]
pub fn bg() -> Color {
    paint(active().bg)
}

pub fn surface() -> Color {
    paint(active().surface)
}

pub fn surface_focused() -> Color {
    paint(active().surface_focused)
}

pub fn overlay() -> Color {
    paint(active().overlay)
}

pub fn accent() -> Color {
    paint(active().accent)
}

pub fn accent_bg() -> Color {
    paint(active().accent_bg)
}

pub fn selection() -> Color {
    paint(active().selection)
}

pub fn text() -> Color {
    paint(active().text)
}

pub fn text_dim() -> Color {
    paint(active().text_dim)
}

pub fn text_muted() -> Color {
    paint(active().text_muted)
}

pub fn prompt_bg() -> Color {
    paint(active().prompt_bg)
}

pub fn running_bg() -> Color {
    paint(active().running_bg)
}

pub fn success_bg() -> Color {
    paint(active().success_bg)
}

pub fn warning_bg() -> Color {
    paint(active().warning_bg)
}

pub fn error_bg() -> Color {
    paint(active().error_bg)
}

pub fn diff_add_bg() -> Color {
    paint(active().diff_add_bg)
}

pub fn diff_add_emph_bg() -> Color {
    paint(active().diff_add_emph_bg)
}

pub fn diff_del_bg() -> Color {
    paint(active().diff_del_bg)
}

pub fn diff_del_emph_bg() -> Color {
    paint(active().diff_del_emph_bg)
}

pub fn success() -> Color {
    paint(active().success)
}

pub fn warning() -> Color {
    paint(active().warning)
}

pub fn error() -> Color {
    paint(active().error)
}

#[allow(dead_code)]
pub fn section_block<'a>(title: &str, focused: bool) -> Block<'a> {
    let bg = if focused {
        surface_focused()
    } else {
        surface()
    };
    let title_style = Style::new().fg(accent()).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(bg)
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::horizontal(1))
}

#[allow(dead_code)]
pub fn panel_block<'a>(title: &str) -> Block<'a> {
    let title_style = Style::new().fg(accent()).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(surface())
        .title_top(Line::from(format!(" {title} ")).style(title_style))
        .padding(Padding::new(1, 1, 0, 0))
}

#[allow(dead_code)]
pub fn transparent_block<'a>() -> Block<'a> {
    Block::new().padding(Padding::horizontal(2))
}

pub fn title_header<'a>(text: &str) -> Line<'a> {
    Line::from(format!(" {text} "))
        .style(Style::new().fg(accent()).add_modifier(Modifier::BOLD))
        .centered()
}

pub fn overlay_block<'a>(title: &str) -> Block<'a> {
    let title_style = Style::new().fg(accent()).add_modifier(Modifier::BOLD);
    Block::new()
        .bg(overlay())
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
            spans.push(Span::raw("    ").fg(text_muted()));
        }
        spans.push(Span::raw(*key).fg(accent()));
        spans.push(Span::raw(format!(" {label}")).fg(text_muted()));
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
        Span::raw(format!(" {app_name}")).fg(accent()).bold(),
        Span::raw("  ").fg(text_muted()),
        Span::raw(route.to_string()).fg(text_dim()),
    ];
    if let Some(p) = provider {
        spans.push(Span::raw("  ").fg(text_muted()));
        spans.push(Span::raw(p.to_string()).fg(text()));
        if let Some(m) = model {
            spans.push(Span::raw(":").fg(text_muted()));
            spans.push(Span::raw(m.to_string()).fg(text()));
        }
    }
    Line::from(spans)
}

/// How long the terminal gets to answer the background color query before
/// detection gives up and assumes a dark terminal.
const DETECT_TIMEOUT: Duration = Duration::from_millis(250);

/// Asks the terminal for its background color (OSC 11) and maps the answer to
/// a theme variant by relative luminance. Keystrokes typed during the wait are
/// retained by the reader and delivered to the TUI afterwards. Dark is the
/// default when the write fails, the terminal never answers within the
/// timeout, or the answer carries no color.
pub fn detect_variant(term: &mut PlatformTerminal, reader: &EventReader) -> ThemeVariant {
    if write!(term, "{}", escape::query_background_color())
        .and_then(|()| term.flush())
        .is_err()
    {
        return ThemeVariant::Dark;
    }
    if let Ok(true) = reader.poll(Some(DETECT_TIMEOUT), background_answer)
        && let Ok(Event::Osc(Osc::ChangeDynamicColors(_, colors))) = reader.read(background_answer)
    {
        for color in colors {
            if let ColorOrQuery::Color(rgb) = color {
                return luminance_variant(rgb.red, rgb.green, rgb.blue);
            }
        }
    }
    ThemeVariant::Dark
}

/// Matches the OSC 11 answer: a background color report carrying an RGB value.
/// Non-matching events (keys typed during detection, other protocol responses)
/// are retained by the reader.
fn background_answer(event: &Event) -> bool {
    matches!(
        event,
        Event::Osc(Osc::ChangeDynamicColors(DynamicColorNumber::TextBackgroundColor, colors))
            if colors.iter().any(|color| matches!(color, ColorOrQuery::Color(_)))
    )
}

/// Maps a background color to a theme variant by relative luminance: light
/// terminals above the midpoint, dark below.
fn luminance_variant(red: u8, green: u8, blue: u8) -> ThemeVariant {
    if relative_luminance(red, green, blue) >= 0.5 {
        ThemeVariant::Light
    } else {
        ThemeVariant::Dark
    }
}

/// Relative luminance (BT.709 weights over linearized sRGB channels).
fn relative_luminance(red: u8, green: u8, blue: u8) -> f64 {
    let channel = |value: u8| -> f64 {
        let value = f64::from(value) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luminance_detects_light_and_dark_backgrounds() {
        assert_eq!(luminance_variant(0, 0, 0), ThemeVariant::Dark);
        assert_eq!(
            luminance_variant(18, 18, 22),
            ThemeVariant::Dark,
            "the Faerun dark background"
        );
        assert_eq!(luminance_variant(127, 127, 127), ThemeVariant::Dark);
        assert_eq!(luminance_variant(255, 255, 255), ThemeVariant::Light);
        assert_eq!(
            luminance_variant(247, 243, 234),
            ThemeVariant::Light,
            "the Faerun light background"
        );
        assert_eq!(luminance_variant(200, 200, 200), ThemeVariant::Light);
    }
}
