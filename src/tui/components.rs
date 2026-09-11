use std::cell::Cell;

use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph};
use termina::event::{KeyCode, KeyEvent, Modifiers};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::theme;
use crate::tui::utils::{alt, ctrl};

pub use version_bar::VersionBar;

mod version_bar;

/// Pastes longer than this many lines collapse into an inline
/// `[pasted N lines]` chip; shorter pastes insert verbatim.
const COMPACT_PASTE_LINES: usize = 2;

// A compacted paste is stored as one marker char in `value` from the BMP
// private use area (U+E000..=U+F8FF); `marker_id` (the offset from the base)
// indexes the `pastes` slots holding the real payload.
const PASTE_MARKER_BASE: u32 = 0xE000;
const PASTE_MARKER_LAST: u32 = 0xF8FF;

fn paste_marker(id: usize) -> Option<char> {
    let code = PASTE_MARKER_BASE + id as u32;
    char::from_u32(code).filter(|c| (*c as u32) <= PASTE_MARKER_LAST)
}

fn marker_id(c: char) -> Option<usize> {
    let code = c as u32;
    (PASTE_MARKER_BASE..=PASTE_MARKER_LAST)
        .contains(&code)
        .then(|| (code - PASTE_MARKER_BASE) as usize)
}

/// Normalize a terminal paste payload: CR/CRLF become `\n` so pasted line
/// breaks stay line breaks in the buffer.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Flatten a paste payload to a single line (for single-line inputs).
pub fn flatten_newlines(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

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
    pastes: Vec<String>,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            scroll_offset: Cell::new(0),
            pastes: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll_offset.set(0);
        self.pastes.clear();
    }

    pub fn push(&mut self, c: char) {
        self.insert_char(c);
    }

    pub fn push_newline(&mut self) {
        self.insert_char('\n');
    }

    fn insert_char(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.value.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Insert pasted text: payloads of more than `COMPACT_PASTE_LINES` lines
    /// collapse into an inline `[pasted N lines]` chip that expands back to
    /// the full text on submit (`expanded`) and is deleted as one unit.
    pub fn paste(&mut self, text: &str) {
        let text = normalize_newlines(text);
        if text.lines().count() > COMPACT_PASTE_LINES
            && let Some(marker) = paste_marker(self.pastes.len())
        {
            self.pastes.push(text);
            self.insert_char(marker);
            return;
        }
        self.insert_str(&text);
    }

    /// The buffer content with compacted pastes expanded back to their text.
    pub fn expanded(&self) -> String {
        if self.pastes.iter().all(String::is_empty) {
            return self.value.clone();
        }
        let mut out = String::with_capacity(self.value.len());
        for c in self.value.chars() {
            match marker_id(c) {
                Some(id) => out.push_str(self.pastes.get(id).map(String::as_str).unwrap_or("")),
                None => out.push(c),
            }
        }
        out
    }

    fn drop_paste(&mut self, marker: char) {
        if let Some(slot) = marker_id(marker).and_then(|id| self.pastes.get_mut(id)) {
            slot.clear();
        }
    }

    fn paste_label(&self, id: usize) -> String {
        let lines = self
            .pastes
            .get(id)
            .map(|content| content.lines().count())
            .unwrap_or(0);
        format!("[pasted {lines} lines]")
    }

    /// The paste marker starting at byte `bi`, when `value[bi..]` holds one.
    fn marker_id_at(&self, bi: usize) -> Option<usize> {
        self.value[bi..].chars().next().and_then(marker_id)
    }

    /// Remove `range` from `value`, clearing the paste slot of every marker
    /// the range swallowed so removed payloads are not retained.
    fn cut_range(&mut self, range: std::ops::Range<usize>) {
        let markers: Vec<char> = self.value[range.clone()]
            .chars()
            .filter(|c| marker_id(*c).is_some())
            .collect();
        self.value.replace_range(range, "");
        for marker in markers {
            self.drop_paste(marker);
        }
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let prev = self.value[..self.cursor].chars().last().unwrap();
            let start = self.cursor - prev.len_utf8();
            self.cut_range(start..self.cursor);
            self.cursor = start;
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
            let end = self.cursor + next.len_utf8();
            self.cut_range(self.cursor..end);
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
            self.cut_range(self.cursor..self.cursor + 1);
        } else {
            let line_end = self.value[self.cursor..]
                .find('\n')
                .map(|i| self.cursor + i)
                .unwrap_or(self.value.len());
            self.cut_range(self.cursor..line_end);
        }
    }

    /// Emacs `backward-kill-line` (bound to `Ctrl+U` in this app): delete from
    /// the start of the current logical line to the cursor.
    pub fn kill_to_line_start(&mut self) {
        let line_start = self.value[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        self.cut_range(line_start..self.cursor);
        self.cursor = line_start;
    }

    pub fn left_word(&mut self) {
        let chars: Vec<(usize, char)> = self.value[..self.cursor].char_indices().collect();
        if chars.is_empty() {
            return;
        }
        let mut i = chars.len();
        while i > 0 && self.is_word_break(chars[i - 1].1) {
            i -= 1;
        }
        while i > 0 && !self.is_word_break(chars[i - 1].1) {
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
        while i < rest.len() && self.is_word_break(rest[i].1) {
            i += 1;
        }
        while i < rest.len() && !self.is_word_break(rest[i].1) {
            i += 1;
        }
        if i < rest.len() {
            self.cursor = rest[i].0;
        } else {
            self.cursor = self.value.len();
        }
    }

    /// Word-boundary predicate: whitespace and paste markers both end a word,
    /// so word jumps land next to a paste placeholder instead of inside it.
    fn is_word_break(&self, c: char) -> bool {
        c.is_whitespace() || marker_id(c).is_some()
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
        self.pastes.clear();
    }

    pub fn cursor_char_index(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

    /// Number of display rows the buffer occupies at the given width.
    pub fn row_count(&self, width: usize) -> usize {
        self.rows(width).len()
    }

    /// Compute the visual rows of the buffer at the given width, splitting on
    /// `\n` and wrapping char-by-char using `unicode-width`. Compacted pastes
    /// expand to their inline label; a label that fits on an empty row breaks
    /// the row before it so it never splits needlessly.
    fn rows(&self, width: usize) -> Vec<Row> {
        let width = width.max(1);
        let mut rows: Vec<Row> = Vec::new();
        let mut chars: Vec<(char, usize)> = Vec::new();
        let mut col = 0usize;
        let push_row = |rows: &mut Vec<Row>, chars: &mut Vec<(char, usize)>, end_byte: usize| {
            rows.push(Row {
                chars: std::mem::take(chars),
                end_byte,
            });
        };
        for (bi, c) in self.value.char_indices() {
            if c == '\n' {
                push_row(&mut rows, &mut chars, bi);
                col = 0;
                continue;
            }
            if let Some(id) = marker_id(c) {
                let label = self.paste_label(id);
                let label_w = label.width();
                if label_w > width {
                    for lc in label.chars() {
                        let w = UnicodeWidthChar::width(lc).unwrap_or(0);
                        if col + w > width && !chars.is_empty() {
                            push_row(&mut rows, &mut chars, bi);
                            col = 0;
                        }
                        chars.push((lc, bi));
                        col += w;
                    }
                    continue;
                }
                if col + label_w > width && !chars.is_empty() {
                    push_row(&mut rows, &mut chars, bi);
                    col = 0;
                }
                for lc in label.chars() {
                    chars.push((lc, bi));
                }
                col += label_w;
                continue;
            }
            let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if col + w > width && !chars.is_empty() {
                push_row(&mut rows, &mut chars, bi);
                col = 0;
            }
            chars.push((c, bi));
            col += w;
        }
        push_row(&mut rows, &mut chars, self.value.len());
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
                let style = if is_cursor {
                    cursor_style
                } else if self.marker_id_at(*bi).is_some() {
                    paste_label_style()
                } else {
                    text_style
                };
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
            let is_cursor = i == cursor_idx;
            if let Some(id) = marker_id(*c) {
                for (li, lc) in self.paste_label(id).chars().enumerate() {
                    let style = if is_cursor && li == 0 {
                        cursor_style
                    } else {
                        paste_label_style()
                    };
                    spans.push(Span::styled(lc.to_string(), style));
                }
                continue;
            }
            let style = if is_cursor { cursor_style } else { text_style };
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

fn paste_label_style() -> Style {
    Style::new()
        .fg(theme::ACCENT)
        .bg(theme::ACCENT_BG)
        .add_modifier(Modifier::BOLD)
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

pub enum TextAreaMessage {
    Input(char),
    Paste(String),
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
            TextAreaMessage::Paste(text) => {
                self.buffer.paste(&text);
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
                let content = self.buffer.expanded().trim().to_string();
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

    /// `text_color` recolors the buffer text — bash mode, where a submit
    /// runs the prompt as a local shell command.
    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, text_color: Color) {
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
            let lines =
                self.buffer
                    .cursor_lines(text_color, crate::tui::theme::ACCENT, width, viewport);
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

    #[test]
    fn view_colors_text_for_bash_mode() {
        use ratatui::{Terminal, backend::TestBackend};

        let mut area = TextArea::with_max_height("p", 8);
        area.set("hi");
        for text_color in [crate::tui::theme::TEXT, crate::tui::theme::ACCENT] {
            let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
            terminal
                .draw(|frame| area.view(frame, Rect::new(0, 0, 40, 5), text_color))
                .unwrap();
            let buf = terminal.backend().buffer();
            let cell = &buf[(2, 1)];
            assert_eq!(cell.symbol(), "h");
            assert_eq!(cell.style().fg, Some(text_color));
        }
    }

    #[test]
    fn paste_short_inserts_verbatim() {
        let mut b = InputBuffer::new();
        b.paste("hello");
        assert_eq!(b.value, "hello");
        assert_eq!(b.cursor, "hello".len());
    }

    #[test]
    fn paste_two_lines_inserts_verbatim() {
        let mut b = InputBuffer::new();
        b.paste("a\nb");
        assert_eq!(b.value, "a\nb");
        assert_eq!(b.expanded(), "a\nb");
    }

    #[test]
    fn paste_normalizes_crlf() {
        let mut b = InputBuffer::new();
        b.paste("a\r\nb");
        assert_eq!(b.value, "a\nb");
        b.clear();
        b.paste("a\r\nb\rc\nd");
        assert_eq!(b.value.chars().count(), 1);
        assert_eq!(b.expanded(), "a\nb\nc\nd");
    }

    #[test]
    fn paste_many_lines_compacts_to_marker() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        assert_eq!(b.value.chars().count(), 1);
        assert_eq!(b.expanded(), "l1\nl2\nl3");
        assert_eq!(b.cursor, b.value.len());
        assert_eq!(b.row_count(60), 1);
    }

    #[test]
    fn paste_label_renders_inline_without_content() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        let lines = b.cursor_lines(crate::tui::theme::TEXT, crate::tui::theme::ACCENT, 60, 8);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("[pasted 3 lines]"), "text={text}");
        assert!(!text.contains("l1"), "content must stay hidden: {text}");
        let chip = lines[0]
            .spans
            .iter()
            .find(|s| s.style.bg == Some(crate::tui::theme::ACCENT_BG))
            .expect("chip background");
        assert_eq!(chip.style.fg, Some(crate::tui::theme::ACCENT));
    }

    #[test]
    fn paste_label_breaks_row_when_it_does_not_fit() {
        let mut b = InputBuffer::new();
        b.push('x');
        b.paste("l1\nl2\nl3");
        assert_eq!(b.row_count(17), 1);
        assert_eq!(b.row_count(16), 2);
    }

    #[test]
    fn backspace_deletes_whole_paste() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.paste("l1\nl2\nl3");
        assert_eq!(b.value.chars().count(), 3);
        b.backspace();
        assert_eq!(b.value, "ab");
        assert_eq!(b.expanded(), "ab");
        assert_eq!(b.cursor, "ab".len());
    }

    #[test]
    fn delete_removes_whole_paste() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        b.push('x');
        b.home();
        b.delete();
        assert_eq!(b.value, "x");
        assert_eq!(b.expanded(), "x");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn left_right_treat_marker_as_single_unit() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.paste("l1\nl2\nl3");
        let marker_bi = "ab".len();
        b.left();
        assert_eq!(b.cursor, marker_bi);
        b.left();
        assert_eq!(b.cursor, "a".len());
        b.right();
        assert_eq!(b.cursor, marker_bi);
        b.right();
        assert_eq!(b.cursor, b.value.len());
    }

    #[test]
    fn word_movement_treats_paste_marker_as_boundary() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        b.insert_str("world");
        let marker_len = b.value.chars().next().unwrap().len_utf8();
        b.home();
        b.right_word();
        assert_eq!(b.cursor, b.value.len());
        b.left_word();
        assert_eq!(b.cursor, marker_len);
        b.left_word();
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn multiple_pastes_expand_in_order() {
        let mut b = InputBuffer::new();
        b.paste("a1\na2\na3");
        b.push(' ');
        b.paste("b1\nb2\nb3");
        assert_eq!(b.expanded(), "a1\na2\na3 b1\nb2\nb3");
    }

    #[test]
    fn deleting_one_paste_keeps_the_other() {
        let mut b = InputBuffer::new();
        b.paste("a1\na2\na3");
        b.push(' ');
        b.paste("b1\nb2\nb3");
        b.backspace();
        assert_eq!(b.expanded(), "a1\na2\na3 ");
    }

    #[test]
    fn clear_and_set_drop_pastes() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        b.clear();
        assert_eq!(b.expanded(), "");
        b.paste("l1\nl2\nl3");
        b.set("fresh");
        assert_eq!(b.expanded(), "fresh");
    }

    #[test]
    fn kill_to_end_drops_swallowed_paste() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.paste("l1\nl2\nl3");
        b.insert_str("more");
        b.home();
        b.right();
        b.right();
        b.kill_to_end();
        assert_eq!(b.value, "ab");
        assert_eq!(b.expanded(), "ab");
        assert!(b.pastes.iter().all(String::is_empty));
    }

    #[test]
    fn kill_to_line_start_drops_swallowed_paste() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.paste("l1\nl2\nl3");
        b.insert_str("more");
        b.kill_to_line_start();
        assert_eq!(b.value, "");
        assert_eq!(b.expanded(), "");
        assert!(b.pastes.iter().all(String::is_empty));
    }

    #[test]
    fn submit_expands_compacted_paste() {
        let mut area = TextArea::with_max_height("p", 8);
        area.buffer.paste("l1\nl2\nl3");
        assert!(!area.is_empty());
        let effect = area.update(TextAreaMessage::Submit);
        match effect {
            Some(TextAreaEffect::Submit { content }) => {
                assert_eq!(content, "l1\nl2\nl3");
            }
            _ => panic!("expected submit effect"),
        }
        assert!(area.is_empty());
    }

    #[test]
    fn paste_message_routes_through_text_area_update() {
        let mut area = TextArea::with_max_height("p", 8);
        area.update(TextAreaMessage::Paste("a\nb\nc\nd".to_string()));
        assert_eq!(area.buffer.expanded(), "a\nb\nc\nd");
        assert_eq!(area.buffer.value.chars().count(), 1);
        area.update(TextAreaMessage::Paste("one line".to_string()));
        assert!(area.buffer.value.contains("one line"));
    }
}
