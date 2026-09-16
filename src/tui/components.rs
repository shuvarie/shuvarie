use std::cell::Cell;
use std::ops::Range;

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
    /// Selection anchor in `value` byte coordinates; the head is the cursor.
    sel_anchor: Cell<Option<usize>>,
    pastes: Vec<String>,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
            scroll_offset: Cell::new(0),
            sel_anchor: Cell::new(None),
            pastes: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
        self.scroll_offset.set(0);
        self.sel_anchor.set(None);
        self.pastes.clear();
    }

    fn clear_anchor(&self) {
        self.sel_anchor.set(None);
    }

    /// Anchor a selection at the current cursor unless one is already
    /// active: the anchor is fixed, the cursor is the moving head.
    pub fn anchor_here(&self) {
        if self.sel_anchor.get().is_none() {
            self.sel_anchor.set(Some(self.cursor));
        }
    }

    /// Extend the selection so its head lands on byte `to`.
    pub fn select_to(&mut self, to: usize) {
        self.anchor_here();
        self.cursor = to.min(self.value.len());
    }

    /// Move the cursor to `byte`, collapsing any selection.
    pub fn place_cursor(&mut self, byte: usize) {
        self.cursor = byte.min(self.value.len());
        self.sel_anchor.set(None);
    }

    /// The selected byte range in `value`, `None` when empty or collapsed.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.sel_anchor.get()?;
        let (start, end) = if anchor <= self.cursor {
            (anchor, self.cursor)
        } else {
            (self.cursor, anchor)
        };
        (start < end).then_some(start..end)
    }

    /// The selected text with paste markers expanded back to their payload.
    pub fn selected_text(&self) -> Option<String> {
        let range = self.selection()?;
        let mut out = String::new();
        for c in self.value[range].chars() {
            match marker_id(c) {
                Some(id) => out.push_str(self.pastes.get(id).map(String::as_str).unwrap_or("")),
                None => out.push(c),
            }
        }
        Some(out)
    }

    /// Delete the active selection, leaving the cursor at its start.
    /// `false` when nothing is selected.
    pub fn delete_selection(&mut self) -> bool {
        let Some(range) = self.selection() else {
            return false;
        };
        self.cut_range(range.clone());
        self.cursor = range.start;
        self.clear_anchor();
        true
    }

    /// Byte index of the `(visual_row, col)` cell at the given wrap width,
    /// or `None` past the buffer's rows. A column past a row's width lands
    /// on the row's break byte.
    pub fn byte_at(&self, width: usize, visual_row: usize, col: usize) -> Option<usize> {
        let rows = self.rows(width);
        let row = rows.get(visual_row)?;
        Some(row_col_to_byte(row, col))
    }

    pub fn push(&mut self, c: char) {
        self.insert_char(c);
    }

    pub fn push_newline(&mut self) {
        self.insert_char('\n');
    }

    pub fn insert_char(&mut self, c: char) {
        self.delete_selection();
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn insert_str(&mut self, s: &str) {
        self.delete_selection();
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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
        if self.delete_selection() {
            return;
        }
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

    /// Whether the cursor sits on the first visual row.
    pub fn cursor_on_first_row(&self, width: usize) -> bool {
        self.cursor_row_col(width).0 == 0
    }

    /// Whether the cursor sits on the last visual row.
    pub fn cursor_on_last_row(&self, width: usize) -> bool {
        self.cursor_row_col(width).0 == self.rows(width).len() - 1
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.value = s.to_string();
        self.cursor = self.value.len();
        self.scroll_offset.set(0);
        self.sel_anchor.set(None);
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
        let selected_style = Style::new().fg(text_color).bg(theme::selection());
        let cursor_style = Style::new()
            .fg(cursor_color)
            .add_modifier(Modifier::REVERSED);
        let sel = self.selection();

        let mut lines: Vec<Line<'static>> = Vec::new();
        for r in rows.iter().skip(start).take(viewport.max(1)) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut found_cursor = false;
            for (c, bi) in r.chars.iter() {
                let is_cursor = *bi == self.cursor && !found_cursor;
                if is_cursor {
                    found_cursor = true;
                }
                let in_selection = sel.as_ref().is_some_and(|s| s.contains(bi));
                let style = if is_cursor {
                    cursor_style
                } else if in_selection {
                    selected_style
                } else if self.marker_id_at(*bi).is_some() {
                    paste_label_style()
                } else {
                    text_style
                };
                spans.push(Span::styled(c.to_string(), style));
            }
            if !found_cursor && self.cursor == r.end_byte {
                spans.push(Span::styled(" ".to_string(), cursor_style));
            } else if spans.is_empty() {
                let row_start = r.chars.first().map_or(r.end_byte, |(_, bi)| *bi);
                let row_selected = sel
                    .as_ref()
                    .is_some_and(|s| s.start <= r.end_byte && s.end > row_start);
                let style = if self.cursor == r.end_byte {
                    cursor_style
                } else if row_selected {
                    selected_style
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
        .fg(theme::accent())
        .bg(theme::accent_bg())
        .add_modifier(Modifier::BOLD)
}

fn input_block<'a>() -> Block<'a> {
    Block::new()
        .bg(theme::surface())
        .padding(ratatui::widgets::Padding::symmetric(2, 1))
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
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectHome,
    SelectEnd,
    MouseDown { column: u16, row: u16 },
    MouseDrag { column: u16, row: u16 },
    MouseUp,
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
    /// Last painted area, for mouse-position → byte mapping between frames.
    view_area: Cell<Rect>,
    /// Prompt drafts recalled with Up/Ctrl+P; most recent last.
    up_stack: Vec<String>,
    /// Drafts displaced by an up-recall, recallable with Down/Ctrl+N.
    down_stack: Vec<String>,
}

impl TextArea {
    #[allow(dead_code)]
    pub fn new(placeholder: &'static str) -> Self {
        Self {
            buffer: InputBuffer::new(),
            placeholder,
            max_height: 8,
            width: Cell::new(0),
            view_area: Cell::new(Rect::default()),
            up_stack: Vec::new(),
            down_stack: Vec::new(),
        }
    }

    pub fn with_max_height(placeholder: &'static str, max_height: u16) -> Self {
        Self {
            buffer: InputBuffer::new(),
            placeholder,
            max_height,
            width: Cell::new(0),
            view_area: Cell::new(Rect::default()),
            up_stack: Vec::new(),
            down_stack: Vec::new(),
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
            KeyCode::Left => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectLeft
                } else {
                    TextAreaMessage::Left
                };
                Some(msg)
            }
            KeyCode::Right => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectRight
                } else {
                    TextAreaMessage::Right
                };
                Some(msg)
            }
            KeyCode::Up => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectUp
                } else {
                    TextAreaMessage::CursorUp
                };
                Some(msg)
            }
            KeyCode::Down => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectDown
                } else {
                    TextAreaMessage::CursorDown
                };
                Some(msg)
            }
            KeyCode::Home => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectHome
                } else {
                    TextAreaMessage::Home
                };
                Some(msg)
            }
            KeyCode::End => {
                let msg = if key.modifiers.contains(Modifiers::SHIFT) {
                    TextAreaMessage::SelectEnd
                } else {
                    TextAreaMessage::End
                };
                Some(msg)
            }
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
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::Right => {
                self.buffer.right();
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::LeftWord => {
                self.buffer.left_word();
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::RightWord => {
                self.buffer.right_word();
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::Home => {
                self.buffer.home();
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::End => {
                self.buffer.end();
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::CursorUp => {
                if !self.recall_up() {
                    self.buffer.up(width);
                }
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::CursorDown => {
                if !self.recall_down() {
                    self.buffer.down(width);
                }
                self.buffer.clear_anchor();
                None
            }
            TextAreaMessage::SelectLeft => {
                self.buffer.anchor_here();
                self.buffer.left();
                None
            }
            TextAreaMessage::SelectRight => {
                self.buffer.anchor_here();
                self.buffer.right();
                None
            }
            TextAreaMessage::SelectUp => {
                self.buffer.anchor_here();
                self.buffer.up(width);
                None
            }
            TextAreaMessage::SelectDown => {
                self.buffer.anchor_here();
                self.buffer.down(width);
                None
            }
            TextAreaMessage::SelectHome => {
                self.buffer.anchor_here();
                self.buffer.home();
                None
            }
            TextAreaMessage::SelectEnd => {
                self.buffer.anchor_here();
                self.buffer.end();
                None
            }
            TextAreaMessage::MouseDown { column, row } => {
                if let Some(byte) = self.hit_byte(column, row) {
                    self.buffer.place_cursor(byte);
                }
                None
            }
            TextAreaMessage::MouseDrag { column, row } => {
                if let Some(byte) = self.hit_byte(column, row) {
                    self.buffer.select_to(byte);
                }
                None
            }
            TextAreaMessage::MouseUp => None,
            TextAreaMessage::Submit => {
                let content = self.buffer.expanded().trim().to_string();
                if content.is_empty() {
                    return None;
                }
                self.buffer.clear();
                Some(TextAreaEffect::Submit { content })
            }
            TextAreaMessage::Clear => {
                self.stash_draft();
                self.buffer.clear();
                None
            }
        }
    }

    fn buffer_width(&self) -> usize {
        self.width.get().max(1)
    }

    /// Byte index under a terminal cell, or `None` outside the input's
    /// content box. `column`/`row` are terminal-cell coordinates from a
    /// mouse event; the visual row accounts for the buffer's scroll.
    fn hit_byte(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.view_area.get();
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let inner = input_block().inner(area);
        if !inner.contains(ratatui::layout::Position::new(column, row)) {
            return None;
        }
        let width = usize::from(inner.width.max(1));
        let visual = usize::from(row - inner.y) + self.buffer.scroll_offset.get();
        let col = usize::from(column - inner.x);
        self.buffer.byte_at(width, visual, col)
    }

    /// Whether an up-recall (`Up`/`Ctrl+P`) should swap in a stashed draft:
    /// the up stack holds something and the cursor sits on the first row.
    pub fn wants_recall_up(&self) -> bool {
        !self.up_stack.is_empty() && self.buffer.cursor_on_first_row(self.buffer_width())
    }

    /// Mirror of [`Self::wants_recall_up`] for the down stack.
    pub fn wants_recall_down(&self) -> bool {
        !self.down_stack.is_empty() && self.buffer.cursor_on_last_row(self.buffer_width())
    }

    /// Push non-empty text onto a stack, skipping a duplicate of its top.
    fn push_stack(stack: &mut Vec<String>, text: String) {
        if !text.is_empty() && stack.last() != Some(&text) {
            stack.push(text);
        }
    }

    /// Stash the current draft onto the up stack (a displaced draft stays
    /// recallable with `Up`); empty and duplicate drafts are skipped.
    pub fn stash_draft(&mut self) {
        let current = self.buffer.expanded();
        Self::push_stack(&mut self.up_stack, current);
    }

    /// Record a sent prompt on the up stack and reset the down stack.
    pub fn remember_sent(&mut self, content: &str) {
        Self::push_stack(&mut self.up_stack, content.to_string());
        self.down_stack.clear();
    }

    /// Swap the current text onto the down stack and recall the previous
    /// draft; `false` when the recall gate ([`Self::wants_recall_up`]) fails.
    fn recall_up(&mut self) -> bool {
        if !self.wants_recall_up() {
            return false;
        }
        let next = self.up_stack.pop().expect("non-empty up stack");
        let current = self.buffer.expanded();
        Self::push_stack(&mut self.down_stack, current);
        self.buffer.set(&next);
        true
    }

    /// Mirror of [`Self::recall_up`] walking back down the down stack.
    fn recall_down(&mut self) -> bool {
        if !self.wants_recall_down() {
            return false;
        }
        let next = self.down_stack.pop().expect("non-empty down stack");
        let current = self.buffer.expanded();
        Self::push_stack(&mut self.up_stack, current);
        self.buffer.set(&next);
        true
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
        self.view_area.set(area);
        let block = input_block();
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width as usize;
        self.width.set(width);
        let viewport = inner.height as usize;
        self.buffer.ensure_cursor_visible(width, viewport);

        if self.buffer.value.is_empty() {
            let mut placeholder_line = self
                .buffer
                .cursor_line(crate::tui::theme::text_muted(), crate::tui::theme::accent());
            let ph = format!(" {}", self.placeholder);
            placeholder_line.push_span(Span::raw(ph).fg(crate::tui::theme::text_muted()));
            frame.render_widget(
                Paragraph::new(placeholder_line).alignment(Alignment::Left),
                inner,
            );
        } else {
            let lines =
                self.buffer
                    .cursor_lines(text_color, crate::tui::theme::accent(), width, viewport);
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
        for text_color in [crate::tui::theme::text(), crate::tui::theme::accent()] {
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
        let lines = b.cursor_lines(
            crate::tui::theme::text(),
            crate::tui::theme::accent(),
            60,
            8,
        );
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
            .collect();
        assert!(text.contains("[pasted 3 lines]"), "text={text}");
        assert!(!text.contains("l1"), "content must stay hidden: {text}");
        let chip = lines[0]
            .spans
            .iter()
            .find(|s| s.style.bg == Some(crate::tui::theme::accent_bg()))
            .expect("chip background");
        assert_eq!(chip.style.fg, Some(crate::tui::theme::accent()));
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

    #[test]
    fn sent_history_round_trips_through_up_and_down() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.remember_sent("one");
        area.remember_sent("two");
        area.buffer.set("draft");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "two");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "one");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "one", "empty up stack moves the cursor");
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.value, "two");
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.value, "draft");
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.value, "draft", "empty down stack is a no-op");
    }

    #[test]
    fn recall_requires_cursor_on_boundary_row() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.remember_sent("old");
        area.buffer.set("one\ntwo");
        assert!(!area.wants_recall_up(), "cursor on the last row");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(
            area.buffer.value, "one\ntwo",
            "cursor moved instead of recalling"
        );
        assert!(!area.wants_recall_down(), "down stack is empty");
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.value, "one\ntwo");
        area.buffer.up(40);
        assert!(area.wants_recall_up());
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "old");
    }

    #[test]
    fn empty_stack_leaves_cursor_movement_untouched() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.buffer.set("hello\nworld");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.cursor, "hello".len());
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.cursor, area.buffer.value.len());
    }

    #[test]
    fn stashes_skip_duplicate_of_stack_top() {
        let mut area = TextArea::with_max_height("p", 8);
        area.remember_sent("same");
        area.buffer.set("same");
        area.stash_draft();
        assert_eq!(area.up_stack, vec!["same".to_string()]);
        area.remember_sent("");
        assert_eq!(area.up_stack, vec!["same".to_string()]);
    }

    #[test]
    fn remember_sent_clears_down_stack() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.buffer.set("draft");
        area.remember_sent("sent");
        area.buffer.set("chip");
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "sent");
        assert_eq!(area.down_stack, vec!["chip".to_string()]);
        area.remember_sent("newer");
        assert!(area.down_stack.is_empty(), "a fresh send resets the walk");
    }

    #[test]
    fn clear_stashes_draft_into_up_stack() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.buffer.set("draft");
        area.update(TextAreaMessage::Clear);
        assert!(area.is_empty());
        assert_eq!(area.up_stack, vec!["draft".to_string()]);
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "draft");
    }

    #[test]
    fn recall_expands_paste_chips() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.remember_sent("previous");
        area.buffer.set("ab");
        area.buffer.paste("l1\nl2\nl3");
        area.buffer.home();
        area.update(TextAreaMessage::CursorUp);
        assert_eq!(area.buffer.value, "previous");
        area.update(TextAreaMessage::CursorDown);
        assert_eq!(area.buffer.expanded(), "abl1\nl2\nl3");
    }

    fn select(b: &mut InputBuffer, start: usize, end: usize) {
        b.place_cursor(start);
        b.anchor_here();
        b.select_to(end);
    }

    #[test]
    fn anchor_and_extend_selects_and_copies() {
        let mut b = InputBuffer::new();
        b.set("hello");
        select(&mut b, 2, 5);
        assert_eq!(b.selection(), Some(2..5));
        assert_eq!(b.selected_text().as_deref(), Some("llo"));
        select(&mut b, 5, 2);
        assert_eq!(b.selection(), Some(2..5), "reversed drag normalizes");
        assert_eq!(b.selected_text().as_deref(), Some("llo"));
    }

    #[test]
    fn plain_move_collapses_selection() {
        let mut area = TextArea::with_max_height("p", 8);
        area.width.set(40);
        area.buffer.set("hello");
        select(&mut area.buffer, 0, 3);
        assert_eq!(area.buffer.selection(), Some(0..3));
        area.update(TextAreaMessage::Right);
        assert_eq!(
            area.buffer.selection(),
            None,
            "plain motion collapses to the head"
        );
        assert_eq!(area.buffer.cursor, 4);
    }

    #[test]
    fn typing_replaces_selection() {
        let mut b = InputBuffer::new();
        b.set("hello");
        select(&mut b, 2, 5);
        b.push('E');
        assert_eq!(b.value, "heE");
        assert_eq!(b.cursor, "heE".len());
        assert_eq!(b.selection(), None);
    }

    #[test]
    fn paste_replaces_selection() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        select(&mut b, 0, 5);
        b.paste("X\nY");
        assert_eq!(b.expanded(), "X\nY world");
        assert_eq!(b.cursor, "X\nY".len());
    }

    #[test]
    fn backspace_deletes_whole_selection() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        select(&mut b, 0, 6);
        b.backspace();
        assert_eq!(b.value, "world");
        assert_eq!(b.cursor, 0);
        assert_eq!(b.selection(), None);
    }

    #[test]
    fn forward_delete_deletes_whole_selection() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        select(&mut b, 0, 6);
        b.delete();
        assert_eq!(b.value, "world");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn kill_keys_delete_selection_first() {
        let mut b = InputBuffer::new();
        b.set("alpha beta");
        select(&mut b, 0, 5);
        b.kill_to_end();
        assert_eq!(b.value, " beta");
        let mut b = InputBuffer::new();
        b.set("alpha beta");
        select(&mut b, 0, 5);
        b.kill_to_line_start();
        assert_eq!(b.value, " beta");
        assert_eq!(b.cursor, 0);
    }

    #[test]
    fn mouse_drag_selects_within_view() {
        let mut area = TextArea::with_max_height("p", 8);
        area.buffer.set("hello world");
        area.width.set(16);
        area.view_area.set(Rect::new(0, 0, 20, 4));
        area.update(TextAreaMessage::MouseDown { column: 2, row: 1 });
        assert_eq!(area.buffer.cursor, 0);
        area.update(TextAreaMessage::MouseDrag { column: 7, row: 1 });
        assert_eq!(area.buffer.selection(), Some(0..5));
        assert_eq!(area.buffer.selected_text().as_deref(), Some("hello"));
        area.update(TextAreaMessage::MouseUp);
        assert_eq!(area.buffer.selection(), Some(0..5), "up keeps the drag");
        area.update(TextAreaMessage::MouseDown { column: 13, row: 1 });
        assert_eq!(area.buffer.selection(), None, "down collapses");
        assert_eq!(area.buffer.cursor, "hello world".len());
    }

    #[test]
    fn mouse_click_outside_text_is_ignored() {
        let mut area = TextArea::with_max_height("p", 8);
        area.buffer.set("hello");
        area.width.set(16);
        area.view_area.set(Rect::new(0, 0, 20, 4));
        area.update(TextAreaMessage::MouseDown { column: 0, row: 1 });
        assert_eq!(area.buffer.cursor, "hello".len(), "padding column ignored");
        area.update(TextAreaMessage::MouseDrag { column: 2, row: 3 });
        assert_eq!(area.buffer.cursor, "hello".len(), "padding row ignored");
        area.view_area.set(Rect::default());
        area.update(TextAreaMessage::MouseDown { column: 2, row: 1 });
        assert_eq!(area.buffer.cursor, "hello".len(), "unpainted area is inert");
    }

    #[test]
    fn selection_expands_paste_marker_on_copy() {
        let mut b = InputBuffer::new();
        b.set("ab");
        b.paste("l1\nl2\nl3");
        select(&mut b, 0, 1);
        assert_eq!(b.selected_text().as_deref(), Some("a"));
        let len = b.value.len();
        select(&mut b, 0, len);
        assert_eq!(b.selected_text().as_deref(), Some("abl1\nl2\nl3"));
    }

    #[test]
    fn deleting_selection_drops_swallowed_paste() {
        let mut b = InputBuffer::new();
        b.paste("l1\nl2\nl3");
        b.insert_str("ok");
        select(&mut b, 0, 3);
        b.delete();
        assert_eq!(b.value, "ok");
        assert_eq!(b.expanded(), "ok");
        assert!(b.pastes.iter().all(String::is_empty));
    }

    #[test]
    fn cursor_lines_highlight_selection_cells() {
        let mut b = InputBuffer::new();
        b.set("hello");
        select(&mut b, 0, 3);
        let lines = b.cursor_lines(theme::text(), theme::accent(), 20, 8);
        let row = &lines[0];
        let highlighted: String = row
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(theme::selection()))
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(highlighted, "hel");
        assert_eq!(
            row.spans[3].style.add_modifier,
            Modifier::REVERSED,
            "the head cell keeps the cursor"
        );
    }

    #[test]
    fn cursor_lines_highlight_empty_row_break() {
        let mut b = InputBuffer::new();
        b.set("a\n\nb");
        select(&mut b, 0, 3);
        assert_eq!(b.selection(), Some(0..3));
        let lines = b.cursor_lines(theme::text(), theme::accent(), 20, 8);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[1].spans[0].style.bg, Some(theme::selection()));
        assert_eq!(lines[2].spans[0].style.bg, None, "beyond the break");
    }

    #[test]
    fn selection_covers_logical_breaks() {
        let mut b = InputBuffer::new();
        b.set("first line\nsecond line");
        let len = b.value.len();
        select(&mut b, 0, len);
        assert_eq!(
            b.selected_text().as_deref(),
            Some("first line\nsecond line")
        );
    }

    #[test]
    fn select_up_extends_visual_column() {
        let mut b = InputBuffer::new();
        b.set("one\ntwo");
        b.end();
        b.anchor_here();
        b.up(20);
        assert_eq!(b.selection(), Some(3..7));
        assert_eq!(b.selected_text().as_deref(), Some("\ntwo"));
    }

    #[test]
    fn selection_resets_on_set_and_clear() {
        let mut b = InputBuffer::new();
        b.set("hello");
        select(&mut b, 0, 3);
        b.set("fresh");
        assert_eq!(b.selection(), None);
        select(&mut b, 0, 3);
        b.clear();
        assert_eq!(b.selection(), None);
    }
}
