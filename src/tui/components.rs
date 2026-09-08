use std::cell::Cell;

use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph};
use termina::event::{KeyCode, KeyEvent, Modifiers};

use crate::tui::utils::{alt, ctrl};

pub use version_bar::VersionBar;

mod version_bar;

/// One visual row of the input buffer after char-level wrapping and newline splitting.
struct Row {
    /// `(char, byte_index)` for each character on this row.
    chars: Vec<(char, usize)>,
    /// Byte index of the cursor position at the end of this row (a `\n` byte,
    /// the byte before a wrapped char, or `value.len()` for the final row).
    end_byte: usize,
}

pub struct InputBuffer {
    pub value: String,
    pub cursor: usize,
    pub scroll_offset: Cell<usize>,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            scroll_offset: Cell::new(0),
        }
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll_offset.set(0);
    }

    pub fn push(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn push_newline(&mut self) {
        self.value.insert(self.cursor, '\n');
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[..self.cursor].chars().last().unwrap();
            let prev_len = prev.len_utf8();
            self.cursor -= prev_len;
            self.value
                .replace_range(self.cursor..self.cursor + prev_len, "");
        }
    }

    pub fn left(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[..self.cursor].chars().last().unwrap();
            self.cursor -= prev.len_utf8();
        }
    }

    pub fn right(&mut self) {
        if let Some(next) = self.value[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    /// Emacs `beginning-of-line`: move to the start of the current logical line.
    pub fn home(&mut self) {
        self.cursor = self.value[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    /// Emacs `end-of-line`: move to the end of the current logical line.
    pub fn end(&mut self) {
        self.cursor = self.value[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.value.len());
    }

    pub fn delete(&mut self) {
        if self.cursor < self.value.len() {
            let next = self.value[self.cursor..].chars().next().unwrap();
            let next_len = next.len_utf8();
            self.value
                .replace_range(self.cursor..self.cursor + next_len, "");
        }
    }

    /// Emacs `kill-line`: if the cursor is at the end of a logical line (just
    /// before a `\n` or at EOF), delete the newline (joining lines); otherwise
    /// delete from the cursor to the end of the current logical line.
    pub fn kill_to_end(&mut self) {
        if self.cursor == self.value.len() {
            return;
        }
        if self.value[self.cursor..].starts_with('\n') {
            self.value.replace_range(self.cursor..self.cursor + 1, "");
        } else {
            let line_end = self.value[self.cursor..]
                .find('\n')
                .map(|i| self.cursor + i)
                .unwrap_or(self.value.len());
            self.value.replace_range(self.cursor..line_end, "");
        }
    }

    /// Emacs `backward-kill-line` (bound to `Ctrl+U` in this app): delete from
    /// the start of the current logical line to the cursor.
    pub fn kill_to_line_start(&mut self) {
        let line_start = self.value[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.value.replace_range(line_start..self.cursor, "");
        self.cursor = line_start;
    }

    pub fn left_word(&mut self) {
        let chars: Vec<(usize, char)> = self.value[..self.cursor].char_indices().collect();
        if chars.is_empty() {
            return;
        }
        let mut i = chars.len();
        while i > 0 && chars[i - 1].1.is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].1.is_whitespace() {
            i -= 1;
        }
        self.cursor = if i < chars.len() {
            chars[i].0
        } else {
            self.cursor
        };
    }

    pub fn right_word(&mut self) {
        let rest: Vec<(usize, char)> = self.value[self.cursor..]
            .char_indices()
            .map(|(i, c)| (self.cursor + i, c))
            .collect();
        let mut i = 0;
        while i < rest.len() && rest[i].1.is_whitespace() {
            i += 1;
        }
        while i < rest.len() && !rest[i].1.is_whitespace() {
            i += 1;
        }
        if i < rest.len() {
            self.cursor = rest[i].0;
        } else {
            self.cursor = self.value.len();
        }
    }

    /// Move the cursor up one visual row, preserving the visual column.
    pub fn up(&mut self, width: usize) {
        let (row, col) = self.cursor_row_col(width);
        if row == 0 {
            return;
        }
        let rows = self.rows(width);
        self.cursor = row_col_to_byte(&rows[row - 1], col);
    }

    /// Move the cursor down one visual row, preserving the visual column.
    pub fn down(&mut self, width: usize) {
        let (row, col) = self.cursor_row_col(width);
        let rows = self.rows(width);
        if row + 1 >= rows.len() {
            return;
        }
        self.cursor = row_col_to_byte(&rows[row + 1], col);
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.value = s.to_string();
        self.cursor = self.value.len();
        self.scroll_offset.set(0);
    }

    pub fn cursor_char_index(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

    /// Number of display rows the buffer occupies at the given width.
    pub fn row_count(&self, width: usize) -> usize {
        self.rows(width).len()
    }

    /// Compute the visual rows of the buffer at the given width, splitting on
    /// `\n` and wrapping char-by-char using `unicode-width`.
    fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows: Vec<Row> = Vec::new();
        let mut chars: Vec<(char, usize)> = Vec::new();
        let mut col = 0usize;
        for (bi, c) in self.value.char_indices() {
            if c == '\n' {
                rows.push(Row {
                    chars: std::mem::take(&mut chars),
                    end_byte: bi,
                });
                col = 0;
                continue;
            }
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if col + w > width && !chars.is_empty() {
                rows.push(Row {
                    chars: std::mem::take(&mut chars),
                    end_byte: bi,
                });
                col = 0;
            }
            chars.push((c, bi));
            col += w;
        }
        rows.push(Row {
            chars,
            end_byte: self.value.len(),
        });
        rows
    }

    /// `(row, col)` of the cursor at the given width.
    fn cursor_row_col(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        for (ri, r) in rows.iter().enumerate() {
            let mut col = 0usize;
            for (c, bi) in r.chars.iter() {
                if *bi >= self.cursor {
                    return (ri, col);
                }
                col += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
            }
            if self.cursor <= r.end_byte {
                return (ri, col);
            }
        }
        let last = rows.len().saturating_sub(1);
        (last, 0)
    }

    /// Adjust `scroll_offset` so the cursor's row is within the viewport.
    fn ensure_cursor_visible(&self, width: usize, viewport: usize) {
        let (row, _) = self.cursor_row_col(width);
        let total = self.rows(width).len();
        let max_offset = total.saturating_sub(1);
        let mut off = self.scroll_offset.get().min(max_offset);
        if row < off {
            off = row;
        } else if row >= off + viewport.max(1) {
            off = row.saturating_sub(viewport.saturating_sub(1));
        }
        self.scroll_offset.set(off.min(max_offset));
    }

    /// Render the visible slice of display rows as styled `Line`s, with the
    /// cursor cell reversed. The placeholder is rendered by the caller when the
    /// buffer is empty.
    pub fn cursor_lines(
        &self,
        text_color: Color,
        cursor_color: Color,
        width: usize,
        viewport: usize,
    ) -> Vec<Line<'static>> {
        let rows = self.rows(width);
        let total = rows.len();
        let start = self.scroll_offset.get().min(total.saturating_sub(1));
        let text_style = Style::new().fg(text_color);
        let cursor_style = Style::new()
            .fg(cursor_color)
            .add_modifier(Modifier::REVERSED);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for r in rows.iter().skip(start).take(viewport.max(1)) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut found_cursor = false;
            for (c, bi) in r.chars.iter() {
                let is_cursor = *bi == self.cursor && !found_cursor;
                if is_cursor {
                    found_cursor = true;
                }
                let style = if is_cursor { cursor_style } else { text_style };
                spans.push(Span::styled(c.to_string(), style));
            }
            if !found_cursor && self.cursor == r.end_byte {
                spans.push(Span::styled(" ".to_string(), cursor_style));
            }
            if spans.is_empty() {
                let style = if self.cursor == r.end_byte {
                    cursor_style
                } else {
                    text_style
                };
                spans.push(Span::styled(" ".to_string(), style));
            }
            lines.push(Line::from(spans));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(" ".to_string(), cursor_style)));
        }
        lines
    }

    /// Single-line cursor render (no wrapping, whole buffer as one line). Used
    /// by single-line inputs (history search, add-provider fields).
    pub fn cursor_line(&self, text_color: Color, cursor_color: Color) -> Line<'static> {
        let chars: Vec<char> = self.value.chars().collect();
        let cursor_idx = self.cursor_char_index();
        let text_style = Style::new().fg(text_color);
        let cursor_style = Style::new()
            .fg(cursor_color)
            .add_modifier(Modifier::REVERSED);

        let mut spans: Vec<Span<'static>> = Vec::new();

        if chars.is_empty() {
            spans.push(Span::styled(" ".to_string(), cursor_style));
            return Line::from(spans);
        }

        for (i, c) in chars.iter().enumerate() {
            let style = if i == cursor_idx {
                cursor_style
            } else {
                text_style
            };
            spans.push(Span::styled(c.to_string(), style));
        }

        if cursor_idx >= chars.len() {
            spans.push(Span::styled(" ".to_string(), cursor_style));
        }

        Line::from(spans)
    }
}

