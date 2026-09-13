use ratatui::layout::{Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::add_provider::centered_rect;
use super::components::{InputBuffer, flatten_newlines};
use super::theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleMessage {
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Submit,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleEffect {
    /// Save the edited (trimmed, non-empty) title.
    Set {
        title: String,
    },
    Close,
}

pub struct TitlePopup {
    pub open: bool,
    pub buffer: InputBuffer,
}

impl TitlePopup {
    pub fn new() -> Self {
        Self {
            open: false,
            buffer: InputBuffer::new(),
        }
    }

    /// Open the editor prefilled with the session's current title (empty when
    /// the session has none).
    pub fn open(&mut self, current: Option<&str>) {
        self.open = true;
        self.buffer.set(current.unwrap_or(""));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.buffer.clear();
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<TitleMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('b') => Some(TitleMessage::Left),
                KeyCode::Char('f') => Some(TitleMessage::Right),
                KeyCode::Char('a') => Some(TitleMessage::Home),
                KeyCode::Char('e') => Some(TitleMessage::End),
                KeyCode::Char('d') => Some(TitleMessage::Delete),
                KeyCode::Char('h') => Some(TitleMessage::Backspace),
                _ => None,
            };
        }
        if alt(key) {
            return match key.code {
                KeyCode::Char('b') => Some(TitleMessage::LeftWord),
                KeyCode::Char('f') => Some(TitleMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Enter => Some(TitleMessage::Submit),
            KeyCode::Escape => Some(TitleMessage::Cancel),
            KeyCode::Backspace => Some(TitleMessage::Backspace),
            KeyCode::Delete => Some(TitleMessage::Delete),
            KeyCode::Left => Some(TitleMessage::Left),
            KeyCode::Right => Some(TitleMessage::Right),
            KeyCode::Home => Some(TitleMessage::Home),
            KeyCode::End => Some(TitleMessage::End),
            KeyCode::Char(c) => Some(TitleMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: TitleMessage) -> Option<TitleEffect> {
        if !self.open {
            return None;
        }
        match msg {
            TitleMessage::Input(c) => self.buffer.push(c),
            TitleMessage::Paste(text) => {
                self.buffer.insert_str(&flatten_newlines(&text));
            }
            TitleMessage::Backspace => self.buffer.backspace(),
            TitleMessage::Delete => self.buffer.delete(),
            TitleMessage::Left => self.buffer.left(),
            TitleMessage::Right => self.buffer.right(),
            TitleMessage::LeftWord => self.buffer.left_word(),
            TitleMessage::RightWord => self.buffer.right_word(),
            TitleMessage::Home => self.buffer.home(),
            TitleMessage::End => self.buffer.end(),
            TitleMessage::Submit => {
                let title = self.buffer.value.trim().to_string();
                self.close();
                if title.is_empty() {
                    return Some(TitleEffect::Close);
                }
                return Some(TitleEffect::Set { title });
            }
            TitleMessage::Cancel => {
                self.close();
                return Some(TitleEffect::Close);
            }
        }
        None
    }

    fn input_line(&self) -> Line<'static> {
        let text_style = Style::new().fg(theme::TEXT);
        let cursor_style = Style::new()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::REVERSED);
        let mut spans = Vec::new();
        let chars: Vec<char> = self.buffer.value.chars().collect();
        let cursor_idx = self.buffer.cursor_char_index();
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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(46, 18, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Session Title");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let [input_area, _spacer, help_area] =
            Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

        if self.buffer.value.is_empty() {
            frame.render_widget(
                Paragraph::new("Untitled session").fg(theme::TEXT_MUTED),
                input_area,
            );
        } else {
            frame.render_widget(Paragraph::new(self.input_line()), input_area);
        }

        frame.render_widget(
            Paragraph::new(theme::help_line(&[("Enter", "save"), ("Esc", "cancel")]))
                .fg(theme::TEXT_MUTED),
            help_area,
        );
    }
}

impl Default for TitlePopup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn open_prefills_with_current_title() {
        let mut popup = TitlePopup::new();
        popup.open(Some("Fix the login bug"));
        assert!(popup.open);
        assert_eq!(popup.buffer.value, "Fix the login bug");
        assert_eq!(popup.buffer.cursor, popup.buffer.value.len());

        popup.close();
        assert!(!popup.open);
        assert!(popup.buffer.value.is_empty());

        popup.open(None);
        assert!(popup.buffer.value.is_empty());
    }

    #[test]
    fn submit_sets_trimmed_title() {
        let mut popup = TitlePopup::new();
        popup.open(None);
        popup.update(TitleMessage::Input('h'));
        popup.update(TitleMessage::Input('i'));
        popup.update(TitleMessage::Input(' '));
        assert_eq!(
            popup.update(TitleMessage::Submit),
            Some(TitleEffect::Set { title: "hi".into() })
        );
        assert!(!popup.open, "submit closes the popup");
    }

    #[test]
    fn cancel_closes_without_setting() {
        let mut popup = TitlePopup::new();
        popup.open(None);
        popup.update(TitleMessage::Input('x'));
        assert_eq!(popup.update(TitleMessage::Cancel), Some(TitleEffect::Close));
        assert!(!popup.open);
    }

    #[test]
    fn empty_submit_closes_without_setting() {
        let mut popup = TitlePopup::new();
        popup.open(None);
        assert_eq!(popup.update(TitleMessage::Submit), Some(TitleEffect::Close));
        assert!(!popup.open);

        let mut popup = TitlePopup::new();
        popup.open(None);
        popup.update(TitleMessage::Input(' '));
        popup.update(TitleMessage::Input(' '));
        assert_eq!(
            popup.update(TitleMessage::Submit),
            Some(TitleEffect::Close),
            "whitespace-only title is not set"
        );
    }

    #[test]
    fn paste_flattens_newlines() {
        let mut popup = TitlePopup::new();
        popup.open(None);
        popup.update(TitleMessage::Paste("a\r\nb\nc".into()));
        assert_eq!(popup.buffer.value, "a b c");
    }

    #[test]
    fn map_event_routes_editing_keys() {
        let popup = TitlePopup::new();
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('x'), Modifiers::NONE)),
            Some(TitleMessage::Input('x'))
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('a'), Modifiers::CONTROL)),
            Some(TitleMessage::Home)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Char('f'), Modifiers::ALT)),
            Some(TitleMessage::RightWord)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Enter, Modifiers::NONE)),
            Some(TitleMessage::Submit)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Escape, Modifiers::NONE)),
            Some(TitleMessage::Cancel)
        );
    }

    #[test]
    fn updates_ignored_while_closed() {
        let mut popup = TitlePopup::new();
        assert_eq!(popup.update(TitleMessage::Input('x')), None);
        assert!(popup.buffer.value.is_empty());
    }
}
