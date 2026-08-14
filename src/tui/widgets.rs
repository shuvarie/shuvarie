use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

pub struct InputBuffer {
    pub value: String,
    pub cursor: usize,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self {
            value: String::new(),
            cursor: 0,
        }
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    pub fn push(&mut self, c: char) {
        self.value.insert(self.cursor, c);
        self.cursor += c.len_utf8();
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

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.len();
    }

    pub fn delete(&mut self) {
        if self.cursor < self.value.len() {
            let next = self.value[self.cursor..].chars().next().unwrap();
            let next_len = next.len_utf8();
            self.value
                .replace_range(self.cursor..self.cursor + next_len, "");
        }
    }

    pub fn kill_to_end(&mut self) {
        self.value.truncate(self.cursor);
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

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set(&mut self, s: &str) {
        self.value = s.to_string();
        self.cursor = self.value.len();
    }

    pub fn cursor_char_index(&self) -> usize {
        self.value[..self.cursor].chars().count()
    }

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
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Submit,
}

pub enum TextAreaEffect {
    Submit { content: String },
}

pub struct TextArea {
    pub buffer: InputBuffer,
    pub placeholder: &'static str,
}

impl TextArea {
    pub fn new(placeholder: &'static str) -> Self {
        Self {
            buffer: InputBuffer::new(),
            placeholder,
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
            KeyCode::Enter => Some(TextAreaMessage::Submit),
            KeyCode::Backspace => Some(TextAreaMessage::Backspace),
            KeyCode::Left => Some(TextAreaMessage::Left),
            KeyCode::Right => Some(TextAreaMessage::Right),
            KeyCode::Home => Some(TextAreaMessage::Home),
            KeyCode::End => Some(TextAreaMessage::End),
            KeyCode::Char(c) => Some(TextAreaMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: TextAreaMessage) -> Option<TextAreaEffect> {
        match msg {
            TextAreaMessage::Input(c) => {
                self.buffer.push(c);
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
            TextAreaMessage::Submit => {
                let content = self.buffer.value.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                self.buffer.clear();
                Some(TextAreaEffect::Submit { content })
            }
        }
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
            .padding(ratatui::widgets::Padding::horizontal(2));
        let inner = block.inner(area);
        frame.render_widget(block, area);

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
            let line = self
                .buffer
                .cursor_line(crate::tui::theme::TEXT, crate::tui::theme::ACCENT);
            frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), inner);
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
    fn kill_to_end_removes_from_cursor() {
        let mut b = InputBuffer::new();
        b.set("hello world");
        b.left();
        b.left();
        b.left();
        b.left();
        b.left();
        b.kill_to_end();
        assert_eq!(b.value, "hello ");
        assert_eq!(b.cursor, "hello ".len());
    }

    #[test]
    fn kill_to_end_at_start_clears_all() {
        let mut b = InputBuffer::new();
        b.set("abc");
        b.home();
        b.kill_to_end();
        assert_eq!(b.value, "");
        assert_eq!(b.cursor, 0);
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
