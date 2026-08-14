use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use shuvarie_core::Role;
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::sidebar::{Sidebar, SidebarMessage};
use super::theme;
use super::widgets::{TextArea, TextAreaEffect, TextAreaMessage};

pub enum SessionMessage {
    Text(TextAreaMessage),
    ScrollUp,
    ScrollDown,
    TokenReceived {
        content: String,
    },
    StreamDone,
    StreamError {
        error: String,
    },
    StreamCancelled,
    CancelRequested,
    UsageUpdate {
        usage: TokenUsage,
        cost: f64,
    },
    Reset,
    UpdateConfig {
        provider: Option<String>,
        model: Option<String>,
        context_length: Option<u64>,
    },
}

pub struct SessionScreen {
    pub input: TextArea,
    pub messages: Vec<(Role, String)>,
    pub streaming: bool,
    pub pending: String,
    pub scroll: usize,
    pub status: Option<String>,
    pub sidebar: Sidebar,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: TextArea::new("Type a message…"),
            messages: Vec::new(),
            streaming: false,
            pending: String::new(),
            scroll: 0,
            status: None,
            sidebar: Sidebar::new(),
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SessionMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(SessionMessage::ScrollDown),
                KeyCode::Char('p') => Some(SessionMessage::ScrollUp),
                _ => self.input.map_event(key).map(SessionMessage::Text),
            };
        }
        if alt(key) {
            return self.input.map_event(key).map(SessionMessage::Text);
        }
        match key.code {
            KeyCode::Up => Some(SessionMessage::ScrollUp),
            KeyCode::Down => Some(SessionMessage::ScrollDown),
            _ => self.input.map_event(key).map(SessionMessage::Text),
        }
    }

    pub fn update(&mut self, msg: SessionMessage) -> Option<SessionEffect> {
        match msg {
            SessionMessage::Text(m) => {
                if let Some(effect) = self.input.update(m) {
                    match effect {
                        TextAreaEffect::Submit { content } => {
                            self.messages.push((Role::User, content.clone()));
                            self.status = Some("thinking…".to_string());
                            return Some(SessionEffect::SendMessage { content });
                        }
                    }
                }
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
            SessionMessage::TokenReceived { content } => {
                if !self.streaming {
                    self.streaming = true;
                    self.pending.clear();
                }
                self.pending.push_str(&content);
                self.status = Some("streaming…".to_string());
                None
            }
            SessionMessage::StreamDone => {
                if self.streaming {
                    self.messages
                        .push((Role::Assistant, std::mem::take(&mut self.pending)));
                    self.streaming = false;
                }
                self.status = None;
                None
            }
            SessionMessage::StreamError { error } => {
                self.streaming = false;
                self.pending.clear();
                self.status = Some(format!("error: {error}"));
                None
            }
            SessionMessage::StreamCancelled => {
                self.streaming = false;
                self.pending.clear();
                self.status = None;
                None
            }
            SessionMessage::CancelRequested => {
                if self.streaming {
                    return Some(SessionEffect::CancelStream);
                }
                None
            }
            SessionMessage::UsageUpdate { usage, cost } => {
                self.sidebar
                    .update(SidebarMessage::UpdateUsage { usage, cost });
                None
            }
            SessionMessage::Reset => {
                self.messages.clear();
                self.streaming = false;
                self.pending.clear();
                self.status = None;
                None
            }
            SessionMessage::UpdateConfig {
                provider,
                model,
                context_length,
            } => {
                self.sidebar.update(SidebarMessage::UpdateConfig {
                    provider,
                    model,
                    context_length,
                });
                None
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let [sidebar_area, content_area] = Layout::horizontal([Length(30), Min(0)])
            .spacing(1)
            .areas(area);

        self.sidebar.view(frame, sidebar_area);

        let title = format!(
            "Shuvarie · {}:{}",
            self.sidebar.provider.as_deref().unwrap_or("?"),
            self.sidebar.model.as_deref().unwrap_or("?")
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

        let mut rendered_messages: Vec<(Role, String)> = self.messages.clone();
        if self.streaming {
            rendered_messages.push((Role::Assistant, self.pending.clone()));
        }
        let lines: Vec<Line> = rendered_messages
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

        self.input.view(frame, input_area);

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
    CancelStream,
}

impl Default for SessionScreen {
    fn default() -> Self {
        Self::new()
    }
}
