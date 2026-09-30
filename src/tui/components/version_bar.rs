use ratatui::{
    Frame,
    layout::Rect,
    prelude::*,
    text::{Line, Span},
};

use crate::tui::theme;

pub struct VersionBar {
    alignment: HorizontalAlignment,
    line: Line<'static>,
}

impl VersionBar {
    pub fn new(alignment: HorizontalAlignment) -> Self {
        Self {
            alignment,
            line: Self::build_line(alignment),
        }
    }

    /// Re-bakes the cached line after the theme palette changed (theme picker
    /// preview/apply/restore or a config reload); the line's colors are
    /// captured from [`theme`] at build time.
    pub fn theme_changed(&mut self) {
        self.line = Self::build_line(self.alignment);
    }

    fn build_line(alignment: HorizontalAlignment) -> Line<'static> {
        const VERSION: &str = env!("CARGO_PKG_VERSION");
        let mut version = format!("v{VERSION}");
        if cfg!(debug_assertions) {
            version += "-dev";
        }

        Line::from(vec![
            Span::raw("⚔️ Shuvarie ").fg(theme::accent()).bold(),
            Span::raw(version).fg(theme::text_dim()),
        ])
        .alignment(alignment)
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(&self.line, area);
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;
    use shuvarie_core::ThemeColors;

    use super::*;

    fn fgs(bar: &VersionBar) -> Vec<Option<Color>> {
        bar.line.spans.iter().map(|s| s.style.fg).collect()
    }

    #[test]
    fn theme_changed_rebakes_the_line_with_the_new_palette() {
        struct Restore(ThemeColors);
        impl Drop for Restore {
            fn drop(&mut self) {
                theme::set(self.0);
            }
        }
        let _lock = theme::lock_for_tests();
        let _restore = Restore(theme::current());

        let mut bar = VersionBar::new(HorizontalAlignment::Left);
        let palette = theme::current();
        let before = fgs(&bar);
        assert_eq!(
            before,
            vec![
                Some(Color::Rgb(
                    palette.accent.0,
                    palette.accent.1,
                    palette.accent.2
                )),
                Some(Color::Rgb(
                    palette.text_dim.0,
                    palette.text_dim.1,
                    palette.text_dim.2
                )),
            ],
            "the baked line starts on the current palette"
        );

        let storm = ThemeColors::tokyo_night_storm();
        assert_ne!(
            theme::current(),
            storm,
            "the test depends on distinct palettes"
        );
        theme::set(storm);
        bar.theme_changed();
        assert_eq!(
            fgs(&bar),
            vec![
                Some(Color::Rgb(storm.accent.0, storm.accent.1, storm.accent.2)),
                Some(Color::Rgb(
                    storm.text_dim.0,
                    storm.text_dim.1,
                    storm.text_dim.2
                )),
            ],
            "the baked line repaints with the new palette"
        );
        assert_eq!(bar.line.alignment, Some(Alignment::Left), "alignment kept");
    }
}
