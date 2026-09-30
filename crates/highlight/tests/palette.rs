//! Palette-mutation tests for the renderer theme. They mutate the
//! process-wide palette, so they live in their own test binary, isolated from
//! the unit tests that render against the stock Faerun palette.

use std::sync::MutexGuard;

use ratatui::style::Color;
use shuvarie_config::ThemeColors;
use shuvarie_highlight::theme::{self, Palette};

/// The process-wide palette, serialized across this binary's tests and
/// restored on drop (panic included).
struct Guard {
    _lock: MutexGuard<'static, ()>,
    restore: theme::Palette,
}

fn swap(palette: theme::Palette) -> Guard {
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let restore = theme::current();
    theme::set_palette(palette);
    Guard {
        _lock: lock,
        restore,
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        theme::set_palette(self.restore);
    }
}

#[test]
fn faerun_is_the_stock_palette() {
    let _guard = swap(theme::Palette::faerun());
    assert_eq!(theme::current(), theme::Palette::faerun());
}

#[test]
fn set_maps_the_theme_palette() {
    let _guard = swap(theme::Palette::from_theme(&ThemeColors::tokyo_night_storm()));
    // Prose and status roles come from the theme roles; code roles from the
    // dedicated code-* roles.
    assert_eq!(theme::accent(), Color::Rgb(122, 162, 247));
    assert_eq!(theme::accent_bg(), Color::Rgb(50, 60, 89));
    assert_eq!(theme::code_string(), Color::Rgb(158, 206, 106));
    assert_eq!(theme::code_type(), Color::Rgb(42, 195, 222));
    assert_eq!(theme::code_keyword(), Color::Rgb(125, 207, 255));
    assert_eq!(theme::code_function(), Color::Rgb(122, 162, 247));
    assert_eq!(theme::text(), Color::Rgb(192, 202, 245));
    assert_eq!(theme::text_dim(), Color::Rgb(169, 177, 214));
    assert_eq!(theme::text_muted(), Color::Rgb(86, 95, 137));
    assert_eq!(theme::success(), Color::Rgb(115, 218, 202));
    assert_eq!(theme::error(), Color::Rgb(219, 75, 75));
}

#[test]
fn set_maps_faerun_identity_unchanged() {
    // The stock palette's code roles are the renderer palette's historical
    // constants, so syncing the default theme changes nothing visually.
    let _guard = swap(theme::Palette::from_theme(&ThemeColors::faerun()));
    assert_eq!(theme::code_keyword(), Color::Rgb(212, 175, 95));
    assert_eq!(theme::code_string(), Color::Rgb(160, 176, 118));
    assert_eq!(theme::code_type(), Color::Rgb(148, 160, 204));
    assert_eq!(theme::code_function(), Color::Rgb(176, 158, 122));
    assert_eq!(theme::accent(), Color::Rgb(212, 175, 95));
    assert_eq!(theme::text(), Color::Rgb(224, 216, 196));
    assert_eq!(theme::text_muted(), Color::Rgb(92, 86, 74));
}

#[test]
fn rendering_picks_up_the_swapped_palette_and_restores() {
    let _guard = swap(theme::Palette::from_theme(&ThemeColors::tokyo_night_storm()));

    let head = shuvarie_highlight::md::render("# Heading");
    assert_eq!(head[0].spans[0].style.fg, Some(Color::Rgb(122, 162, 247)));

    // Inline code paints with the theme's accent background; strings pick up
    // the theme's code-string role.
    let inline = shuvarie_highlight::md::render("a `code` b");
    assert!(inline.iter().any(|line| line.spans.iter().any(|span| {
        span.style.bg == Some(Color::Rgb(50, 60, 89)) && span.content.contains("code")
    })));
    let string_code = shuvarie_highlight::syntax::highlight_code("rust", "let s = \"hi\";");
    assert!(
        string_code
            .iter()
            .flat_map(|line| line.spans.iter())
            .any(|span| span.style.fg == Some(Color::Rgb(158, 206, 106)))
    );

    // Style helpers follow too (reasoning flavor carries the dim italic).
    let dim = shuvarie_highlight::md::render_dim("plain thought");
    assert_eq!(dim[0].spans[0].style.fg, Some(Color::Rgb(169, 177, 214)));
} // Guard droppers restore the stock palette here.
