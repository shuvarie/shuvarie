use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Constraint::*, Layout, Rect, Size};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use shuvarie_catalog::TokenUsage;
use shuvarie_core::Role;
use termina::event::{KeyCode, KeyEvent};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::tui::utils::{alt, ctrl};

use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::sidebar::{Sidebar, SidebarMessage};
use super::theme;

pub enum SessionMessage {
    Text(TextAreaMessage),
    ScrollUp,
    ScrollDown,
    TokenReceived {
        content: String,
    },
    ContextLoaded {
        paths: Vec<String>,
    },
    ToolStarted {
        name: String,
        args: serde_json::Value,
        worker: Option<String>,
    },
    ToolFinished {
        name: String,
        ok: bool,
        output: String,
        worker: Option<String>,
    },
    WorkerStarted {
        name: String,
        args: serde_json::Value,
    },
    WorkerFinished {
        name: String,
        ok: bool,
        output: String,
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

#[derive(Debug, Clone)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

#[derive(Debug, Clone)]
pub struct ToolActivity {
    pub name: String,
    pub args: String,
    pub status: ToolStatus,
    pub output: String,
    pub worker: Option<String>,
    pub message_index: usize,
    pub text_offset: usize,
}

impl ToolActivity {
    fn belongs_to(&self, index: usize) -> bool {
        self.message_index == index
    }
}

#[derive(Debug, Clone)]
pub struct ContextActivity {
    pub paths: Vec<String>,
    pub message_index: usize,
}

pub struct SessionScreen {
    pub input: TextArea,
    pub messages: Vec<(Role, String)>,
    pub tools: Vec<ToolActivity>,
    pub context: Vec<ContextActivity>,
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
            tools: Vec::new(),
            context: Vec::new(),
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
            SessionMessage::ContextLoaded { paths } => {
                self.context.push(ContextActivity {
                    paths,
                    message_index: self.messages.len(),
                });
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::ToolStarted { name, args, worker } => {
                self.tools.push(ToolActivity {
                    name,
                    args: args.to_string(),
                    status: ToolStatus::Running,
                    output: String::new(),
                    worker,
                    message_index: self.messages.len(),
                    text_offset: self.pending.len(),
                });
                self.streaming = true;
                self.status = Some(format!("tool: {}", self.tools.last().unwrap().name));
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::ToolFinished {
                name,
                ok,
                output,
                worker,
            } => {
                if let Some(tool) = self.tools.iter_mut().find(|t| {
                    t.name == name && t.worker == worker && matches!(t.status, ToolStatus::Running)
                }) {
                    tool.status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Failed
                    };
                    tool.output = output;
                }
                self.status = None;
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::WorkerStarted { name, args } => {
                self.tools.push(ToolActivity {
                    name,
                    args: args.to_string(),
                    status: ToolStatus::Running,
                    output: String::new(),
                    worker: Some(String::new()),
                    message_index: self.messages.len(),
                    text_offset: self.pending.len(),
                });
                self.streaming = true;
                self.status = Some(format!("worker: {}", self.tools.last().unwrap().name));
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::WorkerFinished { name, ok, output } => {
                if let Some(tool) = self.tools.iter_mut().find(|t| {
                    t.name == name
                        && t.worker.as_deref() == Some("")
                        && matches!(t.status, ToolStatus::Running)
                }) {
                    tool.status = if ok {
                        ToolStatus::Ok
                    } else {
                        ToolStatus::Failed
                    };
                    tool.output = output;
                }
                self.status = None;
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::StreamDone => {
                if self.streaming {
                    let pending = std::mem::take(&mut self.pending);
                    self.messages.push((Role::Assistant, pending));
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
                self.tools
                    .retain(|t| t.message_index != self.messages.len());
                self.context
                    .retain(|c| c.message_index != self.messages.len());
                self.status = Some(format!("error: {error}"));
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::StreamCancelled => {
                self.streaming = false;
                self.pending.clear();
                self.tools
                    .retain(|t| t.message_index != self.messages.len());
                self.context
                    .retain(|c| c.message_index != self.messages.len());
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
                self.tools.clear();
                self.context.clear();
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
                self.tools.clear();
                self.context.clear();
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
        let mut lines: Vec<Line> = Vec::new();
        for (i, (role, content)) in messages.iter().enumerate() {
            match role {
                Role::User => {
                    lines.push(Line::from(Span::raw("You").fg(theme::ACCENT).bold()));
                    lines.append(&mut shuvarie_highlight::render(content));
                    lines.push(Line::from(""));
                }
                Role::Assistant => {
                    let tool_lines: Vec<&ToolActivity> =
                        self.tools.iter().filter(|t| t.belongs_to(i)).collect();
                    let context_lines: Vec<&ContextActivity> = self
                        .context
                        .iter()
                        .filter(|c| c.message_index == i)
                        .collect();
                    for context in context_lines {
                        self.push_context_lines(&mut lines, context);
                    }
                    if content.is_empty() {
                        for tool in &tool_lines {
                            self.push_tool_lines(&mut lines, tool);
                        }
                        let placeholder = if self.streaming && i == self.messages.len() {
                            "(working…)"
                        } else {
                            "(tool output only — no text reply)"
                        };
                        lines.push(Line::from(Span::raw(placeholder).fg(theme::TEXT_MUTED)));
                    } else {
                        self.push_interleaved(&mut lines, content, &tool_lines);
                    }
                    lines.push(Line::from(""));
                }
                Role::System => {
                    lines.push(Line::from(Span::raw("System").fg(theme::TEXT_MUTED).bold()));
                    lines.append(&mut shuvarie_highlight::render(content));
                    lines.push(Line::from(""));
                }
            }
        }
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        let content_height = paragraph.line_count(content_width).min(u16::MAX as usize) as u16;
        let mut scroll_view = ScrollView::new(Size::new(content_width, content_height))
            .scrollbars_visibility(ScrollbarVisibility::Never);
        scroll_view.render_widget(&paragraph, scroll_view.area());
        *self.scroll_view.borrow_mut() = scroll_view;
    }

    fn push_interleaved(&self, lines: &mut Vec<Line>, content: &str, tools: &[&ToolActivity]) {
        let mut tool_idx = 0;
        let mut text = String::new();
        let flush = |lines: &mut Vec<Line>, text: &mut String| {
            lines.append(&mut shuvarie_highlight::render(text));
            text.clear();
        };
        for (i, ch) in content.char_indices() {
            if tool_idx < tools.len() && i >= tools[tool_idx].text_offset {
                flush(lines, &mut text);
                while tool_idx < tools.len() && i >= tools[tool_idx].text_offset {
                    self.push_tool_lines(lines, tools[tool_idx]);
                    tool_idx += 1;
                }
            }
            text.push(ch);
        }
        flush(lines, &mut text);
        while tool_idx < tools.len() {
            self.push_tool_lines(lines, tools[tool_idx]);
            tool_idx += 1;
        }
    }

    fn push_tool_lines(&self, lines: &mut Vec<Line>, tool: &ToolActivity) {
        let (marker, fg) = match tool.status {
            ToolStatus::Running => ("›", theme::ACCENT),
            ToolStatus::Ok => ("✓", theme::SUCCESS),
            ToolStatus::Failed => ("✗", theme::ERROR),
        };
        let (prefix, marker, fg) = match tool.worker.as_deref() {
            Some("") => ("", "❖", theme::ACCENT),
            Some(_) => ("  ", marker, fg),
            None => ("", marker, fg),
        };
        let mut header = vec![Span::raw(format!("{prefix}{marker}")).fg(fg).bold()];
        header.push(Span::raw(format!(" {}", tool.name)).fg(theme::TEXT).bold());
        if !tool.args.is_empty() {
            let args = &tool.args;
            let args_display: String = if args.len() > 120 {
                format!("{}…", &args[..120])
            } else {
                args.clone()
            };
            header.push(Span::raw(format!(" {args_display}")).fg(theme::TEXT_MUTED));
        }
        lines.push(Line::from(header));
        if !tool.output.is_empty() {
            let first: String = tool
                .output
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            lines.push(Line::from(
                Span::raw(format!("{prefix}    {first}")).fg(theme::TEXT_DIM),
            ));
        }
    }

    fn push_context_lines(&self, lines: &mut Vec<Line>, context: &ContextActivity) {
        for path in &context.paths {
            let line = vec![
                Span::raw("◈").fg(theme::ACCENT).bold(),
                Span::raw(format!(" Loaded {path}")).fg(theme::TEXT),
            ];
            lines.push(Line::from(line));
        }
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
