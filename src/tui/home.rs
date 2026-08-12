use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Paragraph};
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::logo;
use super::theme;
use super::widgets::InputBuffer;

pub enum HomeMessage {
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

pub enum HomeEffect {
    Submit { content: String },
}

pub struct HomeScreen {
    pub input: InputBuffer,
}

impl HomeScreen {
    pub fn new() -> Self {
        Self {
            input: InputBuffer::new(),
        }
    }

    pub fn handle_event(&self, key: KeyEvent) -> Option<HomeMessage> {
        if ctrl(&key) {
            return match key.code {
                KeyCode::Char('b') => Some(HomeMessage::Left),
                KeyCode::Char('f') => Some(HomeMessage::Right),
                KeyCode::Char('a') => Some(HomeMessage::Home),
                KeyCode::Char('e') => Some(HomeMessage::End),
                KeyCode::Char('d') => Some(HomeMessage::Delete),
                KeyCode::Char('h') => Some(HomeMessage::Backspace),
                KeyCode::Char('k') => Some(HomeMessage::KillToEnd),
                _ => None,
            };
        }
        if alt(&key) {
            return match key.code {
                KeyCode::Char('b') => Some(HomeMessage::LeftWord),
                KeyCode::Char('f') => Some(HomeMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Enter => Some(HomeMessage::Submit),
            KeyCode::Backspace => Some(HomeMessage::Backspace),
            KeyCode::Left => Some(HomeMessage::Left),
            KeyCode::Right => Some(HomeMessage::Right),
            KeyCode::Home => Some(HomeMessage::Home),
            KeyCode::End => Some(HomeMessage::End),
            KeyCode::Char(c) => Some(HomeMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: HomeMessage) -> Option<HomeEffect> {
        match msg {
            HomeMessage::Input(c) => {
                self.input.push(c);
                None
            }
            HomeMessage::Backspace => {
                self.input.backspace();
                None
            }
            HomeMessage::Delete => {
                self.input.delete();
                None
            }
            HomeMessage::KillToEnd => {
                self.input.kill_to_end();
                None
            }
            HomeMessage::Left => {
                self.input.left();
                None
            }
            HomeMessage::Right => {
                self.input.right();
                None
            }
            HomeMessage::LeftWord => {
                self.input.left_word();
                None
            }
            HomeMessage::RightWord => {
                self.input.right_word();
                None
            }
            HomeMessage::Home => {
                self.input.home();
                None
            }
            HomeMessage::End => {
                self.input.end();
                None
            }
            HomeMessage::Submit => {
                let content = self.input.value.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                self.input.clear();
                Some(HomeEffect::Submit { content })
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let [top_spacer, logo_area, gap, input_area, bottom_spacer] = Layout::vertical([
            Min(0),
            Length(logo::height() as u16),
            Length(1),
            Length(3),
            Min(0),
        ])
        .areas(area);

        let _ = top_spacer;
        logo::render(frame, logo_area);
        let _ = gap;

        let input_block = Block::new()
            .bg(theme::SURFACE)
            .padding(ratatui::widgets::Padding::horizontal(2));
        let input_inner = input_block.inner(input_area);
        frame.render_widget(input_block, input_area);

        if self.input.value.is_empty() {
            let mut placeholder = self.input.cursor_line(theme::TEXT_MUTED, theme::ACCENT);
            placeholder.push_span(Span::raw(" Ask anything…").fg(theme::TEXT_MUTED));
            frame.render_widget(
                Paragraph::new(placeholder).alignment(Alignment::Left),
                input_inner,
            );
        } else {
            let line = self.input.cursor_line(theme::TEXT, theme::ACCENT);
            frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), input_inner);
        }

        let _ = bottom_spacer;
    }
}

impl Default for HomeScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::ALT)
}
