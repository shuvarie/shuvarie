//! Shared scaffolding for the TUI's popup dialogs, wrapping
//! [`tui_popup::Popup`] with the active palette: the crate paints the frame
//! (background clear, bordered block, accent title) and this module feeds it
//! the theme's colors. The topmost dialog of a stack paints the ordinary
//! border; a dialog sitting underneath another popup paints the dim border
//! (`dimmed`), so stacked dialogs read top-down.

use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    symbols::border,
    text::Line,
    widgets::{Borders, Widget},
};
use tui_popup::{KnownSize, Popup};

use super::theme;

/// Paints a themed popup dialog across exactly `rect` — the outer area
/// including the border ring (the same `centered_rect` geometry the dialog
/// views have always painted, so update paths that mirror view geometry keep
/// matching). `content` receives the block's inner area and paints the body
/// into it. `dimmed` marks a dialog stacked underneath another popup.
pub fn dialog<F>(frame: &mut Frame<'_>, rect: Rect, title: &str, dimmed: bool, content: F)
where
    F: FnOnce(Rect, &mut Buffer),
{
    paint(frame.buffer_mut(), rect, title, dimmed, content);
}

/// Buffer-level [`dialog`], so tests can paint against a bare [`Buffer`].
pub fn paint<F>(buf: &mut Buffer, rect: Rect, title: &str, dimmed: bool, content: F)
where
    F: FnOnce(Rect, &mut Buffer),
{
    let body = FixedBody {
        content,
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    };
    let popup = Popup::new(body)
        .title(title_line(title))
        .style(Style::new().bg(theme::overlay()).fg(theme::text()))
        .borders(Borders::ALL)
        .border_set(border::ROUNDED)
        .border_style(border_style(dimmed));
    Widget::render(popup, rect, buf);
}

/// A popup body with a size fixed by the caller. tui-popup derives the
/// popup's placement from the body's size, so a body of `rect` minus the
/// border ring centers the popup back onto exactly `rect`.
pub struct FixedBody<F> {
    content: F,
    width: u16,
    height: u16,
}

impl<F> KnownSize for FixedBody<F> {
    fn width(&self) -> usize {
        usize::from(self.width)
    }

    fn height(&self) -> usize {
        usize::from(self.height)
    }
}

impl<F> Widget for FixedBody<F>
where
    F: FnOnce(Rect, &mut Buffer),
{
    fn render(self, area: Rect, buf: &mut Buffer) {
        (self.content)(area, buf);
    }
}

/// The dialog title riding the top border: the accent on a padded label,
/// matching the old borderless title treatment.
fn title_line(title: &str) -> Line<'static> {
    Line::from(format!(" {title} ")).style(
        Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD),
    )
}

/// The frame border: the theme's popup border, dimmed for a dialog that
/// another popup covers.
fn border_style(dimmed: bool) -> Style {
    if dimmed {
        Style::new()
            .fg(theme::popup_border_dim())
            .add_modifier(Modifier::DIM)
    } else {
        Style::new().fg(theme::popup_border())
    }
}

#[cfg(test)]
mod tests {
    use super::theme::lock_for_tests;
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    const RECT: Rect = Rect::new(3, 1, 18, 6);
    const INNER: Rect = Rect::new(4, 2, 16, 4);

    fn draw(dimmed: bool, content: impl FnOnce(Rect, &mut Buffer)) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
        terminal
            .draw(|frame| paint(frame.buffer_mut(), RECT, "Test", dimmed, content))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn dialog_paints_frame_title_and_body_inner() {
        let _guard = lock_for_tests();
        let mut inner_seen = None;
        let buf = draw(false, |inner, buf| {
            Paragraph::new("hello world").render(inner, buf);
            inner_seen = Some(inner);
        });

        assert_eq!(inner_seen, Some(INNER), "content paints the block's inner");
        assert_eq!(buf[(3, 1)].symbol(), "╭");
        assert_eq!(buf[(20, 1)].symbol(), "╮");
        assert_eq!(buf[(3, 6)].symbol(), "╰");
        assert_eq!(buf[(20, 6)].symbol(), "╯");

        let top_row: String = (4..=19).map(|x| buf[(x, 1)].symbol()).collect();
        assert!(top_row.contains("Test"), "{top_row}");
        assert!(top_row.contains('─'), "border trails the title: {top_row}");

        assert_eq!(buf[(4, 2)].symbol(), "h", "body text lands in the inner");
        assert_eq!(buf[(4, 2)].style().fg, Some(theme::text()));
        assert_eq!(buf[(10, 4)].style().bg, Some(theme::overlay()));
        assert_eq!(
            buf[(3, 1)].style().fg,
            Some(theme::popup_border()),
            "the topmost dialog borders with the accent tint"
        );
    }

    #[test]
    fn stacked_dialog_dims_the_border() {
        let _guard = lock_for_tests();
        assert_ne!(theme::popup_border(), theme::popup_border_dim());

        let buf = draw(true, |inner, buf| {
            Paragraph::new("under").render(inner, buf);
        });

        assert_eq!(buf[(3, 1)].style().fg, Some(theme::popup_border_dim()));
        // The body still paints fully: dimming marks the stack, it does not
        // blank the dialog.
        assert_eq!(buf[(4, 2)].symbol(), "u");
    }

    #[test]
    fn content_styles_win_over_the_body_style() {
        let _guard = lock_for_tests();
        let colored = Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::BOLD);
        let buf = draw(false, |inner, buf| {
            Paragraph::new("painted").style(colored).render(inner, buf);
        });

        assert_eq!(buf[(4, 2)].style().fg, Some(theme::accent()));
        assert_eq!(buf[(4, 2)].style().bg, Some(theme::overlay()));
        assert_eq!(buf[(4, 2)].style().add_modifier, Modifier::BOLD);
    }
}
