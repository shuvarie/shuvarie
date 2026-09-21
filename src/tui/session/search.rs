use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use termina::event::{KeyCode, KeyEvent, KeyEventKind};
use unicode_width::UnicodeWidthChar;

use crate::tui::utils::ctrl;

use super::super::theme;

/// The tooltip's fixed width regardless of the needle's length; a needle
/// longer than the visible budget scrolls inside the prompt instead.
const TOOLTIP_WIDTH: u16 = 30;

/// Search-mode editing messages: text edits move the needle, cursor keys
/// move within the prompt, and [`SearchMessage::Exit`] closes the mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMessage {
    Insert(char),
    Backspace,
    Delete,
    CursorLeft,
    CursorRight,
    CursorHome,
    CursorEnd,
    Paste(String),
    /// A key search mode doesn't use; swallowed so nothing reaches the
    /// prompt underneath while the tooltip is open.
    Swallow,
    /// Leave search mode (Escape).
    Exit,
}

/// The floating chat-search tooltip: a one-row prompt pinned to the top-right
/// corner of the chat pane. While it is open every keypress edits the needle
/// (or scrolls the chat) instead of reaching the input area, and the chat
/// paints live match tints underneath. Purely a TUI model — nothing here is
/// persisted or sent to the core.
pub struct SearchPrompt {
    open: bool,
    buffer: String,
    /// Cursor position as a char index into `buffer`.
    cursor: usize,
}

