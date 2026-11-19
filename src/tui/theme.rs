//! The active TUI palette: one static, swappable set of colors that every
//! view reads through the accessor functions below. `init` installs the
//! resolved theme at startup; `set` swaps it live (the theme picker walks
//! palettes through it and restores the previous one on cancel). Swapping
//! here also re-syncs the markdown/code renderer palette
//! ([`shuvarie_highlight::theme`]), so chat rendering follows every preview,
//! apply, and restore. What the startup probe learned about the terminal's own
//! background lives here too, so the session page can cover it when the
//! terminal and the theme disagree about light and dark ([`session_cover`]).

use std::io::Write;
use std::sync::RwLock;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Padding};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};
use termina::event::Event;
use termina::{EventReader, PlatformTerminal};

use shuvarie_core::{ResolvedTheme, Rgb, ThemeColors, ThemeVariant};

use super::escape;

static ACTIVE: RwLock<ThemeColors> = RwLock::new(ThemeColors::faerun());

/// The terminal's own background as the startup probe found it
/// ([`detect_variant`] records it): the session page's cover is mixed against
/// it.
static TERMINAL: RwLock<TerminalBackground> = RwLock::new(TerminalBackground::DARK);

/// Installs the resolved theme for this run: every palette access below
/// paints its colors from then on. Without a call the built-in Faerun
/// palette stays active.
pub fn init(resolved: ResolvedTheme) {
    set(resolved.colors);
}

/// Swaps the active palette in place: the next frame paints with `colors`.
/// Used by the theme picker to preview a highlighted palette and to restore
/// the previous one. The markdown/code renderer palette is kept in lockstep.
pub fn set(colors: ThemeColors) {
    *ACTIVE.write().expect("theme palette lock") = colors;
    shuvarie_highlight::theme::set(colors);
}

/// A snapshot of the active palette (for restoring it after a preview).
pub fn current() -> ThemeColors {
    active()
}

fn active() -> ThemeColors {
    *ACTIVE.read().expect("theme palette")
}

/// Serializes the palette-mutating tests (the palette is process-wide state).
#[cfg(test)]
pub(crate) fn lock_for_tests() -> std::sync::MutexGuard<'static, ()> {
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A palette and terminal background installed for one test, both handed back
/// when the page drops. The palette is process-wide, so this also holds the
/// palette test lock: a test painting against one known page cannot have
/// another test's palette swap land mid-frame.
#[cfg(test)]
pub(crate) struct TestPage {
    /// The palette installed before the page.
    previous_theme: ThemeColors,

    /// The terminal background installed before the page.
    previous_terminal: TerminalBackground,