/// Resolve a `(row, col)` target to a byte index within a `Row`.
fn row_col_to_byte(row: &Row, col: usize) -> usize {
    let mut acc = 0usize;
    for (c, bi) in row.chars.iter() {
        if acc >= col {
            return *bi;
        }
        acc += unicode_width::UnicodeWidthChar::width(*c).unwrap_or(0);
    }
    row.end_byte
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

pub enum TextAreaMessage {
    Input(char),
    Backspace,
    Delete,
    KillToEnd,
    KillToLineStart,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Newline,
    CursorUp,
    CursorDown,
    Submit,
    Clear,
}

pub enum TextAreaEffect {
    Submit { content: String },
}

pub struct TextArea {
    pub buffer: InputBuffer,
    pub placeholder: &'static str,
    pub max_height: u16,
    pub width: Cell<usize>,
}

impl TextArea {
    #[allow(dead_code)]
    pub fn new(placeholder: &'static str) -> Self {
        Self {
            buffer: InputBuffer::new(),
            placeholder,
            max_height: 8,
            width: Cell::new(0),
        }
    }

    pub fn with_max_height(placeholder: &'static str, max_height: u16) -> Self {
        Self {
            buffer: InputBuffer::new(),
            placeholder,
            max_height,
            width: Cell::new(0),
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<TextAreaMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('b') => Some(TextAreaMessage::Left),
                KeyCode::Char('f') => Some(TextAreaMessage::Right),
                KeyCode::Char('a') => Some(TextAreaMessage::Home),
                KeyCode::Char('e') => Some(TextAreaMessage::End),
                KeyCode::Char('d') => Some(TextAreaMessage::Delete),
                KeyCode::Char('h') => Some(TextAreaMessage::Backspace),
                KeyCode::Char('k') => Some(TextAreaMessage::KillToEnd),
                KeyCode::Char('u') => Some(TextAreaMessage::KillToLineStart),
                _ => None,
            };
        }
        if alt(key) {
            return match key.code {
                KeyCode::Char('b') => Some(TextAreaMessage::LeftWord),
                KeyCode::Char('f') => Some(TextAreaMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Enter if key.modifiers.contains(Modifiers::SHIFT) => {
                Some(TextAreaMessage::Newline)
            }
            KeyCode::Enter => Some(TextAreaMessage::Submit),
            KeyCode::Backspace => Some(TextAreaMessage::Backspace),
            KeyCode::Left => Some(TextAreaMessage::Left),
            KeyCode::Right => Some(TextAreaMessage::Right),
            KeyCode::Up => Some(TextAreaMessage::CursorUp),
            KeyCode::Down => Some(TextAreaMessage::CursorDown),
            KeyCode::Home => Some(TextAreaMessage::Home),
            KeyCode::End => Some(TextAreaMessage::End),
            KeyCode::Char(c) => Some(TextAreaMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: TextAreaMessage) -> Option<TextAreaEffect> {
        let width = self.width.get().max(1);
        match msg {
            TextAreaMessage::Input(c) => {
                self.buffer.push(c);
                None
            }
            TextAreaMessage::Newline => {
                self.buffer.push_newline();
                None
            }
            TextAreaMessage::Backspace => {
                self.buffer.backspace();
                None
            }
            TextAreaMessage::Delete => {
                self.buffer.delete();
                None
            }
            TextAreaMessage::KillToEnd => {
                self.buffer.kill_to_end();
                None
            }
            TextAreaMessage::KillToLineStart => {
                self.buffer.kill_to_line_start();
                None
            }
            TextAreaMessage::Left => {
                self.buffer.left();
                None
            }
            TextAreaMessage::Right => {
                self.buffer.right();
                None
            }
            TextAreaMessage::LeftWord => {
                self.buffer.left_word();
                None
            }
            TextAreaMessage::RightWord => {
                self.buffer.right_word();
                None
            }
            TextAreaMessage::Home => {
                self.buffer.home();
                None
            }
            TextAreaMessage::End => {
                self.buffer.end();
                None
            }
            TextAreaMessage::CursorUp => {
                self.buffer.up(width);
                None
            }
            TextAreaMessage::CursorDown => {
                self.buffer.down(width);
                None
            }
            TextAreaMessage::Submit => {
                let content = self.buffer.value.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                self.buffer.clear();
                Some(TextAreaEffect::Submit { content })
            }
            TextAreaMessage::Clear => {
                self.buffer.clear();
                None
            }
        }
    }

    /// The height (in rows, including the block padding) the input wants at
    /// the given content width, capped at `max_height`.
    pub fn desired_height(&self, width: usize) -> u16 {
        let inner_w = width.saturating_sub(4).max(1);
        let rows = self.buffer.row_count(inner_w).max(1) as u16;
        rows.min(self.max_height) + 2
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.value.is_empty()
    }

    #[allow(dead_code)]
    pub fn value(&self) -> &str {
        &self.buffer.value
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    #[allow(dead_code)]
    pub fn set(&mut self, s: &str) {
        self.buffer.set(s);
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::new()
            .bg(crate::tui::theme::SURFACE)
            .padding(ratatui::widgets::Padding::symmetric(2, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width as usize;
        self.width.set(width);
        let viewport = inner.height as usize;
        self.buffer.ensure_cursor_visible(width, viewport);

        if self.buffer.value.is_empty() {
            let mut placeholder_line = self
                .buffer
                .cursor_line(crate::tui::theme::TEXT_MUTED, crate::tui::theme::ACCENT);
            let ph = format!(" {}", self.placeholder);
            placeholder_line.push_span(Span::raw(ph).fg(crate::tui::theme::TEXT_MUTED));
            frame.render_widget(
                Paragraph::new(placeholder_line).alignment(Alignment::Left),
                inner,
            );
        } else {
            let lines = self.buffer.cursor_lines(
                crate::tui::theme::TEXT,
                crate::tui::theme::ACCENT,
                width,
                viewport,
            );
            frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_backspace() {
        let mut b = InputBuffer::new();
        b.push('a');
        b.push('b');
        b.push('c');
        assert_eq!(b.value, "abc");
        assert_eq!(b.cursor, 3);
        b.backspace();
        assert_eq!(b.value, "ab");
        assert_eq!(b.cursor, 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut b = InputBuffer::new();
        b.backspace();
        assert_eq!(b.value, "");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn handles_unicode() {
        let mut b = InputBuffer::new();
        b.push('ä');
        b.push('日');
        assert_eq!(b.value, "ä日");
        b.backspace();
        assert_eq!(b.value, "ä");
    }

    #[test]
    fn cursor_left_right_home_end() {
        let mut b = InputBuffer::new();
        b.set("hello");
        b.left();
        b.left();
        assert_eq!(b.cursor, "hel".len());
        b.right();
        assert_eq!(b.cursor, "hell".len());
        b.home();
        assert_eq!(b.cursor, 0);
        b.end();
        assert_eq!(b.cursor, "hello".len());
    }

    #[test]
    fn home_end_are_line_wise() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        assert_eq!(b.cursor, "hello\n".len());
        b.home();
        assert_eq!(b.cursor, "hello\n".len());
        b.left();
        b.left();
        b.home();
        assert_eq!(b.cursor, 0);
        b.end();
        assert_eq!(b.cursor, "hello".len());
    }

    #[test]
    fn delete_removes_char_right_of_cursor() {
        let mut b = InputBuffer::new();
        b.set("hello");
        b.home();
        b.delete();
        assert_eq!(b.value, "ello");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn delete_at_end_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hi");
        b.delete();
        assert_eq!(b.value, "hi");
        assert_eq!(b.cursor, "hi".len());
    }

    #[test]
    fn kill_to_end_kills_to_logical_line_end() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        b.left();
        b.home();
        b.right();
        b.right();
        b.kill_to_end();
        assert_eq!(b.value, "he\nworld");
        assert_eq!(b.cursor, "he".len());
    }

    #[test]
    fn kill_to_end_at_line_boundary_joins_lines() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        b.left();
        b.home();
        b.end();
        b.kill_to_end();
        assert_eq!(b.value, "helloworld");
        assert_eq!(b.cursor, "hello".len());
    }

    #[test]
    fn kill_to_end_at_eof_is_noop() {
        let mut b = InputBuffer::new();
        b.set("abc");
        b.kill_to_end();
        assert_eq!(b.value, "abc");
        assert_eq!(b.cursor, "abc".len());
    }

    #[test]
    fn kill_to_line_start_deletes_backward() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.end();
        b.left();
        b.kill_to_line_start();
        assert_eq!(b.value, "hello\nd");
        assert_eq!(b.cursor, "hello\n".len());
    }

    #[test]
    fn kill_to_line_start_at_line_start_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        b.kill_to_line_start();
        assert_eq!(b.value, "hello\nworld");
        assert_eq!(b.cursor, "hello\n".len());
    }

    #[test]
    fn push_newline_inserts_and_advances() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.left();
        b.push_newline();
        assert_eq!(b.value, "a\nb");
        assert_eq!(b.cursor, "a\n".len());
    }

    #[test]
    fn up_down_preserve_column_within_bounds() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        b.right();
        b.right();
        b.down(10);
        assert_eq!(b.cursor, "hello\nwo".len());
        b.up(10);
        assert_eq!(b.cursor, "he".len());
    }

    #[test]
    fn up_at_top_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.home();
        b.up(10);
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn down_at_bottom_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hello\nworld");
        b.end();
        b.down(10);
        assert_eq!(b.cursor, "hello\nworld".len());
    }

    #[test]
    fn row_count_wraps_long_lines() {
        let mut b = InputBuffer::new();
        b.set("abcdefghij");
        assert_eq!(b.row_count(5), 2);
        assert_eq!(b.row_count(10), 1);
        assert_eq!(b.row_count(3), 4);
    }

    #[test]
    fn row_count_splits_on_newlines() {
        let mut b = InputBuffer::new();
        b.set("ab\ncd");
        assert_eq!(b.row_count(10), 2);
    }

    #[test]
    fn row_count_counts_wide_chars_by_width() {
        let mut b = InputBuffer::new();
        b.set("a日b日");
        assert_eq!(b.row_count(3), 2);
        assert_eq!(b.row_count(4), 2);
        assert_eq!(b.row_count(5), 2);
        assert_eq!(b.row_count(6), 1);
        assert_eq!(b.row_count(2), 4);
    }

    #[test]
    fn row_count_empty_is_one() {
        let b = InputBuffer::new();
        assert_eq!(b.row_count(10), 1);
    }

    #[test]
    fn ensure_cursor_visible_clamps_to_cursor() {
        let mut b = InputBuffer::new();
        b.set("0\n1\n2\n3\n4\n5\n6\n7\n8\n9");
        b.up(10);
        b.ensure_cursor_visible(10, 3);
        let off = b.scroll_offset.get();
        assert!(off <= 7 && off + 3 > 8, "offset={off}");
    }

    #[test]
    fn left_word_skips_whitespace_then_word() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        b.left_word();
        assert_eq!(b.cursor, "hello ".len());
        b.left_word();
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn right_word_skips_whitespace_then_word() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        b.home();
        b.right_word();
        assert_eq!(b.cursor, "hello".len());
        b.right_word();
        assert_eq!(b.cursor, "hello world".len());
    }

    #[test]
    fn left_word_at_start_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hello");
        b.home();
        b.left_word();
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn right_word_at_end_is_noop() {
        let mut b = InputBuffer::new();
        b.set("hello");
        b.right_word();
        assert_eq!(b.cursor, "hello".len());
    }
}
