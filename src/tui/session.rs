use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Constraint::*, Layout, Rect, Size};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph, Wrap};
use shuvarie_core::Role;
use shuvarie_llm::TokenUsage;
use shuvarie_llm::{DiffLine, DiffLineKind, FileChange, PatchFileKind};
use termina::event::{KeyCode, KeyEvent};
use tui_scrollview::{ScrollView, ScrollViewState, ScrollbarVisibility};

use crate::tui::utils::{alt, ctrl};

use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::question::{QuestionEffect, QuestionMessage, QuestionUI};
use super::sidebar::{Sidebar, SidebarMessage};
use super::theme;

pub enum SessionMessage {
    Text(TextAreaMessage),
    ScrollUp,
    ScrollDown,
    TokenReceived {
        content: String,
    },
    ReasoningReceived {
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
        file_change: Option<FileChange>,
        todo_update: Option<shuvarie_llm::TodoUpdate>,
    },
    ToolOutput {
        tool: String,
        worker: Option<String>,
        content: String,
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
    TurnReverted {
        session: shuvarie_core::Session,
    },
    TurnRestored {
        session: shuvarie_core::Session,
    },
    UpdateConfig {
        provider: Option<String>,
        model: Option<String>,
        context_length: Option<u64>,
    },
    LspDiagnostics {
        path: String,
        diagnostics: Vec<shuvarie_core::DiagnosticInfo>,
    },
    QuestionAsked {
        id: u64,
        questions: Vec<shuvarie_core::QuestionPrompt>,
    },
    Question(QuestionMessage),
}

#[derive(Debug, Clone, PartialEq)]
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
    pub file_change: Option<FileChange>,
    pub todo_list: Option<Vec<shuvarie_llm::TodoItem>>,
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
    pub question: QuestionUI,
    pub messages: Vec<(Role, String)>,
    pub summary_indices: std::collections::HashSet<usize>,
    pub tools: Vec<ToolActivity>,
    pub context: Vec<ContextActivity>,
    pub todos: Vec<shuvarie_llm::TodoItem>,
    pub reasoning: Vec<(usize, String)>,
    pub expanded_reasoning: std::collections::HashSet<usize>,
    pub streaming: bool,
    pub busy: bool,
    pub pending: String,
    pub pending_reasoning: String,
    pub interrupted: bool,
    pub scroll_state: RefCell<ScrollViewState>,
    scroll_view: RefCell<ScrollView>,
    scroll_dirty: Cell<bool>,
    scroll_width: Cell<u16>,
    committed_lines: RefCell<Vec<Line<'static>>>,
    committed_scroll: RefCell<ScrollView>,
    committed_dirty: Cell<bool>,
    pub status: Option<String>,
    pub sidebar: Sidebar,
    pub session_id: Option<u64>,
    pub session_title: Option<String>,
    pub error: Option<String>,
    pub lsp_diagnostics: std::collections::BTreeMap<String, Vec<shuvarie_core::DiagnosticInfo>>,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: TextArea::with_max_height("Type a message…", 8),
            question: QuestionUI::new(),
            messages: Vec::new(),
            summary_indices: std::collections::HashSet::new(),
            tools: Vec::new(),
            context: Vec::new(),
            todos: Vec::new(),
            reasoning: Vec::new(),
            expanded_reasoning: std::collections::HashSet::new(),
            streaming: false,
            busy: false,
            pending: String::new(),
            pending_reasoning: String::new(),
            interrupted: false,
            scroll_state: RefCell::new(ScrollViewState::default()),
            scroll_view: RefCell::new(ScrollView::new(Size::new(0, 0))),
            scroll_dirty: Cell::new(false),
            scroll_width: Cell::new(0),
            committed_lines: RefCell::new(Vec::new()),
            committed_scroll: RefCell::new(ScrollView::new(Size::new(0, 0))),
            committed_dirty: Cell::new(true),
            status: None,
            sidebar: Sidebar::new(),
            session_id: None,
            session_title: None,
            error: None,
            lsp_diagnostics: std::collections::BTreeMap::new(),
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SessionMessage> {
        if self.question.open {
            return self.question.map_event(key).map(SessionMessage::Question);
        }
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
            KeyCode::Up | KeyCode::Down if self.input_is_multiline() => {
                self.input.map_event(key).map(SessionMessage::Text)
            }
            KeyCode::Up => Some(SessionMessage::ScrollUp),
            KeyCode::Down => Some(SessionMessage::ScrollDown),
            _ => self.input.map_event(key).map(SessionMessage::Text),
        }
    }

    fn input_is_multiline(&self) -> bool {
        let width = self.input.width.get().max(1);
        let inner_w = width.saturating_sub(4).max(1);
        self.input.buffer.row_count(inner_w) > 1
    }

    pub fn update(&mut self, msg: SessionMessage) -> Option<SessionEffect> {
        match msg {
            SessionMessage::Text(m) => {
                if let Some(effect) = self.input.update(m) {
                    match effect {
                        TextAreaEffect::Submit { content } => {
                            self.messages.push((Role::User, content.clone()));
                            self.busy = true;
                            self.status = Some("thinking…".to_string());
                            self.mark_committed_dirty();
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
                self.busy = true;
                self.pending.push_str(&content);
                self.status = Some("streaming…".to_string());
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::ReasoningReceived { content } => {
                if !self.streaming {
                    self.streaming = true;
                }
                self.busy = true;
                self.pending_reasoning.push_str(&content);
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
                    file_change: None,
                    todo_list: None,
                });
                self.streaming = true;
                self.busy = true;
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
                file_change,
                todo_update,
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
                    tool.file_change = file_change;
                    if let Some(update) = &todo_update {
                        tool.todo_list = Some(update.todos.clone());
                        self.todos = update.todos.clone();
                        self.sidebar
                            .update(SidebarMessage::UpdateTodos(update.todos.clone()));
                    }
                }
                self.status = None;
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::ToolOutput {
                tool,
                worker,
                content,
            } => {
                if let Some(entry) = self.tools.iter_mut().find(|t| {
                    t.name == tool && t.worker == worker && matches!(t.status, ToolStatus::Running)
                }) {
                    entry.output = content;
                    self.mark_scroll_dirty();
                    self.follow_bottom();
                }
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
                    file_change: None,
                    todo_list: None,
                });
                self.streaming = true;
                self.busy = true;
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
                    let reasoning = std::mem::take(&mut self.pending_reasoning);
                    let idx = self.messages.len();
                    self.messages.push((Role::Assistant, pending));
                    if !reasoning.is_empty() {
                        self.reasoning.push((idx, reasoning));
                    }
                    self.streaming = false;
                    self.interrupted = false;
                }
                self.busy = false;
                self.status = None;
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::StreamError { error } => {
                if self.streaming {
                    let pending = std::mem::take(&mut self.pending);
                    let reasoning = std::mem::take(&mut self.pending_reasoning);
                    let idx = self.messages.len();
                    self.tools.retain(|t| {
                        t.message_index != idx || !matches!(t.status, ToolStatus::Running)
                    });
                    let has_tools = self.tools.iter().any(|t| t.message_index == idx);
                    if !pending.is_empty() || !reasoning.is_empty() || has_tools {
                        self.messages.push((Role::Assistant, pending));
                        if !reasoning.is_empty() {
                            self.reasoning.push((idx, reasoning));
                        }
                        self.interrupted = true;
                    } else {
                        self.context.retain(|c| c.message_index != idx);
                    }
                    self.streaming = false;
                    self.question.close();
                }
                self.busy = false;
                self.status = Some(format!("error: {error}"));
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
                None
            }
            SessionMessage::StreamCancelled => {
                if self.streaming {
                    let pending = std::mem::take(&mut self.pending);
                    let reasoning = std::mem::take(&mut self.pending_reasoning);
                    let idx = self.messages.len();
                    self.tools.retain(|t| {
                        t.message_index != idx || !matches!(t.status, ToolStatus::Running)
                    });
                    let has_tools = self.tools.iter().any(|t| t.message_index == idx);
                    if !pending.is_empty() || !reasoning.is_empty() || has_tools {
                        self.messages.push((Role::Assistant, pending));
                        if !reasoning.is_empty() {
                            self.reasoning.push((idx, reasoning));
                        }
                        self.interrupted = true;
                    } else {
                        self.context.retain(|c| c.message_index != idx);
                    }
                    self.streaming = false;
                    self.question.close();
                }
                self.busy = false;
                self.status = None;
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                self.follow_bottom();
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
                self.summary_indices.clear();
                self.tools.clear();
                self.context.clear();
                self.todos.clear();
                self.reasoning.clear();
                self.expanded_reasoning.clear();
                self.question.close();
                self.streaming = false;
                self.busy = false;
                self.pending.clear();
                self.pending_reasoning.clear();
                self.interrupted = false;
                self.status = None;
                self.session_id = None;
                self.session_title = None;
                self.lsp_diagnostics.clear();
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage::default(),
                    cost: 0.0,
                });
                *self.scroll_state.get_mut() = ScrollViewState::default();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::Loaded { id, title, session } => {
                let interrupted = session.last_assistant_interrupted();
                let usage = TokenUsage {
                    input_tokens: session.input_tokens,
                    output_tokens: session.output_tokens,
                    total_tokens: session.tokens,
                    cached_input_tokens: session.cached_tokens,
                    reasoning_tokens: session.reasoning_tokens,
                    ..Default::default()
                };
                let cost = session.cost;
                let todos = session.todos.clone();
                self.summary_indices = match session.summary_seq {
                    Some(seq) => std::iter::once(seq as usize).collect(),
                    None => std::collections::HashSet::new(),
                };
                self.messages = session
                    .messages
                    .into_iter()
                    .map(|m| (m.role, m.content))
                    .collect();
                self.tools = session
                    .tool_records
                    .iter()
                    .map(|tr| ToolActivity {
                        name: tr.name.clone(),
                        args: tr.args_json.clone(),
                        status: if tr.ok {
                            ToolStatus::Ok
                        } else {
                            ToolStatus::Failed
                        },
                        output: tr.output.clone(),
                        worker: tr.worker.clone(),
                        message_index: tr.message_seq as usize,
                        text_offset: 0,
                        file_change: tr.file_change.clone(),
                        todo_list: None,
                    })
                    .collect();
                self.todos = todos.clone();
                self.sidebar.update(SidebarMessage::UpdateTodos(todos));
                self.context.clear();
                self.question.close();
                self.reasoning = session
                    .reasoning
                    .iter()
                    .map(|(seq, text)| (*seq as usize, text.clone()))
                    .collect();
                self.expanded_reasoning.clear();
                self.streaming = false;
                self.busy = false;
                self.pending.clear();
                self.pending_reasoning.clear();
                self.interrupted = interrupted;
                self.status = None;
                self.session_id = Some(id);
                self.session_title = Some(title);
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                *self.scroll_state.get_mut() = ScrollViewState::default();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::TurnReverted { session } => {
                self.apply_session(session);
                *self.scroll_state.get_mut() = ScrollViewState::default();
                self.mark_committed_dirty();
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::TurnRestored { session } => {
                self.apply_session(session);
                self.mark_committed_dirty();
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
            SessionMessage::LspDiagnostics { path, diagnostics } => {
                if diagnostics.is_empty() {
                    self.lsp_diagnostics.remove(&path);
                } else {
                    self.lsp_diagnostics.insert(path, diagnostics);
                }
                self.mark_scroll_dirty();
                None
            }
            SessionMessage::QuestionAsked { id, questions } => {
                self.question.open(id, questions);
                None
            }
            SessionMessage::Question(m) => {
                if let Some(effect) = self.question.update(m) {
                    match effect {
                        QuestionEffect::Answer { id, answers } => {
                            self.question.close();
                            return Some(SessionEffect::AnswerQuestion { id, answers });
                        }
                    }
                }
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
        let input_height = if self.question.open {
            self.question.desired_height(content_area.width as usize)
        } else {
            self.input.desired_height(content_area.width as usize)
        };
        let todo_bar_height = if self.todos.iter().any(|t| t.status == "in_progress") {
            1
        } else {
            0
        };
        let [
            title_area,
            todo_bar_area,
            history_area,
            input_area,
            status_area,
            footer_area,
        ] = Layout::vertical([
            Length(1),
            Length(todo_bar_height),
            Min(0),
            Length(input_height),
            Length(1),
            Length(1),
        ])
        .areas(content_area);

        frame.render_widget(
            Paragraph::new(theme::title_header(&title))
                .bg(theme::SURFACE)
                .alignment(Alignment::Center),
            title_area,
        );

        if todo_bar_height > 0 {
            let active: Vec<&shuvarie_llm::TodoItem> = self
                .todos
                .iter()
                .filter(|t| t.status == "in_progress")
                .collect();
            let mut spans: Vec<Span> = Vec::new();
            for (i, todo) in active.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  ·  ").fg(theme::TEXT_MUTED));
                }
                spans.push(Span::raw("▸ ").fg(theme::ACCENT).bold());
                let content: String = todo.content.chars().take(60).collect();
                spans.push(Span::raw(content).fg(theme::TEXT));
            }
            frame.render_widget(
                Paragraph::new(Line::from(spans))
                    .bg(theme::SURFACE)
                    .alignment(Alignment::Left),
                todo_bar_area,
            );
        }

        let history_block = Block::new().padding(Padding::horizontal(2));
        let history_inner = history_block.inner(history_area);
        frame.render_widget(history_block, history_area);

        let content_width = history_inner.width.saturating_sub(1);
        if self.scroll_width.get() != content_width {
            self.scroll_dirty.set(true);
            self.committed_dirty.set(true);
            self.scroll_width.set(content_width);
        }
        if self.scroll_dirty.replace(false) {
            self.rebuild_scroll_view(content_width);
        }

        let mut scroll_state = self.scroll_state.borrow_mut();
        let scroll_view = self.scroll_view.borrow();
        frame.render_stateful_widget(&*scroll_view, history_inner, &mut scroll_state);

        let content_height = scroll_view.size().height;
        if content_height > history_inner.height {
            self.render_scrollbar(frame, history_inner, &scroll_state, content_height);
        }

        if self.question.open {
            self.question.view(frame, input_area);
        } else {
            self.input.view(frame, input_area);
        }

        if let Some(status) = &self.status {
            let mut spans = Vec::new();
            if self.busy {
                spans.push(super::spinner::spinner());
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(status.as_str()).fg(theme::TEXT_MUTED));
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        }

        if let Some(error) = &self.error {
            frame.render_widget(Paragraph::new(error.as_str()).fg(theme::ERROR), footer_area);
        } else {
            let footer = if self.streaming {
                theme::help_line(&[("Ctrl+C", "stop"), ("Ctrl+M", "commands")])
            } else if self.interrupted {
                theme::help_line(&[("Enter", "send"), ("Ctrl+M", "resume"), ("Ctrl+C", "quit")])
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

    /// Mark the scroll view dirty so an animated spinner re-renders.
    pub fn mark_spinner_dirty(&self) {
        self.scroll_dirty.set(true);
    }

    fn mark_committed_dirty(&self) {
        self.committed_dirty.set(true);
    }

    fn apply_session(&mut self, session: shuvarie_core::Session) {
        let interrupted = session.last_assistant_interrupted();
        let usage = TokenUsage {
            input_tokens: session.input_tokens,
            output_tokens: session.output_tokens,
            total_tokens: session.tokens,
            cached_input_tokens: session.cached_tokens,
            reasoning_tokens: session.reasoning_tokens,
            ..Default::default()
        };
        let cost = session.cost;
        let todos = session.todos.clone();
        self.summary_indices = match session.summary_seq {
            Some(seq) => std::iter::once(seq as usize).collect(),
            None => std::collections::HashSet::new(),
        };
        self.messages = session
            .messages
            .into_iter()
            .map(|m| (m.role, m.content))
            .collect();
        self.tools = session
            .tool_records
            .iter()
            .map(|tr| ToolActivity {
                name: tr.name.clone(),
                args: tr.args_json.clone(),
                status: if tr.ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Failed
                },
                output: tr.output.clone(),
                worker: tr.worker.clone(),
                message_index: tr.message_seq as usize,
                text_offset: 0,
                file_change: tr.file_change.clone(),
                todo_list: None,
            })
            .collect();
        self.todos = todos.clone();
        self.sidebar.update(SidebarMessage::UpdateTodos(todos));
        self.reasoning = session
            .reasoning
            .iter()
            .map(|(seq, text)| (*seq as usize, text.clone()))
            .collect();
        self.streaming = false;
        self.busy = false;
        self.pending.clear();
        self.pending_reasoning.clear();
        self.interrupted = interrupted;
        self.status = None;
        self.sidebar
            .update(SidebarMessage::SetUsage { usage, cost });
    }

    fn rebuild_scroll_view(&self, content_width: u16) {
        if self.committed_dirty.replace(false) {
            let mut committed: Vec<Line> = Vec::new();
            for (i, (role, content)) in self.messages.iter().enumerate() {
                self.push_message_lines(&mut committed, i, role, content);
            }
            *self.committed_lines.borrow_mut() = committed;

            let para =
                Paragraph::new(self.committed_lines.borrow().clone()).wrap(Wrap { trim: true });
            let height = para.line_count(content_width).min(u16::MAX as usize) as u16;
            let mut sv = ScrollView::new(Size::new(content_width, height))
                .scrollbars_visibility(ScrollbarVisibility::Never);
            sv.render_widget(&para, sv.area());
            *self.committed_scroll.borrow_mut() = sv;
        }

        let mut tail: Vec<Line> = Vec::new();
        if self.streaming {
            let i = self.messages.len();
            self.push_message_lines(&mut tail, i, &Role::Assistant, &self.pending);
        }

        let committed_scroll = self.committed_scroll.borrow();
        let committed_height = committed_scroll.size().height;
        let tail_height = if tail.is_empty() {
            0
        } else {
            let para = Paragraph::new(tail.clone()).wrap(Wrap { trim: true });
            para.line_count(content_width).min(u16::MAX as usize) as u16
        };

        let total_height = committed_height.saturating_add(tail_height);
        let mut scroll_view = ScrollView::new(Size::new(content_width, total_height))
            .scrollbars_visibility(ScrollbarVisibility::Never);

        {
            let src = committed_scroll.buf();
            let dst = scroll_view.buf_mut();
            for y in 0..committed_height {
                for x in 0..content_width {
                    dst[(x, y)] = src[(x, y)].clone();
                }
            }
        }

        if tail_height > 0 {
            let para = Paragraph::new(tail).wrap(Wrap { trim: true });
            scroll_view.render_widget(
                &para,
                Rect::new(0, committed_height, content_width, tail_height),
            );
        }

        *self.scroll_view.borrow_mut() = scroll_view;
    }

    fn push_message_lines(&self, lines: &mut Vec<Line>, i: usize, role: &Role, content: &str) {
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
                    self.push_context_lines(lines, context);
                }
                self.push_reasoning_lines(lines, i);
                if self.summary_indices.contains(&i) {
                    lines.push(Line::from(
                        Span::raw("◈ summary of earlier conversation")
                            .fg(theme::ACCENT)
                            .italic(),
                    ));
                }
                if content.is_empty() {
                    for tool in &tool_lines {
                        self.push_tool_lines(lines, tool);
                    }
                    let placeholder = if self.streaming && i == self.messages.len() {
                        "(working…)"
                    } else {
                        "(tool output only — no text reply)"
                    };
                    let mut spans = Vec::new();
                    if self.streaming && i == self.messages.len() {
                        spans.push(super::spinner::spinner());
                        spans.push(Span::raw(" "));
                    }
                    spans.push(Span::raw(placeholder).fg(theme::TEXT_MUTED));
                    lines.push(Line::from(spans));
                } else {
                    self.push_interleaved(lines, content, &tool_lines);
                }
                if self.interrupted && i == self.messages.len().saturating_sub(1) && !self.streaming
                {
                    lines.push(Line::from(
                        Span::raw("(interrupted)").fg(theme::WARNING).italic(),
                    ));
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

    fn push_reasoning_lines(&self, lines: &mut Vec<Line>, message_index: usize) {
        let reasoning: Option<&str> = if self.streaming && message_index == self.messages.len() {
            if self.pending_reasoning.is_empty() {
                None
            } else {
                Some(&self.pending_reasoning)
            }
        } else {
            self.reasoning
                .iter()
                .find(|(idx, _)| *idx == message_index)
                .map(|(_, text)| text.as_str())
        };
        let Some(reasoning) = reasoning else {
            return;
        };
        if reasoning.is_empty() {
            return;
        }
        let expanded = self.expanded_reasoning.contains(&message_index);
        let header = vec![
            Span::raw("⌥ ").fg(theme::TEXT_MUTED),
            Span::raw(if expanded {
                "thinking ▾"
            } else {
                "thinking ▸"
            })
            .fg(theme::TEXT_MUTED)
            .italic(),
        ];
        lines.push(Line::from(header));
        if expanded {
            for line in reasoning.lines() {
                lines.push(Line::from(
                    Span::raw(format!("  {line}")).fg(theme::TEXT_DIM).italic(),
                ));
            }
        }
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
        if tool.name == "question" {
            self.push_question_tool_lines(lines, tool);
            return;
        }
        if let Some(todos) = &tool.todo_list {
            self.push_todo_lines(lines, todos);
            return;
        }
        let (marker, fg) = match tool.status {
            ToolStatus::Running => ("", theme::ACCENT),
            ToolStatus::Ok => ("✓", theme::SUCCESS),
            ToolStatus::Failed => ("✗", theme::ERROR),
        };
        let (prefix, marker, fg) = match tool.worker.as_deref() {
            Some("") => ("", "❖", theme::ACCENT),
            Some(_) => ("  ", marker, fg),
            None => ("", marker, fg),
        };
        let mut header = Vec::new();
        if tool.status == ToolStatus::Running {
            if !prefix.is_empty() {
                header.push(Span::raw(prefix).fg(theme::TEXT_MUTED));
            }
            header.push(super::spinner::spinner());
        } else {
            header.push(Span::raw(format!("{prefix}{marker}")).fg(fg).bold());
        }
        header.push(Span::raw(format!(" {}", tool.name)).fg(theme::TEXT).bold());
        if !tool.args.is_empty() {
            let args_display: String = tool
                .args
                .chars()
                .take(120)
                .map(|c| if c == '\n' { ' ' } else { c })
                .collect();
            header.push(Span::raw(format!(" {args_display}")).fg(theme::TEXT_MUTED));
        }
        lines.push(Line::from(header));
        if !tool.output.is_empty() {
            let take: Vec<String> = if tool.status == ToolStatus::Running {
                let all: Vec<&str> = tool.output.lines().collect();
                let start = all.len().saturating_sub(3);
                all[start..].iter().map(|s| s.to_string()).collect()
            } else {
                vec![
                    tool.output
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(120)
                        .collect(),
                ]
            };
            if tool.status == ToolStatus::Running {
                let indent = format!("{prefix}    ");
                for line in take {
                    let truncated: String = line.chars().take(120).collect();
                    lines.push(Line::from(
                        Span::raw(format!("{indent}{truncated}")).fg(theme::TEXT_DIM),
                    ));
                }
            } else {
                let first = take.first().map(String::as_str).unwrap_or_default();
                lines.push(Line::from(
                    Span::raw(format!("{prefix}    {first}")).fg(theme::TEXT_DIM),
                ));
            }
        }
        if let Some(change) = &tool.file_change {
            self.push_file_change_lines(lines, change, prefix);
            let path = match change {
                FileChange::Edit { path, .. } => path,
                FileChange::Write { path, .. } => path,
                FileChange::Patch { files, .. } => match files.len() {
                    0 => "",
                    1 => files[0].path.as_str(),
                    _ => "",
                },
            };
            if !path.is_empty() {
                self.push_diagnostics_lines(lines, path, prefix);
            }
        }
    }

    fn push_todo_lines(&self, lines: &mut Vec<Line>, todos: &[shuvarie_llm::TodoItem]) {
        lines.push(Line::from(vec![
            Span::raw("▸ ").fg(theme::ACCENT).bold(),
            Span::raw("todos").fg(theme::TEXT).bold(),
        ]));
        for todo in todos {
            let (marker, fg) = match todo.status.as_str() {
                "completed" => ("☑", theme::SUCCESS),
                "in_progress" => ("▸", theme::ACCENT),
                "cancelled" => ("☐", theme::TEXT_MUTED),
                _ => ("☐", theme::TEXT_DIM),
            };
            let content: String = todo.content.chars().take(120).collect();
            lines.push(Line::from(vec![
                Span::raw(format!("  {marker} ")).fg(fg).bold(),
                Span::raw(content).fg(if todo.status == "in_progress" {
                    theme::TEXT
                } else {
                    theme::TEXT_DIM
                }),
            ]));
        }
    }

    /// Renders the `question` tool as a Q&A block: a `? Questions` header
    /// (spinner while pending) followed by each question and its chosen
    /// answers, parsed from the persisted tool args + output.
    fn push_question_tool_lines(&self, lines: &mut Vec<Line>, tool: &ToolActivity) {
        let mut header = Vec::new();
        if tool.status == ToolStatus::Running {
            header.push(super::spinner::spinner());
        } else {
            let (marker, fg) = if tool.status == ToolStatus::Ok {
                ("✓", theme::SUCCESS)
            } else {
                ("✗", theme::ERROR)
            };
            header.push(Span::raw(marker).fg(fg).bold());
        }
        header.push(Span::raw(" question").fg(theme::TEXT).bold());
        lines.push(Line::from(header));

        if tool.status == ToolStatus::Running {
            lines.push(Line::from(
                Span::raw("    asking…").fg(theme::TEXT_MUTED).italic(),
            ));
            return;
        }

        let questions = tool
            .args
            .is_empty()
            .then(Vec::new)
            .unwrap_or_else(|| parse_question_prompts(&tool.args));
        if tool.status == ToolStatus::Failed || questions.is_empty() {
            let first: String = tool
                .output
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(120)
                .collect();
            let note = if first.is_empty() {
                "dismissed".to_string()
            } else {
                first
            };
            lines.push(Line::from(
                Span::raw(format!("    {note}"))
                    .fg(theme::TEXT_DIM)
                    .italic(),
            ));
            return;
        }

        for q in &questions {
            let answer = answer_for_question(&tool.output, &q.question);
            lines.push(Line::from(vec![
                Span::raw("    ? ").fg(theme::ACCENT),
                Span::raw(q.question.clone()).fg(theme::TEXT),
            ]));
            let (marker, fg) = if answer.is_empty() {
                ("  ⚠ ", theme::WARNING)
            } else {
                ("  ⇒ ", theme::SUCCESS)
            };
            let shown = if answer.is_empty() {
                "unanswered".to_string()
            } else {
                answer
            };
            lines.push(Line::from(Span::raw(format!("{marker}{shown}")).fg(fg)));
        }
    }

    fn push_diagnostics_lines(&self, lines: &mut Vec<Line>, path: &str, prefix: &str) {
        let Some(diags) = self.lsp_diagnostics.get(path) else {
            return;
        };
        if diags.is_empty() {
            return;
        }
        lines.push(Line::from(vec![
            Span::raw(format!("{prefix}  ── diagnostics: ")).fg(theme::TEXT_MUTED),
            Span::raw(path.to_string()).fg(theme::ACCENT),
        ]));
        for d in diags {
            let (sev_label, sev_color) = match d.severity {
                shuvarie_core::DiagnosticSeverity::Error => ("error", theme::ERROR),
                shuvarie_core::DiagnosticSeverity::Warning => ("warning", theme::WARNING),
                shuvarie_core::DiagnosticSeverity::Information => ("info", theme::ACCENT),
                shuvarie_core::DiagnosticSeverity::Hint => ("hint", theme::TEXT_DIM),
            };
            let loc = format!("{}:{}", d.line, d.col);
            let msg = d.message.chars().take(140).collect::<String>();
            lines.push(Line::from(vec![
                Span::raw(format!("{prefix}    {loc:<10} ")).fg(theme::TEXT_MUTED),
                Span::raw(format!("{sev_label:<8} ")).fg(sev_color),
                Span::raw(msg).fg(theme::TEXT_DIM),
            ]));
        }
    }

    fn push_file_change_lines(&self, lines: &mut Vec<Line>, change: &FileChange, prefix: &str) {
        match change {
            FileChange::Edit { path, diff, .. } => {
                lines.push(Line::from(vec![
                    Span::raw(format!("{prefix}  ── diff: ")).fg(theme::TEXT_MUTED),
                    Span::raw(path.clone()).fg(theme::ACCENT),
                ]));
                for line in diff {
                    self.push_diff_line(lines, line, prefix);
                }
            }
            FileChange::Write { path, content, .. } => {
                lines.push(Line::from(vec![
                    Span::raw(format!("{prefix}  ── new file: ")).fg(theme::TEXT_MUTED),
                    Span::raw(path.clone()).fg(theme::ACCENT),
                ]));
                for (i, text) in content.lines().enumerate() {
                    let num = format!("{:>4}", i + 1);
                    lines.push(Line::from(vec![
                        Span::raw(format!("{prefix}    {num} ")).fg(theme::TEXT_MUTED),
                        Span::raw(text.to_string()).fg(theme::TEXT_DIM),
                    ]));
                }
            }
            FileChange::Patch { files, .. } => {
                for file in files {
                    match (&file.kind, &file.moved_to) {
                        (PatchFileKind::Delete, _) => {
                            lines.push(Line::from(vec![
                                Span::raw(format!("{prefix}  ── deleted: ")).fg(theme::TEXT_MUTED),
                                Span::raw(file.path.clone()).fg(theme::ERROR),
                            ]));
                        }
                        (PatchFileKind::Add, _) => {
                            lines.push(Line::from(vec![
                                Span::raw(format!("{prefix}  ── new file: ")).fg(theme::TEXT_MUTED),
                                Span::raw(file.path.clone()).fg(theme::ACCENT),
                            ]));
                            for (i, text) in
                                file.new.as_deref().unwrap_or_default().lines().enumerate()
                            {
                                let num = format!("{:>4}", i + 1);
                                lines.push(Line::from(vec![
                                    Span::raw(format!("{prefix}    {num} ")).fg(theme::TEXT_MUTED),
                                    Span::raw(text.to_string()).fg(theme::TEXT_DIM),
                                ]));
                            }
                        }
                        (PatchFileKind::Update, Some(target)) => {
                            lines.push(Line::from(vec![
                                Span::raw(format!("{prefix}  ── moved: ")).fg(theme::TEXT_MUTED),
                                Span::raw(file.path.clone()).fg(theme::ACCENT),
                                Span::raw(" → ").fg(theme::TEXT_MUTED),
                                Span::raw(target.clone()).fg(theme::ACCENT),
                            ]));
                            for line in &file.diff {
                                self.push_diff_line(lines, line, prefix);
                            }
                        }
                        (PatchFileKind::Update, None) => {
                            lines.push(Line::from(vec![
                                Span::raw(format!("{prefix}  ── diff: ")).fg(theme::TEXT_MUTED),
                                Span::raw(file.path.clone()).fg(theme::ACCENT),
                            ]));
                            for line in &file.diff {
                                self.push_diff_line(lines, line, prefix);
                            }
                        }
                    }
                }
            }
        }
    }

    fn push_diff_line(&self, lines: &mut Vec<Line>, line: &DiffLine, prefix: &str) {
        if line.kind == DiffLineKind::Ellipsis {
            lines.push(Line::from(
                Span::raw(format!("{prefix}    …")).fg(theme::TEXT_MUTED),
            ));
            return;
        }
        let (marker, fg) = match line.kind {
            DiffLineKind::Add => ("+", theme::SUCCESS),
            DiffLineKind::Remove => ("-", theme::ERROR),
            DiffLineKind::Context => (" ", theme::TEXT_DIM),
            DiffLineKind::Ellipsis => unreachable!(),
        };
        let old_num = line
            .old_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        let new_num = line
            .new_line
            .map(|n| format!("{n:>4}"))
            .unwrap_or_else(|| "    ".to_string());
        let text = line.text.trim_end_matches('\n');
        lines.push(Line::from(vec![
            Span::raw(format!("{prefix}  {old_num} {new_num} ")).fg(theme::TEXT_MUTED),
            Span::raw(marker).fg(fg).bold(),
            Span::raw(text.to_string()).fg(fg),
        ]));
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
    SendMessage {
        content: String,
    },
    CancelStream,
    AnswerQuestion {
        id: u64,
        answers: Option<Vec<Vec<String>>>,
    },
}

/// Parse the `question` tool's args JSON into question prompts (defensive:
/// returns empty when the args are malformed).
fn parse_question_prompts(args_json: &str) -> Vec<shuvarie_core::QuestionPrompt> {
    serde_json::from_str::<serde_json::Value>(args_json)
        .ok()
        .and_then(|v| serde_json::from_value(v.get("questions").cloned()?).ok())
        .unwrap_or_default()
}

/// Extract the answer recorded in the tool output for a question whose text
/// starts with `question`. Output format: `... "q"="a, b", "q2"="c"`.
fn answer_for_question(output: &str, question: &str) -> String {
    let pairs = match output.split_once(": ") {
        Some((_, rest)) => rest,
        None => return String::new(),
    };
    for pair in pairs.split("\", \"") {
        let pair = pair.trim_start_matches('"');
        if let Some((q, a)) = pair.split_once("\"=\"")
            && q == question
        {
            return a.trim_end_matches('"').to_string();
        }
    }
    String::new()
}

impl Default for SessionScreen {
    fn default() -> Self {
        Self::new()
    }
}