    /// Held until the page is dropped, so the palette stays put.
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl TestPage {
    /// A page painted with `theme` on a terminal that reported `background`.
    pub(crate) fn new(theme: ThemeColors, background: Rgb) -> Self {
        Self::install(theme, TerminalBackground::reported(background))
    }

    /// A page painted with `theme` on a terminal that never answered the query,
    /// which detection assumes to be `variant`.
    pub(crate) fn guessed(theme: ThemeColors, variant: ThemeVariant) -> Self {
        Self::install(
            theme,
            TerminalBackground {
                variant,
                reported: None,
            },
        )
    }

    fn install(theme: ThemeColors, terminal: TerminalBackground) -> Self {
        let lock = lock_for_tests();
        let previous_terminal = std::mem::replace(
            &mut *TERMINAL.write().expect("terminal background lock"),
            terminal,
        );
        let previous_theme = current();
        set(theme);
        Self {
            previous_theme,
            previous_terminal,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for TestPage {
    fn drop(&mut self) {
        set(self.previous_theme);
        *TERMINAL.write().expect("terminal background lock") = self.previous_terminal;
    }
}

fn paint(color: Rgb) -> Color {
    Color::Rgb(color.0, color.1, color.2)
}

#[allow(dead_code)]
pub fn bg() -> Color {
    paint(active().bg)
}

/// How much of the terminal's own background shows through the session page's
/// cover; the sheet's remaining share is the theme's page color.
const COVER_TRANSLUCENCY: f32 = 0.15;

/// The wash over the session page's own background when the terminal and the
/// active theme disagree about light and dark — a light cover over a dark
/// terminal under a light theme, a dark one over a light terminal under a dark
/// theme — or `None` when they agree, where the terminal's background already
/// belongs to the page.
///
/// An SGR color carries no alpha, so the sheet cannot be translucent over the
/// screen itself: it is the theme's page color mixed against the terminal's
/// own background by [`COVER_TRANSLUCENCY`], which over a uniform terminal
/// background is what a translucent sheet would look like, and keeps the
/// terminal's tone showing faintly through the page instead of repainting it
/// flat.
pub fn session_cover() -> Option<Color> {
    let terminal = *TERMINAL.read().expect("terminal background lock");
    let bg = active().bg;
    (luminance_variant(bg.0, bg.1, bg.2) != terminal.variant)
        .then(|| paint(blend(terminal.color(), bg, 1.0 - COVER_TRANSLUCENCY)))
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

fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let mix = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// Blends two colors of the active palette, `t` of the way from `from` to `to`,
/// with `t` clamped to the ends of that range. For the effects that *tint* what is
/// already on the page rather than pick a tone off it — the screensaver's lightning
/// wash lifting the plate toward the page's own ink.
///
/// A pair that is not both RGB resolves to one color or the other: a palette is
/// always painted as RGB, so this only catches a caller that reached past the
/// palette accessors.
pub fn mix(from: Color, to: Color, t: f32) -> Color {
    let (Color::Rgb(fr, fg, fb), Color::Rgb(tr, tg, tb)) = (from, to) else {
        return if t < 0.5 { from } else { to };
    };
    paint(blend((fr, fg, fb), (tr, tg, tb), t.clamp(0.0, 1.0)))
}

/// The chat search-match highlight: a golden tint blended between the
/// selection background and the warning accent, distinct from both so
/// matches never read as a mouse selection.
pub fn search_match() -> Color {
    paint(blend(active().selection, active().warning, 0.55))
}

/// The border of a popup dialog: the accent tinted halfway toward the overlay
/// background so the frame reads as quiet chrome around the content.
pub fn popup_border() -> Color {
    paint(blend(active().accent, active().overlay, 0.5))
}

/// The border of a popup stacked underneath another popup: a ghost of the
/// ordinary border, marking the lower dialogs of a stack as inactive.
pub fn popup_border_dim() -> Color {
    paint(blend(active().accent, active().overlay, 0.85))
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

/// What the run knows about the terminal's own background: the mode it falls
/// in, and the color it reported when it answered the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalBackground {
    /// The mode the background falls in, by relative luminance.
    variant: ThemeVariant,

    /// The color the terminal reported; `None` when detection had to guess,
    /// leaving [`Self::color`] to answer the mode's nominal background.
    reported: Option<Rgb>,
}

impl TerminalBackground {
    /// What detection assumes when the terminal never says: a dark one, whose
    /// background is taken to be the dark page's.
    const DARK: Self = Self {
        variant: ThemeVariant::Dark,
        reported: None,
    };

    /// A terminal that answered the query with `rgb`.
    fn reported(rgb: Rgb) -> Self {
        Self {
            variant: luminance_variant(rgb.0, rgb.1, rgb.2),
            reported: Some(rgb),
        }
    }

    /// The terminal's background color: the reported one, else the nominal
    /// page color of the detected mode.
    fn color(self) -> Rgb {
        self.reported.unwrap_or(match self.variant {
            ThemeVariant::Dark => ThemeColors::faerun().bg,
            ThemeVariant::Light => ThemeColors::faerun_light().bg,
        })
    }
}

/// Asks the terminal for its background color (OSC 11) and maps the answer to
/// a theme variant by relative luminance, recording the background for the run
/// ([`session_cover`] mixes the session page against it). Keystrokes typed
/// during the wait are retained by the reader and delivered to the TUI
/// afterwards. Dark is the default when the write fails, the terminal never
/// answers within the timeout, or the answer carries no color.
pub fn detect_variant(term: &mut PlatformTerminal, reader: &EventReader) -> ThemeVariant {
    let background = detect_background(term, reader);
    *TERMINAL.write().expect("terminal background lock") = background;
    background.variant
}

/// The detection itself: what the terminal reported, or the dark guess.
fn detect_background(term: &mut PlatformTerminal, reader: &EventReader) -> TerminalBackground {
    if write!(term, "{}", escape::query_background_color())
        .and_then(|()| term.flush())
        .is_err()
    {
        return TerminalBackground::DARK;
    }
    if let Ok(true) = reader.poll(Some(DETECT_TIMEOUT), background_answer)
        && let Ok(Event::Osc(Osc::ChangeDynamicColors(_, colors))) = reader.read(background_answer)
    {
        for color in colors {
            if let ColorOrQuery::Color(rgb) = color {
                return TerminalBackground::reported((rgb.red, rgb.green, rgb.blue));
            }
        }
    }
    TerminalBackground::DARK
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

    /// The cover's channels, which every test expects as RGB.
    fn channels(cover: Option<Color>) -> Rgb {
        match cover {
            Some(Color::Rgb(red, green, blue)) => (red, green, blue),
            other => panic!("expected an RGB cover, got {other:?}"),
        }
    }

    /// Asserts each channel is within one step of `expected`: the blend rounds.
    fn assert_close(actual: Rgb, expected: Rgb) {
        let pairs = [
            (actual.0, expected.0),
            (actual.1, expected.1),
            (actual.2, expected.2),
        ];
        for (actual, expected) in pairs {
            assert!(
                actual.abs_diff(expected) <= 1,
                "{actual} is not within one step of {expected}"
            );
        }
    }

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

    #[test]
    fn set_swaps_the_active_palette_and_restores_it() {
        // Swap, observe, restore, inside one test so cross-test interleaving
        // cannot flake (the palette global is process-wide).
        let _guard = lock_for_tests();
        let previous = *ACTIVE.read().unwrap();
        set(ThemeColors::tokyo_night_night());
        assert_eq!(*ACTIVE.read().unwrap(), ThemeColors::tokyo_night_night());
        assert_eq!(accent(), Color::Rgb(122, 162, 247));
        assert_eq!(bg(), Color::Rgb(26, 27, 38));
        // The markdown/code renderer palette follows the same swap.
        assert_eq!(
            shuvarie_highlight::theme::code_string(),
            shuvarie_highlight::theme::Palette::from_theme(&ThemeColors::tokyo_night_night())
                .code_string
        );
        set(previous);
        assert_eq!(*ACTIVE.read().unwrap(), previous);
        assert_eq!(
            shuvarie_highlight::theme::current(),
            shuvarie_highlight::theme::Palette::from_theme(&previous),
            "restoring restores the renderer palette too"
        );
    }

    #[test]
    fn session_cover_lifts_a_dark_terminal_under_a_light_theme() {
        let _page = TestPage::new(ThemeColors::faerun_light(), (12, 12, 14));
        // The light theme's page color, 15% of the terminal's dark tone still
        // showing through: neither endpoint, and light in its own right.
        let cover = channels(session_cover());
        assert_close(cover, (212, 208, 201));
        assert_eq!(
            luminance_variant(cover.0, cover.1, cover.2),
            ThemeVariant::Light
        );
    }

    #[test]
    fn session_cover_darkens_a_light_terminal_under_a_dark_theme() {
        let _page = TestPage::new(ThemeColors::faerun(), (255, 255, 255));
        let cover = channels(session_cover());
        assert_close(cover, (54, 54, 57));
        assert_eq!(
            luminance_variant(cover.0, cover.1, cover.2),
            ThemeVariant::Dark
        );
    }

    #[test]
    fn session_cover_is_absent_under_a_matching_dark_page() {
        let _page = TestPage::new(ThemeColors::faerun(), (12, 12, 14));
        assert_eq!(session_cover(), None);
    }

    #[test]
    fn session_cover_is_absent_under_a_matching_light_page() {
        let _page = TestPage::new(ThemeColors::faerun_light(), (255, 255, 255));
        assert_eq!(session_cover(), None);
    }

    #[test]
    fn session_cover_falls_back_to_the_nominal_terminal_background() {
        // Detection gave up, so the terminal counts as dark and its background
        // as the dark page's.
        let _page = TestPage::guessed(ThemeColors::faerun_light(), ThemeVariant::Dark);
        assert_close(channels(session_cover()), (213, 209, 202));
    }
}
