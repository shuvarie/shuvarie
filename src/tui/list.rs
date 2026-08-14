use ratatui::prelude::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::ListItem;

use super::theme;

pub fn scroll_offset_for(
    selected: usize,
    old_offset: usize,
    viewport_height: usize,
    len: usize,
) -> usize {
    if len == 0 || viewport_height == 0 {
        return 0;
    }
    let selected = selected.min(len - 1);
    let max_offset = len.saturating_sub(viewport_height);
    if selected < old_offset {
        selected
    } else if selected >= old_offset + viewport_height {
        selected.saturating_sub(viewport_height - 1).min(max_offset)
    } else {
        old_offset.min(max_offset)
    }
}

pub fn render_list_item(content: String, is_selected: bool) -> ListItem<'static> {
    let prefix: &'static str = if is_selected { "▶ " } else { "  " };
    let line = Line::from(vec![
        Span::raw(prefix).fg(theme::ACCENT),
        Span::raw(content).fg(theme::TEXT),
    ]);
    let style = if is_selected {
        ratatui::style::Style::new()
            .bg(theme::ACCENT_BG)
            .fg(theme::TEXT)
    } else {
        ratatui::style::Style::new()
    };
    ListItem::new(line).style(style)
}

pub fn render_list_item_line(line: Line<'static>, is_selected: bool) -> ListItem<'static> {
    let prefix: &'static str = if is_selected { "▶ " } else { "  " };
    let mut spans = vec![Span::raw(prefix).fg(theme::ACCENT)];
    spans.extend(line.spans);
    let style = if is_selected {
        ratatui::style::Style::new()
            .bg(theme::ACCENT_BG)
            .fg(theme::TEXT)
    } else {
        ratatui::style::Style::new()
    };
    ListItem::new(Line::from(spans)).style(style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_no_scroll_when_visible() {
        assert_eq!(scroll_offset_for(2, 0, 5, 10), 0);
    }

    #[test]
    fn offset_scrolls_down_when_selection_below_view() {
        assert_eq!(scroll_offset_for(7, 0, 5, 10), 3);
    }

    #[test]
    fn offset_scrolls_up_when_selection_above_view() {
        assert_eq!(scroll_offset_for(1, 5, 5, 10), 1);
    }

    #[test]
    fn offset_unchanged_when_selection_in_view() {
        assert_eq!(scroll_offset_for(6, 5, 5, 10), 5);
    }

    #[test]
    fn offset_empty_list_is_zero() {
        assert_eq!(scroll_offset_for(0, 0, 5, 0), 0);
    }

    #[test]
    fn offset_zero_viewport_is_zero() {
        assert_eq!(scroll_offset_for(5, 0, 0, 10), 0);
    }

    #[test]
    fn offset_at_last_item() {
        assert_eq!(scroll_offset_for(9, 5, 5, 10), 5);
    }

    #[test]
    fn offset_clamps_to_max() {
        assert_eq!(scroll_offset_for(9, 0, 5, 10), 5);
    }

    #[test]
    fn offset_selection_above_old_offset_returns_selected() {
        assert_eq!(scroll_offset_for(0, 3, 5, 10), 0);
    }
}
