//! The active renderer palette: the markdown/code colors every renderer below
//! reads through the accessors in this module. It mirrors the swappable TUI
//! palette (`src/tui/theme.rs`): the TUI calls [`set`] with the resolved
//! palette whenever it previews, applies, or restores a theme, and renders
//! pick up the new colors on their next re-render.

use std::sync::RwLock;

use ratatui::style::{Color, Modifier, Style};
use shuvarie_config::{Rgb, ThemeColors};

const fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// The renderer palette. The default is the Faerun renderer palette's
/// historical constants, so a theme that does not sync still renders exactly
/// as before.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub text: Color,
    pub text_dim: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_bg: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    /// Keywords, tags, and other keyword-family code tokens.
    pub code_keyword: Color,
    /// String literals.
    pub code_string: Color,
    /// Types, class names, and markdown links.
    pub code_type: Color,
    /// Function names and calls.
    pub code_function: Color,
}

impl Palette {
    pub const fn faerun() -> Self {
        Self {
            text: Color::Rgb(224, 216, 196),
            text_dim: Color::Rgb(128, 120, 104),
            text_muted: Color::Rgb(92, 86, 74),
            accent: Color::Rgb(212, 175, 95),
            accent_bg: Color::Rgb(52, 48, 42),
            success: Color::Rgb(138, 146, 90),
            error: Color::Rgb(186, 88, 72),
            warning: Color::Rgb(192, 152, 72),
            code_keyword: Color::Rgb(212, 175, 95),
            code_string: Color::Rgb(160, 176, 118),
            code_type: Color::Rgb(148, 160, 204),
            code_function: Color::Rgb(176, 158, 122),
        }
    }

    /// Maps a resolved TUI palette onto the renderer roles. Plain prose,
    /// status, and background roles reuse the matching theme roles; the code
    /// roles come from the dedicated `code-*` palette roles.
    pub fn from_theme(theme: &ThemeColors) -> Self {
        Self {
            text: color(theme.text),
            text_dim: color(theme.text_dim),
            text_muted: color(theme.text_muted),
            accent: color(theme.accent),
            accent_bg: color(theme.accent_bg),
            success: color(theme.success),
            error: color(theme.error),
            warning: color(theme.warning),
            code_keyword: color(theme.code_keyword),
            code_string: color(theme.code_string),
            code_type: color(theme.code_type),
            code_function: color(theme.code_function),
        }
    }
}

static PALETTE: RwLock<Palette> = RwLock::new(Palette::faerun());

/// Bumped on every palette swap: view caches that bake renderer colors record
/// it and re-render when it moves.
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Swaps the renderer palette to the one mapped from the resolved TUI
/// `colors`. The TUI calls this from its `theme::set` chokepoint, so previews
/// and restores always travel together with the app chrome palette.
pub fn set(colors: ThemeColors) {
    set_palette(Palette::from_theme(&colors));
}

/// Swaps the renderer palette directly.
pub fn set_palette(palette: Palette) {
    *PALETTE.write().expect("renderer palette lock") = palette;
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// How many times the palette has swapped: render caches compare this to the
/// value they rendered at to invalidate their baked colors.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// A snapshot of the active renderer palette (for restores).
pub fn current() -> Palette {
    *PALETTE.read().expect("renderer palette")
}

pub fn text() -> Color {
    current().text
}

pub fn text_dim() -> Color {
    current().text_dim
}

pub fn text_muted() -> Color {
    current().text_muted
}

pub fn accent() -> Color {
    current().accent
}

pub fn accent_bg() -> Color {
    current().accent_bg
}

pub fn success() -> Color {
    current().success
}

pub fn error() -> Color {
    current().error
}

pub fn warning() -> Color {
    current().warning
}

pub fn code_keyword() -> Color {
    current().code_keyword
}

pub fn code_string() -> Color {
    current().code_string
}

pub fn code_type() -> Color {
    current().code_type
}

pub fn code_function() -> Color {
    current().code_function
}

pub fn plain() -> Style {
    Style::new().fg(text())
}

pub fn reasoning() -> Style {
    Style::new().fg(text_dim()).add_modifier(Modifier::ITALIC)
}

pub fn fence() -> Style {
    Style::new().fg(text_muted())
}

pub fn fence_lang() -> Style {
    Style::new().fg(text_dim())
}