impl SearchPrompt {
    pub fn new() -> Self {
        Self {
            open: false,
            buffer: String::new(),
            cursor: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Enter search mode. `Some(text)` seeds the needle (the `/search <args>`
    /// form); `None` reopens with the previous text.
    pub fn open(&mut self, initial: Option<&str>) {
        self.open = true;
        if let Some(text) = initial {
            self.buffer = text.to_string();
        }
        self.cursor = self.buffer.chars().count();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Drop the needle (session load/fork/reset); keeps the mode closed.
    pub fn reset(&mut self) {
        self.open = false;
        self.buffer.clear();
        self.cursor = 0;
    }

    /// The active needle: the text only while the tooltip is open and
    /// non-empty; an empty needle disables highlighting.
    pub fn needle(&self) -> Option<String> {
        self.open
            .then(|| self.buffer.clone())
            .filter(|text| !text.is_empty())
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SearchMessage> {
        if !self.open || key.kind == KeyEventKind::Release {
            return None;
        }
        match key.code {
            KeyCode::Escape => Some(SearchMessage::Exit),
            KeyCode::Left => Some(SearchMessage::CursorLeft),
            KeyCode::Right => Some(SearchMessage::CursorRight),
            KeyCode::Home => Some(SearchMessage::CursorHome),
            KeyCode::End => Some(SearchMessage::CursorEnd),
            KeyCode::Backspace => Some(SearchMessage::Backspace),
            KeyCode::Delete => Some(SearchMessage::Delete),
            KeyCode::Char(c) if !ctrl(key) => Some(SearchMessage::Insert(c)),
            _ => None,
        }
    }

    /// Apply an edit; `true` when the needle text changed and the chat must
    /// re-pull it. Cursor-only moves return `false`.
    pub fn update(&mut self, msg: SearchMessage) -> bool {
        match msg {
            SearchMessage::Insert(c) => {
                let byte = self.cursor_byte();
                self.buffer.insert(byte, c);
                self.cursor += 1;
                true
            }
            SearchMessage::Backspace => {
                if self.cursor == 0 {
                    return false;
                }
                self.cursor -= 1;
                let byte = self.cursor_byte();
                self.buffer.replace_range(byte..self.next_byte(byte), "");
                true
            }
            SearchMessage::Delete => {
                let byte = self.cursor_byte();
                if byte >= self.buffer.len() {
                    return false;
                }
                self.buffer.replace_range(byte..self.next_byte(byte), "");
                true
            }
            SearchMessage::CursorLeft => {
                self.cursor = self.cursor.saturating_sub(1);
                false
            }
            SearchMessage::CursorRight => {
                self.cursor = (self.cursor + 1).min(self.buffer.chars().count());
                false
            }
            SearchMessage::CursorHome => {
                self.cursor = 0;
                false
            }
            SearchMessage::CursorEnd => {
                self.cursor = self.buffer.chars().count();
                false
            }
            SearchMessage::Paste(text) => {
                let byte = self.cursor_byte();
                self.cursor += text.chars().count();
                self.buffer.insert_str(byte, &text);
                true
            }
            SearchMessage::Swallow => false,
            SearchMessage::Exit => {
                self.close();
                false
            }
        }
    }

    fn cursor_byte(&self) -> usize {
        self.buffer
            .char_indices()
            .nth(self.cursor)
            .map_or(self.buffer.len(), |(byte, _)| byte)
    }

    fn next_byte(&self, byte: usize) -> usize {
        self.buffer[byte..]
            .chars()
            .next()
            .map_or(byte, |c| byte + c.len_utf8())
    }

    /// The tooltip rect: one title row, one padding row, and the needle row
    /// (`overlay_block` reserves the title plus padding inside), pinned to
    /// the history pane's top-right corner at [`TOOLTIP_WIDTH`], clamped to
    /// the pane when it is narrower. `None` when closed or the pane is too
    /// small to show anything.
    pub fn rect_for(&self, history: Rect) -> Option<Rect> {
        if !self.open || history.width < 12 || history.height < 4 {
            return None;
        }
        let width = TOOLTIP_WIDTH.min(history.width);
        let x = history.x + history.width - width;
        Some(Rect::new(x, history.y, width, 4))
    }

    /// Paint the tooltip over the chat content: title, the needle with a
    /// block cursor, and the visible-match label on the right.
    pub fn view(&self, frame: &mut Frame<'_>, history: Rect, matches: usize) {
        let Some(area) = self.rect_for(history) else {
            return;
        };
        let block = theme::overlay_block("search");
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);

        let inner_w = usize::from(inner.width);
        let label = right_label(matches, !self.buffer.is_empty());
        let label_w = if label.is_empty() {
            0
        } else {
            label.chars().count() + 1
        };
        // With a fixed width a tiny pane can clamp the tooltip below the
        // label's size; drop the label then instead of overlapping it with
        // the needle, and let the needle use the full row.
        let label_fits = label_w <= inner_w;
        let budget = if label_fits {
            inner_w.saturating_sub(label_w)
        } else {
            inner_w
        }
        .max(1);

        let mut spans = self.text_spans(budget);
        spans.push(Span::raw(" ".repeat(budget.saturating_sub(
            spans.iter().map(|s: &Span<'_>| s.width()).sum::<usize>(),
        ))));
        frame.render_widget(Paragraph::new(Line::from(spans)), inner);

        if !label.is_empty() && label_fits {
            frame.render_widget(
                Paragraph::new(label)
                    .fg(theme::text_muted())
                    .alignment(Alignment::Right),
                inner,
            );
        }
    }

    /// The visible window of the needle with the cursor cell styled; the
    /// window keeps the cursor on screen, extending left then right to fill
    /// the budget.
    fn text_spans(&self, budget: usize) -> Vec<Span<'static>> {
        let text_style = Style::new().fg(theme::text());
        let cursor_style = Style::new()
            .fg(theme::text())
            .add_modifier(Modifier::REVERSED);
        let chars: Vec<char> = self.buffer.chars().collect();

        let mut used = 1usize; // the cursor cell is always visible
        let mut start = self.cursor.min(chars.len());
        while start > 0 {
            let w = chars[start - 1].width().unwrap_or(0);
            if used + w > budget {
                break;
            }
            used += w;
            start -= 1;
        }
        let mut end = self.cursor.min(chars.len());
        while end < chars.len() && used + chars[end].width().unwrap_or(0) <= budget {
            used += chars[end].width().unwrap_or(0);
            end += 1;
        }

        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, c) in chars.iter().enumerate().skip(start).take(end - start) {
            let style = if i == self.cursor {
                cursor_style
            } else {
                text_style
            };
            spans.push(Span::styled(c.to_string(), style));
        }
        if self.cursor >= end {
            spans.push(Span::styled(" ".to_string(), cursor_style));
        }
        spans
    }
}

impl Default for SearchPrompt {
    fn default() -> Self {
        Self::new()
    }
}

/// The tooltip's right label: an exit hint while the needle is empty, then
/// the on-screen match count the chat's paint pass reported.
fn right_label(matches: usize, has_text: bool) -> String {
    if !has_text {
        "esc to close".to_string()
    } else if matches == 0 {
        "no matches".to_string()
    } else {
        format!("{matches} match{}", if matches == 1 { "" } else { "es" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn text(p: &SearchPrompt) -> String {
        p.buffer.clone()
    }

    #[test]
    fn open_seeds_the_needle_and_reopen_keeps_text() {
        let mut p = SearchPrompt::new();
        p.open(Some("foo"));
        assert_eq!(text(&p), "foo");
        assert_eq!(p.cursor, 3);
        assert_eq!(p.needle().as_deref(), Some("foo"));

        p.open(None);
        assert_eq!(text(&p), "foo", "bare reopen keeps the previous text");

        p.open(Some("bar"));
        assert_eq!(text(&p), "bar");
    }

    #[test]
    fn editing_updates_the_needle() {
        let mut p = SearchPrompt::new();
        p.open(Some("ab"));
        p.cursor = 1;

        assert!(p.update(SearchMessage::Insert('x')));
        assert_eq!(text(&p), "axb");

        assert!(p.update(SearchMessage::Backspace));
        assert_eq!(text(&p), "ab");
        p.update(SearchMessage::CursorHome);
        assert!(!p.update(SearchMessage::Backspace), "at start: no change");

        assert!(p.update(SearchMessage::Delete));
        assert_eq!(text(&p), "b");
        p.update(SearchMessage::CursorEnd);
        assert!(!p.update(SearchMessage::Delete), "at end: no change");
    }

    #[test]
    fn cursor_moves_do_not_change_the_needle() {
        let mut p = SearchPrompt::new();
        p.open(Some("abc"));
        p.update(SearchMessage::CursorHome);
        assert_eq!(p.cursor, 0);
        p.update(SearchMessage::CursorEnd);
        assert_eq!(p.cursor, 3);
        p.update(SearchMessage::CursorLeft);
        assert_eq!(p.cursor, 2);
        p.update(SearchMessage::CursorRight);
        assert_eq!(p.cursor, 3);
        assert_eq!(p.needle().as_deref(), Some("abc"));
    }

    #[test]
    fn paste_inserts_at_the_cursor() {
        let mut p = SearchPrompt::new();
        p.open(Some("ac"));
        p.cursor = 1;
        assert!(p.update(SearchMessage::Paste("bx".into())));
        assert_eq!(text(&p), "abxc");
        assert_eq!(p.cursor, 3);
    }

    #[test]
    fn empty_or_closed_tooltip_has_no_needle() {
        let mut p = SearchPrompt::new();
        assert!(p.needle().is_none());
        p.open(Some("hi"));
        p.update(SearchMessage::Exit);
        assert!(!p.is_open());
        assert!(p.needle().is_none(), "closed: no needle");
        p.open(None);
        p.update(SearchMessage::Backspace);
        p.update(SearchMessage::Backspace);
        assert!(p.needle().is_none(), "empty text: no needle");
    }

    #[test]
    fn map_event_routes_editing_keys() {
        let mut p = SearchPrompt::new();
        p.open(Some("ab"));
        assert!(matches!(
            p.map_event(&key(KeyCode::Escape)),
            Some(SearchMessage::Exit)
        ));
        assert!(matches!(
            p.map_event(&key(KeyCode::Left)),
            Some(SearchMessage::CursorLeft)
        ));
        assert!(matches!(
            p.map_event(&key(KeyCode::Char('x'))),
            Some(SearchMessage::Insert('x'))
        ));
        assert!(
            p.map_event(&key(KeyCode::Up)).is_none(),
            "scroll keys stay with the chat"
        );
        p.close();
        assert!(p.map_event(&key(KeyCode::Char('x'))).is_none());
    }

    #[test]
    fn tooltip_is_pinned_to_the_history_top_right() {
        let mut p = SearchPrompt::new();
        assert!(p.rect_for(Rect::new(0, 0, 80, 10)).is_none(), "closed");

        p.open(Some("hi"));
        let rect = p.rect_for(Rect::new(2, 5, 80, 10)).expect("open");
        assert_eq!(rect.y, 5, "pinned to the top edge");
        assert_eq!(rect.right(), 82, "pinned to the right edge");
        assert_eq!(rect.width, TOOLTIP_WIDTH, "fixed width");
        assert_eq!(rect.height, 4);

        // A longer needle does not widen the tooltip; it scrolls inside.
        p.update(SearchMessage::Paste(" and a long needle".into()));
        assert_eq!(text(&p), "hi and a long needle");
        assert_eq!(
            p.rect_for(Rect::new(2, 5, 80, 10)).unwrap().width,
            TOOLTIP_WIDTH,
            "fixed width regardless of the needle"
        );

        let narrow = p.rect_for(Rect::new(0, 0, 8, 10));
        assert!(narrow.is_none(), "too narrow: no tooltip");

        // Narrower panes clamp the fixed width instead of hiding it.
        let clamped = p.rect_for(Rect::new(0, 0, 20, 10)).expect("clamped");
        assert_eq!(clamped.width, 20);
        assert_eq!(clamped.right(), 20, "still pinned to the right edge");
    }

    #[test]
    fn long_needle_windows_around_the_cursor() {
        let mut p = SearchPrompt::new();
        p.open(Some("abcdefghij"));

        p.update(SearchMessage::CursorHome);
        let head: String = p
            .text_spans(5)
            .into_iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(head, "abcd", "cursor at the start keeps the head visible");

        p.update(SearchMessage::CursorEnd);
        let tail: String = p
            .text_spans(5)
            .into_iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(
            tail, "ghij ",
            "cursor at the end keeps the tail + cursor cell visible"
        );
    }
}
