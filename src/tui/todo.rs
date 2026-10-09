//! The `/todo` popup: a read-only, scrollable view of the session's todo list.
//!
//! The list is a snapshot taken when the popup opens (the session's list is
//! replayed from its `todo` tool records); the agent owns the list, so the
//! popup only walks it — `/todo` again refreshes the snapshot.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use shuvarie_core::tools::todos::{TodoItem, TodoStatus};

use super::{list, popup, theme};
use crate::tui::add_provider::centered_rect;
use crate::tui::list::scroll_offset_for;
use crate::tui::utils::ctrl;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoMessage {
    Next,
    Prev,
    First,
    Last,
    Close,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoEffect {
    Close,
}

/// The `/todo` popup: one row per item on the session's todo list.
pub struct TodoPopup {
    pub open: bool,
    items: Vec<TodoItem>,
    selected: usize,
    offset: usize,
}

impl TodoPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            items: Vec::new(),
            selected: 0,
            offset: 0,
        }
    }

    /// Open on a snapshot of the session's todo list, the first row selected.
    pub fn open(&mut self, items: Vec<TodoItem>) {
        self.open = true;
        self.items = items;
        self.selected = 0;
        self.offset = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.items.clear();
        self.selected = 0;
        self.offset = 0;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<TodoMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(TodoMessage::Next),
                KeyCode::Char('p') => Some(TodoMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape | KeyCode::Char('q') => Some(TodoMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(TodoMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(TodoMessage::Prev),
            KeyCode::Home | KeyCode::Char('g') => Some(TodoMessage::First),
            KeyCode::End | KeyCode::Char('G') => Some(TodoMessage::Last),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: TodoMessage) -> Option<TodoEffect> {
        if !self.open {
            return None;
        }
        match msg {
            TodoMessage::Close => {
                self.close();
                Some(TodoEffect::Close)
            }
            TodoMessage::Next => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + 1).min(self.items.len() - 1);
                    self.recompute_offset();
                }
                None
            }
            TodoMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset();
                None
            }
            TodoMessage::First => {
                self.selected = 0;
                self.recompute_offset();
                None
            }
            TodoMessage::Last => {
                self.selected = self.items.len().saturating_sub(1);
                self.recompute_offset();
                None
            }
        }
    }

    /// The popup's height is a fixed share of the viewport, so the update site
    /// has no list height to scroll against; the render re-derives the window
    /// from the real height each frame (the split every list popup uses).
    fn recompute_offset(&mut self) {
        self.offset = scroll_offset_for(self.selected, self.offset, 0, self.items.len());
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(70, 60, area);
        popup::dialog(frame, popup, "Todos", dimmed, |inner, buf| {
            if inner.width == 0 || inner.height == 0 {
                return;
            }

            let [header_area, list_area, _spacer, help_area] =
                Layout::vertical([Length(1), Min(0), Length(1), Length(1)]).areas(inner);

            let (done, total) = shuvarie_core::tools::todos::done_total(&self.items);
            let header = if total == 0 {
                "No todos in this session.".to_string()
            } else {
                format!("Todos ({done}/{total} done)")
            };
            Paragraph::new(header)
                .fg(theme::text_muted())
                .render(header_area, buf);

            if !self.items.is_empty() {
                let offset = scroll_offset_for(
                    self.selected,
                    self.offset,
                    list_area.height as usize,
                    self.items.len(),
                );
                let items: Vec<ListItem> = self
                    .items
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(list_area.height as usize)
                    .map(|(idx, item)| self.render_row(item, idx == self.selected))
                    .collect();
                Widget::render(List::new(items), list_area, buf);
            }

            let hint = theme::help_line(&[("↑↓", "walk"), ("Esc", "close")]);
            Paragraph::new(hint)
                .fg(theme::text_muted())
                .render(help_area, buf);
        });
    }

    fn render_row(&self, item: &TodoItem, is_selected: bool) -> ListItem<'static> {
        let (glyph, glyph_color) = match item.status {
            TodoStatus::Done => ("x", theme::text_dim()),
            TodoStatus::InProgress => ("~", theme::accent()),
            TodoStatus::Pending => (" ", theme::text_muted()),
        };
        let text_color = match item.status {
            TodoStatus::Done => theme::text_dim(),
            _ => theme::text(),
        };
        let line = Line::from(vec![
            Span::raw(format!("[{glyph}] ")).fg(glyph_color),
            Span::raw(format!("#{}", item.id)).fg(theme::text_muted()),
            Span::raw("  ").fg(theme::text_muted()),
            Span::raw(item.text.clone()).fg(text_color),
        ]);
        list::render_list_item_line(line, is_selected)
    }
}

