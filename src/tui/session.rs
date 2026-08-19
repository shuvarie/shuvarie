use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Constraint::*, Layout, Rect, Size};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use shuvarie_core::Role;
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

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
    ShowError {
        error: String,
    },
    ClearError,
    UsageUpdate {
        usage: TokenUsage,
        cost: f64,
    },
    Reset,
    Loaded {
        id: u64,
        title: String,
        session: shuvarie_core::Session,
    },
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
    pub scroll_state: RefCell<ScrollViewState>,
    scroll_view: RefCell<ScrollView>,
    scroll_dirty: Cell<bool>,
    scroll_width: Cell<u16>,
    pub status: Option<String>,
    pub sidebar: Sidebar,
    pub session_id: Option<u64>,
    pub session_title: Option<String>,
    pub error: Option<String>,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: TextArea::new("Type a message…"),
            messages: Vec::new(),
            streaming: false,
            pending: String::new(),
            scroll_state: RefCell::new(ScrollViewState::default()),
            scroll_view: RefCell::new(ScrollView::new(Size::new(0, 0))),
            scroll_dirty: Cell::new(false),
            scroll_width: Cell::new(0),
            status: None,
            sidebar: Sidebar::new(),
            session_id: None,
            session_title: None,
            error: None,
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
                            self.mark_scroll_dirty();
                            self.follow_bottom();
                            return Some(SessionEffect::SendMessage { content });
                        }
                    }
                }
                None
            }
            SessionMessage::ScrollUp => {
                self.scroll_state.get_mut().scroll_up();
                None
            }
            SessionMessage::ScrollDown => {
                self.scroll_state.get_mut().scroll_down();
                None
            }
            SessionMessage::TokenReceived { content } => {
                if !self.streaming {
                    self.streaming = true;
                    self.pending.clear();
                }
                self.pending.push_str(&content);
                self.status = Some("streaming…".to_string());
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::StreamDone => {
                if self.streaming {
                    self.messages
                        .push((Role::Assistant, std::mem::take(&mut self.pending)));
                    self.streaming = false;
                }
                self.status = None;
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::StreamError { error } => {
                self.streaming = false;
                self.pending.clear();
                self.status = Some(format!("error: {error}"));
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::StreamCancelled => {
                self.streaming = false;
                self.pending.clear();
                self.status = None;
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::CancelRequested => {
                if self.streaming {
                    return Some(SessionEffect::CancelStream);
                }
                None
            }
            SessionMessage::ShowError { error } => {
                self.error = Some(error);
                None
            }
            SessionMessage::ClearError => {
                self.error = None;
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
                self.session_id = None;
                self.session_title = None;
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage::default(),
                    cost: 0.0,
                });
                *self.scroll_state.get_mut() = ScrollViewState::default();
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::Loaded { id, title, session } => {
                self.messages = session
                    .messages
                    .into_iter()
                    .map(|m| (m.role, m.content))
                    .collect();
                self.streaming = false;
                self.pending.clear();
                self.status = None;
                self.session_id = Some(id);
                self.session_title = Some(title);
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage {
                        input_tokens: session.input_tokens,
                        output_tokens: session.output_tokens,
                        total_tokens: session.tokens,
                        cached_input_tokens: session.cached_tokens,
                        reasoning_tokens: session.reasoning_tokens,
                    },
                    cost: session.cost,
                });
                *self.scroll_state.get_mut() = ScrollViewState::default();
                self.mark_scroll_dirty();
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

        let title = match &self.session_title {
            Some(t) if !t.is_empty() => format!("Shuvarie · {t}"),
            _ => format!(
                "Shuvarie · {}:{}",
                self.sidebar.provider.as_deref().unwrap_or("?"),
                self.sidebar.model.as_deref().unwrap_or("?")
            ),
        };
        let [
            title_area,
            history_area,
            input_area,
            status_area,
            footer_area,
        ] = Layout::vertical([Length(1), Min(0), Length(3), Length(1), Length(1)])
            .areas(content_area);

        frame.render_widget(
            Paragraph::new(theme::title_header(&title))
                .bg(theme::SURFACE)
                .alignment(Alignment::Center),
            title_area,
        );

        let history_block = Block::new().padding(Padding::horizontal(2));
        let history_inner = history_block.inner(history_area);
        frame.render_widget(history_block, history_area);

        let content_width = history_inner.width.saturating_sub(1);
        if self.scroll_width.get() != content_width {
            self.scroll_dirty.set(true);
            self.scroll_width.set(content_width);
        }
        if self.scroll_dirty.replace(false) {
            let mut rendered_messages: Vec<(Role, String)> = self.messages.clone();
            if self.streaming {
                rendered_messages.push((Role::Assistant, self.pending.clone()));
            }
            self.rebuild_scroll_view(&rendered_messages, content_width);
        }

        let mut scroll_state = self.scroll_state.borrow_mut();
        let scroll_view = self.scroll_view.borrow();
        frame.render_stateful_widget(&*scroll_view, history_inner, &mut scroll_state);

        let content_height = scroll_view.size().height;
        if content_height > history_inner.height {
            self.render_scrollbar(frame, history_inner, &scroll_state, content_height);
        }

        self.input.view(frame, input_area);

        if let Some(status) = &self.status {
            frame.render_widget(
                Paragraph::new(status.as_str()).fg(theme::TEXT_MUTED),
                status_area,
            );
        }

        if let Some(error) = &self.error {
            frame.render_widget(Paragraph::new(error.as_str()).fg(theme::ERROR), footer_area);
        } else {
            let footer = if self.streaming {
                theme::help_line(&[("Ctrl+C", "stop"), ("Ctrl+M", "commands")])
            } else {
                theme::help_line(&[
                    ("Enter", "send"),
                    ("Ctrl+M", "commands"),
                    ("Ctrl+C", "quit"),
                ])
            };
            frame.render_widget(Paragraph::new(footer).fg(theme::TEXT_MUTED), footer_area);
        }
    }

    fn mark_scroll_dirty(&self) {
        self.scroll_dirty.set(true);
    }

    fn rebuild_scroll_view(&self, messages: &[(Role, String)], content_width: u16) {
        let lines: Vec<Line> = messages
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
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        let content_height = paragraph.line_count(content_width).min(u16::MAX as usize) as u16;
        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height))
            .scrollbars_visibility(ScrollbarVisibility::Never);
        scroll_view.render_widget(&paragraph, scroll_view.area());
        *self.scroll_view.borrow_mut() = scroll_view;
    }

    fn render_scrollbar(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        state: &ScrollViewState,
        content_height: u16,
    ) {
        let track_len = area.height as usize;
        if track_len < 2 {
            return;
        }
        let content_len = content_height as usize;
        let viewport_len = area.height as usize;
        let offset = state.offset().y as usize;
        let max_offset = content_len.saturating_sub(viewport_len);
        let max_start = track_len.saturating_sub(1);
        let thumb_len = max_start.max(1) * viewport_len / content_len.max(1);
        let thumb_len = thumb_len.clamp(1, max_start);
        let thumb_start = max_start
            .saturating_sub(thumb_len)
            .saturating_mul(offset)
            .div_ceil(max_offset.max(1))
            .min(max_start.saturating_sub(thumb_len));
        let bar_x = area.right().saturating_sub(1);
        let buf = frame.buffer_mut();
        for row in area.top()..area.bottom() {
            let y = row as usize;
            let (symbol, style) = if (thumb_start..thumb_start + thumb_len).contains(&y) {
                ("█", theme::ACCENT)
            } else {
                (" ", theme::TEXT_MUTED)
            };
            let cell = buf.cell_mut((bar_x, row)).expect("bar_x in bounds");
            cell.set_symbol(symbol);
            cell.set_style(Style::new().fg(style));
        }
    }

    fn follow_bottom(&self) {
        let mut state = self.scroll_state.borrow_mut();
        if state.is_at_bottom() {
            state.scroll_to_bottom();
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
