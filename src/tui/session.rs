use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use shuvarie_core::{Config, Role};
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::sidebar;
use super::theme;
use super::widgets::InputBuffer;

pub enum SessionMessage {
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
    ScrollUp,
    ScrollDown,
    MessageReceived { content: String },
    ReplyError { error: String },
    Reset,
}

pub struct SessionScreen {
    pub input: InputBuffer,
    pub messages: Vec<(Role, String)>,
    pub scroll: usize,
    pub status: Option<String>,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: InputBuffer::new(),
            messages: Vec::new(),
            scroll: 0,
            status: None,
        }
    }

    pub fn handle_event(&self, key: KeyEvent) -> Option<SessionMessage> {
        if ctrl(&key) {
            return match key.code {
                KeyCode::Char('b') => Some(SessionMessage::Left),
                KeyCode::Char('f') => Some(SessionMessage::Right),
                KeyCode::Char('a') => Some(SessionMessage::Home),
                KeyCode::Char('e') => Some(SessionMessage::End),
                KeyCode::Char('d') => Some(SessionMessage::Delete),
                KeyCode::Char('h') => Some(SessionMessage::Backspace),
                KeyCode::Char('k') => Some(SessionMessage::KillToEnd),
                KeyCode::Char('n') => Some(SessionMessage::ScrollDown),
                KeyCode::Char('p') => Some(SessionMessage::ScrollUp),
                _ => None,
            };
        }
        if alt(&key) {
            return match key.code {
                KeyCode::Char('b') => Some(SessionMessage::LeftWord),
                KeyCode::Char('f') => Some(SessionMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Enter => Some(SessionMessage::Submit),
            KeyCode::Backspace => Some(SessionMessage::Backspace),
            KeyCode::Left => Some(SessionMessage::Left),
            KeyCode::Right => Some(SessionMessage::Right),
            KeyCode::Home => Some(SessionMessage::Home),
            KeyCode::End => Some(SessionMessage::End),
            KeyCode::Up => Some(SessionMessage::ScrollUp),
            KeyCode::Down => Some(SessionMessage::ScrollDown),
            KeyCode::Char(c) => Some(SessionMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: SessionMessage) -> Option<SessionEffect> {
        match msg {
            SessionMessage::Input(c) => {
                self.input.push(c);
                None
            }
            SessionMessage::Backspace => {
                self.input.backspace();
                None
            }
            SessionMessage::Delete => {
                self.input.delete();
                None
            }
            SessionMessage::KillToEnd => {
                self.input.kill_to_end();
                None
            }
            SessionMessage::Left => {
                self.input.left();
                None
            }
            SessionMessage::Right => {
                self.input.right();
                None
            }
            SessionMessage::LeftWord => {
                self.input.left_word();
                None
            }
            SessionMessage::RightWord => {
                self.input.right_word();
                None
            }
            SessionMessage::Home => {
                self.input.home();
                None
            }
            SessionMessage::End => {
                self.input.end();
                None
            }
            SessionMessage::ScrollUp => {
                self.scroll = self.scroll.saturating_add(1);
                None
            }
            SessionMessage::ScrollDown => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            SessionMessage::Submit => {
                let content = self.input.value.trim().to_string();
                if content.is_empty() {
                    return None;
                }
                self.input.clear();
                self.messages.push((Role::User, content.clone()));
                self.status = Some("thinking…".to_string());
                Some(SessionEffect::SendMessage { content })
            }
            SessionMessage::MessageReceived { content } => {
                self.messages.push((Role::Assistant, content));
                self.status = None;
                None
            }
            SessionMessage::ReplyError { error } => {
                self.status = Some(format!("error: {error}"));
                None
            }
            SessionMessage::Reset => {
                self.messages.clear();
                self.status = None;
                None
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, config: &Config) {
        let [sidebar_area, content_area] = Layout::horizontal([Length(30), Min(0)])
            .spacing(1)
            .areas(area);

        sidebar::render(
            frame,
            sidebar_area,
            sidebar::SidebarData {
                version: env!("CARGO_PKG_VERSION"),
                tokens: 0,
                cost: 0.0,
                provider: config.active_provider.as_deref(),
                model: config.active_model.as_deref(),
            },
        );

        let title = format!(
            "Shuvarie · {}:{}",
            config.active_provider.as_deref().unwrap_or("?"),
            config.active_model.as_deref().unwrap_or("?")
        );
        let [title_area, history_area, input_area, status_area] =
            Layout::vertical([Length(1), Min(0), Length(3), Length(1)]).areas(content_area);

        frame.render_widget(
            Paragraph::new(theme::title_header(&title))
                .bg(theme::SURFACE)
                .alignment(Alignment::Center),
            title_area,
        );

        let history_block = Block::new().padding(Padding::horizontal(2));
        let history_inner = history_block.inner(history_area);
        frame.render_widget(history_block, history_area);

        let lines: Vec<Line> = self
            .messages
            .iter()
            .flat_map(|(role, content)| {
                let (label, label_fg) = match role {
                    Role::User => ("You", theme::ACCENT),
                    Role::Assistant => ("Assistant", theme::TEXT),
                    Role::System => ("System", theme::TEXT_MUTED),
                };
                let mut out = vec![Line::from(Span::raw(label.to_string()).fg(label_fg).bold())];
                for text_line in content.lines() {
                    out.push(Line::from(Span::raw(text_line.to_string()).fg(theme::TEXT)));
                }
                out.push(Line::from(""));
                out
            })
            .collect();
        let history = Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .scroll((self.scroll as u16, 0));
        frame.render_widget(history, history_inner);

        let input_block = Block::new()
            .bg(theme::SURFACE)
            .padding(Padding::horizontal(2));
        let input_inner = input_block.inner(input_area);
        frame.render_widget(input_block, input_area);
        if self.input.value.is_empty() {
            let mut placeholder = self.input.cursor_line(theme::TEXT_MUTED, theme::ACCENT);
            placeholder.push_span(Span::raw(" Type a message…").fg(theme::TEXT_MUTED));
            frame.render_widget(
                Paragraph::new(placeholder).alignment(Alignment::Left),
                input_inner,
            );
        } else {
            let line = self.input.cursor_line(theme::TEXT, theme::ACCENT);
            frame.render_widget(Paragraph::new(line).alignment(Alignment::Left), input_inner);
        }

        if let Some(status) = &self.status {
            frame.render_widget(
                Paragraph::new(status.as_str()).fg(theme::TEXT_MUTED),
                status_area,
            );
        }
    }
}

pub enum SessionEffect {
    SendMessage { content: String },
}

impl Default for SessionScreen {
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