impl Default for TodoPopup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    fn item(id: u64, text: &str, status: TodoStatus) -> TodoItem {
        TodoItem {
            id,
            text: text.to_string(),
            status,
        }
    }

    fn draw(popup: &TodoPopup, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| popup.view(frame, frame.area(), false))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// The first row carrying `needle` — panics when there is none.
    fn row_with(buf: &ratatui::buffer::Buffer, needle: &str) -> u16 {
        (0..buf.area().height)
            .find(|&y| row_text(buf, y).contains(needle))
            .unwrap_or_else(|| panic!("no row containing {needle:?}"))
    }

    fn items(n: usize) -> Vec<TodoItem> {
        (1..=n as u64)
            .map(|id| TodoItem {
                id,
                text: format!("step {id}"),
                status: TodoStatus::Pending,
            })
            .collect()
    }

    #[test]
    fn open_selects_the_first_row() {
        let mut popup = TodoPopup::new();
        popup.open(items(3));
        assert!(popup.open);
        assert_eq!(popup.selected, 0);
        assert_eq!(popup.items.len(), 3);
    }

    #[test]
    fn escape_closes_the_popup() {
        let mut popup = TodoPopup::new();
        popup.open(items(2));
        assert_eq!(popup.update(TodoMessage::Close), Some(TodoEffect::Close));
        assert!(!popup.open);
        assert!(popup.items.is_empty(), "closing drops the snapshot");
        assert_eq!(popup.update(TodoMessage::Close), None);
    }

    #[test]
    fn view_marks_the_selected_row_and_shows_every_status() {
        let _guard = theme::lock_for_tests();
        let mut popup = TodoPopup::new();
        popup.open(vec![
            item(1, "plan it", TodoStatus::Done),
            item(2, "write tests", TodoStatus::InProgress),
            item(3, "ship it", TodoStatus::Pending),
        ]);
        let buf = draw(&popup, 60, 16);

        row_with(&buf, "Todos (1/3 done)");
        assert!(row_text(&buf, row_with(&buf, "plan it")).contains("[x] #1  plan it"));
        assert!(row_text(&buf, row_with(&buf, "write tests")).contains("[~] #2  write tests"));
        assert!(row_text(&buf, row_with(&buf, "ship it")).contains("[ ] #3  ship it"));

        let first = row_with(&buf, "plan it");
        let marker = row_text(&buf, first).find('▶').expect("walk marker");
        assert_eq!(
            buf[(marker as u16, first)].style().bg,
            Some(theme::accent_bg()),
            "the selected row carries the highlight band"
        );

        popup.update(TodoMessage::Next);
        let buf = draw(&popup, 60, 16);
        assert!(!row_text(&buf, row_with(&buf, "plan it")).contains('▶'));
        assert!(row_text(&buf, row_with(&buf, "write tests")).contains('▶'));
    }

    #[test]
    fn view_reports_an_empty_list() {
        let _guard = theme::lock_for_tests();
        let mut popup = TodoPopup::new();
        popup.open(Vec::new());
        let buf = draw(&popup, 60, 16);
        row_with(&buf, "No todos in this session.");
        row_with(&buf, "Esc close");
    }

    #[test]
    fn navigation_clamps_at_the_ends() {
        let mut popup = TodoPopup::new();
        popup.open(items(3));
        popup.update(TodoMessage::Prev);
        assert_eq!(popup.selected, 0);
        for _ in 0..5 {
            popup.update(TodoMessage::Next);
        }
        assert_eq!(popup.selected, 2, "clamps at the last row");
    }

    #[test]
    fn first_end_and_last_reach_the_ends() {
        let mut popup = TodoPopup::new();
        popup.open(items(5));
        popup.update(TodoMessage::Last);
        assert_eq!(popup.selected, 4);
        popup.update(TodoMessage::First);
        assert_eq!(popup.selected, 0);
    }

    #[test]
    fn empty_list_ignores_navigation() {
        let mut popup = TodoPopup::new();
        popup.open(Vec::new());
        assert_eq!(popup.update(TodoMessage::Next), None);
        assert_eq!(popup.update(TodoMessage::Last), None);
        assert_eq!(popup.selected, 0);
        assert!(popup.open, "an empty popup stays open");
    }

    #[test]
    fn closed_popup_ignores_messages() {
        let mut popup = TodoPopup::new();
        assert_eq!(popup.update(TodoMessage::Next), None);
        assert_eq!(popup.update(TodoMessage::Close), None);
    }

    #[test]
    fn map_event_routes_the_keys() {
        let popup = TodoPopup::new();
        assert_eq!(
            popup.map_event(&key(KeyCode::Down)),
            Some(TodoMessage::Next)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('j'))),
            Some(TodoMessage::Next)
        );
        assert_eq!(popup.map_event(&key(KeyCode::Up)), Some(TodoMessage::Prev));
        assert_eq!(
            popup.map_event(&key(KeyCode::Home)),
            Some(TodoMessage::First)
        );
        assert_eq!(popup.map_event(&key(KeyCode::End)), Some(TodoMessage::Last));
        assert_eq!(
            popup.map_event(&key(KeyCode::Escape)),
            Some(TodoMessage::Close)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('q'))),
            Some(TodoMessage::Close)
        );
        assert_eq!(popup.map_event(&key(KeyCode::Char('x'))), None);
    }
}
