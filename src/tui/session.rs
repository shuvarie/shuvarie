use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::utils::{alt, alt_shift, ctrl};

use super::commands::{self, CommandAction};
use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::question::{QuestionEffect, QuestionMessage, QuestionUI};
use super::sidebar::{Sidebar, SidebarMessage};
use super::slash::{SlashMenu, SlashMessage};
use super::spinner::SpinnerKind;
use super::theme;
use shuvarie_core::Skill;

pub mod blocks;
pub mod chat;
pub mod segment;
pub mod virtualizer;

pub use chat::ChatMessage;

pub enum SessionMessage {
    Text(TextAreaMessage),
    Chat(ChatMessage),
    CancelRequested,
    ShowError {
        error: String,
    },
    ClearError,
    /// Replaces the session's skill list (mirrors what the sidebar shows).
    SetSkills {
        skills: Vec<Skill>,
    },
    /// Per-request usage added to the sidebar's running totals;
    /// `context_tokens` is the request's context footprint when it came from
    /// the main stream (workers run separate conversations), so the sidebar
    /// can anchor its context-occupancy display on the latest one.
    UsageUpdate {
        usage: TokenUsage,
        cost: f64,
        context_tokens: Option<u64>,
    },
    /// Authoritative cumulative usage sent after a turn commits; replaces the
    /// sidebar's running totals so live per-request updates resync.
    UsageSnapshot {
        usage: TokenUsage,
        cost: f64,
    },
    UpdateConfig {
        provider: Option<String>,
        model: Option<String>,
        context_length: Option<u64>,
    },
    QuestionAsked {
        id: u64,
        questions: Vec<shuvarie_core::QuestionPrompt>,
    },
    Question(QuestionMessage),
    Slash(SlashMessage),
    /// A retryable connection failure; the core task re-sends the turn after
    /// `delay_ms`. Shows a red countdown in the status row.
    RetryScheduled {
        reason: String,
        attempt: usize,
        max_attempts: usize,
        delay_ms: u64,
    },
    /// An LLM compaction summarizer call is running after a context-budget
    /// overflow. No stream events arrive until `CompactionFinished`, so the
    /// busy indicator must stay armed or the screen stops rendering for the
    /// whole call.
    CompactionStarted,
    /// The compaction summarizer call finished; the core replays the
    /// interrupted turn.
    CompactionFinished,
    Reset,
    Loaded {
        id: uuid::Uuid,
        title: String,
        session: shuvarie_core::Session,
    },
    TurnReverted {
        session: shuvarie_core::Session,
    },
    TurnRestored {
        session: shuvarie_core::Session,
    },
    /// A user turn started streaming in the core: either an accepted submit
    /// or a dispatched steered prompt. Renders the user prompt and arms the
    /// busy indicator; when `steered`, the first queued entry also leaves the
    /// chat display.
    TurnStarted {
        content: String,
        steered: bool,
    },
    /// Recall a steered prompt into the input area (Alt+Up / Alt+Shift+Up).
    RecallSteered {
        stacked: bool,
    },
    /// The core answered a recall request: the recalled content, or `None`
    /// when nothing was queued.
    SteeredRecalled {
        stacked: bool,
        content: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyKind {
    Generating,
    Tool,
    Waiting,
}

/// A scheduled connection retry, rendered as a red countdown in the status
/// row (`<reason>. Retry in <seconds>s [<attempt>/<cap>]`).
#[derive(Debug, Clone)]
pub(crate) struct RetryWait {
    pub(crate) reason: String,
    pub(crate) attempt: usize,
    pub(crate) max_attempts: usize,
    pub(crate) deadline: std::time::Instant,
}

/// In-progress todo rows shown in the strip under the title bar; further
/// items collapse into an overflow hint row.
const MAX_WORKING_ROWS: usize = 3;

/// The session screen: sidebar, title bar with the working-todos strip, chat
/// history pane (a [`chat::Chat`] TEA model), input, question prompt, slash
/// menu, status row, and footer.
pub struct SessionScreen {
    pub input: TextArea,
    pub question: QuestionUI,
    slash: SlashMenu,
    pub chat: chat::Chat,
    pub busy: bool,
    busy_kind: BusyKind,
    pub status: Option<String>,
    pub sidebar: Sidebar,
    provider: Option<String>,
    model: Option<String>,
    pub session_id: Option<uuid::Uuid>,
    pub session_title: Option<String>,
    pub error: Option<String>,
    pub(crate) retry: Option<RetryWait>,
    working_todos: Vec<shuvarie_core::tools::todos::TodoItem>,
    skills: Vec<Skill>,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: TextArea::with_max_height("Type a message", 8),
            question: QuestionUI::new(),
            slash: SlashMenu::new(),
            chat: chat::Chat::new(),
            busy: false,
            busy_kind: BusyKind::Generating,
            status: None,
            sidebar: Sidebar::new(),
            provider: None,
            model: None,
            session_id: None,
            session_title: None,
            error: None,
            retry: None,
            working_todos: Vec::new(),
            skills: Vec::new(),
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.chat.is_streaming()
    }

    /// The wide status-row spinner currently animating, if any.
    pub(crate) fn busy_spinner(&self) -> Option<SpinnerKind> {
        self.busy.then_some(match self.busy_kind {
            BusyKind::Generating => SpinnerKind::Generating,
            BusyKind::Tool => SpinnerKind::Tool,
            BusyKind::Waiting => SpinnerKind::Waiting,
        })
    }

    pub fn has_messages(&self) -> bool {
        self.chat.has_messages()
    }

    pub fn can_continue(&self) -> bool {
        self.chat.last_turn_interrupted()
    }

    pub fn begin_continue(&mut self) {
        self.chat.update(ChatMessage::BeginUserTurn {
            content: shuvarie_core::session::CONTINUE_PROMPT.to_string(),
        });
        self.busy = true;
        self.busy_kind = BusyKind::Generating;
        self.status = Some("Thinking...".to_string());
        self.retry = None;
    }

    /// Mark the chat dirty so an animated spinner re-renders.
    pub fn mark_spinner_dirty(&self) {
        self.chat.mark_spinner_dirty();
    }

    /// Expand a `/skill:<name> [args]` submit into the skill's prompt
    /// content. `Ok(None)` when the text is not a skill invocation;
    /// `Err(reason)` for an unknown skill or an unreadable SKILL.md.
    fn expand_skill(&self, content: &str) -> Result<Option<String>, String> {
        let Some(invocation) = commands::parse_skill_invocation(content) else {
            return Ok(None);
        };
        let skill = self
            .skills
            .iter()
            .find(|s| s.name == invocation.name)
            .ok_or_else(|| format!("unknown skill: {}", invocation.name))?;
        skill
            .invocation_content(invocation.args.as_deref())
            .map(Some)
            .map_err(|e| format!("failed to read skill {}: {e}", invocation.name))
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SessionMessage> {
        if self.question.open {
            return self.question.map_event(key).map(SessionMessage::Question);
        }
        if self.slash.active()
            && let Some(m) = self.slash.map_event(key)
        {
            return Some(SessionMessage::Slash(m));
        }
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(SessionMessage::Chat(ChatMessage::ScrollDown)),
                KeyCode::Char('p') => Some(SessionMessage::Chat(ChatMessage::ScrollUp)),
                KeyCode::Char('o') => Some(SessionMessage::Chat(ChatMessage::ToggleLastTool)),
                _ => self.input.map_event(key).map(SessionMessage::Text),
            };
        }
        if alt(key) {
            if key.code == KeyCode::Up {
                return Some(SessionMessage::RecallSteered {
                    stacked: alt_shift(key),
                });
            }
            return self.input.map_event(key).map(SessionMessage::Text);
        }
        match key.code {
            KeyCode::Up | KeyCode::Down if self.input_is_multiline() => {
                self.input.map_event(key).map(SessionMessage::Text)
            }
            KeyCode::Up => Some(SessionMessage::Chat(ChatMessage::ScrollUp)),
            KeyCode::Down => Some(SessionMessage::Chat(ChatMessage::ScrollDown)),
            _ => self.input.map_event(key).map(SessionMessage::Text),
        }
    }

    fn input_is_multiline(&self) -> bool {
        let width = self.input.width.get().max(1);
        let inner_w = width.saturating_sub(4).max(1);
        self.input.buffer.row_count(inner_w) > 1
    }

    fn sync_slash(&mut self) {
        let has_messages = self.chat.has_messages();
        self.slash
            .set_availability(CommandAction::UndoLastTurn, has_messages);
        self.slash
            .set_availability(CommandAction::Redo, has_messages);
        self.slash
            .set_availability(CommandAction::Replay, has_messages);
        self.slash
            .set_availability(CommandAction::Continue, self.chat.last_turn_interrupted());
        let buffer = self.input.buffer.value.clone();
        self.slash.sync(&buffer);
    }

    pub fn update(&mut self, msg: SessionMessage) -> Option<SessionEffect> {
        self.sync_slash();
        match msg {
            SessionMessage::Text(m) => {
                if let Some(effect) = self.input.update(m) {
                    match effect {
                        TextAreaEffect::Submit { content } => {
                            if let Some(action) = commands::parse_command(&content) {
                                self.sync_slash();
                                return Some(SessionEffect::RunCommand(action));
                            }
                            let content = match self.expand_skill(&content) {
                                Ok(Some(expanded)) => expanded,
                                Ok(None) => commands::unescape(&content).to_string(),
                                Err(error) => {
                                    self.error = Some(error);
                                    return None;
                                }
                            };
                            // Display of the user prompt is event-driven: the
                            // core's `TurnStarted` decides whether this begins a
                            // turn or gets steered behind a busy agent.
                            self.sync_slash();
                            return Some(SessionEffect::SendMessage { content });
                        }
                    }
                }
                self.sync_slash();
                None
            }
            SessionMessage::Chat(msg) => {
                self.observe_chat(&msg);
                self.chat.update(msg);
                None
            }
            SessionMessage::Slash(m) => match m {
                SlashMessage::Next => {
                    self.slash.next();
                    None
                }
                SlashMessage::Prev => {
                    self.slash.prev();
                    None
                }
                SlashMessage::Complete => {
                    if let Some(action) = self.slash.selected_action() {
                        let text = format!("{}{} ", self.slash.trigger_char(), action.slash_name());
                        self.input.buffer.set(&text);
                    }
                    self.sync_slash();
                    None
                }
                SlashMessage::Run => {
                    if let Some(action) = self.slash.selected_action() {
                        self.input.buffer.clear();
                        self.sync_slash();
                        return Some(SessionEffect::RunCommand(action));
                    }
                    None
                }
                SlashMessage::Dismiss => {
                    self.slash.dismiss();
                    None
                }
            },
            SessionMessage::CancelRequested => {
                if self.chat.is_streaming() {
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
            SessionMessage::SetSkills { skills } => {
                self.skills = skills;
                None
            }
            SessionMessage::UsageUpdate {
                usage,
                cost,
                context_tokens,
            } => {
                self.sidebar.update(SidebarMessage::UpdateUsage {
                    usage,
                    cost,
                    context_tokens,
                });
                None
            }
            SessionMessage::UsageSnapshot { usage, cost } => {
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                None
            }
            SessionMessage::UpdateConfig {
                provider,
                model,
                context_length,
            } => {
                self.provider = provider;
                self.model = model;
                self.sidebar
                    .update(SidebarMessage::UpdateConfig { context_length });
                None
            }
            SessionMessage::QuestionAsked { id, questions } => {
                self.question.open(id, questions);
                self.busy = true;
                self.busy_kind = BusyKind::Waiting;
                self.status = Some("Waiting for answer...".to_string());
                None
            }
            SessionMessage::Question(m) => {
                if let Some(effect) = self.question.update(m) {
                    match effect {
                        QuestionEffect::Answer { id, answers } => {
                            self.question.close();
                            self.busy_kind = BusyKind::Tool;
                            self.status = Some("Calling tool: question".to_string());
                            return Some(SessionEffect::AnswerQuestion { id, answers });
                        }
                    }
                }
                None
            }
            SessionMessage::RetryScheduled {
                reason,
                attempt,
                max_attempts,
                delay_ms,
            } => {
                self.retry = Some(RetryWait {
                    reason,
                    attempt,
                    max_attempts,
                    deadline: std::time::Instant::now()
                        + std::time::Duration::from_millis(delay_ms),
                });
                self.busy = true;
                self.busy_kind = BusyKind::Waiting;
                self.status = None;
                None
            }
            SessionMessage::CompactionStarted => {
                self.busy = true;
                self.busy_kind = BusyKind::Waiting;
                self.status = Some("Compacting context...".to_string());
                self.retry = None;
                // The post-compaction context is unknown until the replay's
                // next response; the stale pre-compaction footprint would
                // overstate occupancy.
                self.sidebar
                    .update(SidebarMessage::SetContextTokens { tokens: None });
                None
            }
            SessionMessage::CompactionFinished => {
                self.busy = true;
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Resuming after compaction...".to_string());
                self.retry = None;
                None
            }
            SessionMessage::Reset => {
                self.chat.update(ChatMessage::Reset);
                self.busy = false;
                self.busy_kind = BusyKind::Generating;
                self.status = None;
                self.retry = None;
                self.session_id = None;
                self.session_title = None;
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage::default(),
                    cost: 0.0,
                });
                self.sidebar
                    .update(SidebarMessage::SetContextTokens { tokens: None });
                self.sync_todos(Vec::new());
                None
            }
            SessionMessage::Loaded { id, title, session } => {
                let usage = session.usage();
                let cost = session.cost;
                self.session_id = Some(id);
                self.session_title = Some(title);
                self.status = None;
                self.retry = None;
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sidebar
                    .update(SidebarMessage::SetContextTokens { tokens: None });
                self.sync_todos(shuvarie_core::tools::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::Load { session });
                None
            }
            SessionMessage::TurnReverted { session } => {
                let usage = session.usage();
                let cost = session.cost;
                self.retry = None;
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sidebar
                    .update(SidebarMessage::SetContextTokens { tokens: None });
                self.sync_todos(shuvarie_core::tools::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::TurnReverted { session });
                None
            }
            SessionMessage::TurnRestored { session } => {
                let usage = session.usage();
                let cost = session.cost;
                self.retry = None;
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sidebar
                    .update(SidebarMessage::SetContextTokens { tokens: None });
                self.sync_todos(shuvarie_core::tools::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::TurnRestored { session });
                None
            }
            SessionMessage::TurnStarted { content, steered } => {
                if steered {
                    self.chat.update(ChatMessage::SteeredDispatched);
                }
                self.chat.update(ChatMessage::BeginUserTurn { content });
                self.busy = true;
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Thinking...".to_string());
                self.retry = None;
                self.sync_slash();
                None
            }
            SessionMessage::RecallSteered { stacked } => {
                Some(SessionEffect::RecallSteered { stacked })
            }
            SessionMessage::SteeredRecalled { stacked, content } => {
                let content = content?;
                self.chat.update(ChatMessage::SteeredRecalled);
                if stacked && !self.input.is_empty() {
                    let existing = self.input.buffer.value.clone();
                    self.input.buffer.set(&format!("{content}\n\n{existing}"));
                } else {
                    self.input.buffer.set(&content);
                }
                self.sync_slash();
                None
            }
        }
    }

    /// Mirror the streaming lifecycle onto the screen's busy/status row while
    /// the chat model owns the rendering-side state.
    fn observe_chat(&mut self, msg: &ChatMessage) {
        match msg {
            ChatMessage::TokenReceived { .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Yappin'...".to_string());
                self.retry = None;
            }
            ChatMessage::ReasoningReceived { .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Thinking...".to_string());
                self.retry = None;
            }
            ChatMessage::ToolStarted { name, .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Tool;
                self.status = Some(format!("Calling tool: {name}"));
                self.retry = None;
            }
            ChatMessage::WorkerStarted { name, .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Tool;
                self.status = Some(format!("Spawned worker: {name}"));
                self.retry = None;
            }
            ChatMessage::ToolFinished {
                name, ok, output, ..
            } => {
                self.status = None;
                if name == "todo" && *ok {
                    self.sync_todo_output(output);
                }
            }
            ChatMessage::WorkerFinished { .. } => {
                self.status = None;
            }
            ChatMessage::StreamDone => {
                self.busy = false;
                self.status = None;
                self.retry = None;
            }
            ChatMessage::StreamError { error } => {
                self.busy = false;
                self.status = Some(format!("error: {error}"));
                self.retry = None;
            }
            ChatMessage::StreamCancelled => {
                self.busy = false;
                self.status = None;
                self.retry = None;
            }
            _ => {}
        }
    }

    /// Apply a full todo list: sidebar counts plus the working-items strip
    /// (the in-progress items shown under the title bar).
    fn sync_todos(&mut self, items: Vec<shuvarie_core::tools::todos::TodoItem>) {
        let (done, total) = shuvarie_core::tools::todos::done_total(&items);
        self.sidebar
            .update(SidebarMessage::SetTodos { done, total });
        self.working_todos = items
            .into_iter()
            .filter(|item| item.status == shuvarie_core::tools::todos::TodoStatus::InProgress)
            .collect();
    }

    /// Parse the finished `todo` tool call's list output. A successful call
    /// always carries the full list, so no item rows means the list is empty
    /// (`Todos (none)`).
    fn sync_todo_output(&mut self, output: &str) {
        self.sync_todos(shuvarie_core::tools::todos::parse_items(output).unwrap_or_default());
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
                self.provider.as_deref().unwrap_or("?"),
                self.model.as_deref().unwrap_or("?")
            ),
        };
        let input_height = if self.question.open {
            self.question.desired_height(content_area.width as usize)
        } else {
            self.input.desired_height(content_area.width as usize)
        };
        let working_rows = working_rows(&self.working_todos);
        let [
            title_area,
            todos_area,
            history_area,
            input_area,
            status_area,
            footer_area,
        ] = Layout::vertical([
            Length(1),
            Length(working_rows),
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

        if working_rows > 0 {
            let todos_block = Block::new()
                .bg(theme::SURFACE)
                .padding(Padding::horizontal(2));
            let todos_inner = todos_block.inner(todos_area);
            frame.render_widget(todos_block, todos_area);
            frame.render_widget(
                Paragraph::new(working_todo_lines(&self.working_todos, todos_inner.width)),
                todos_inner,
            );
        }

        let history_block = Block::new().padding(Padding::horizontal(2));
        let history_inner = history_block.inner(history_area);
        frame.render_widget(history_block, history_area);
        self.chat.view(frame, history_inner);

        if self.question.open {
            self.question.view(frame, input_area);
        } else {
            self.input.view(frame, input_area);
        }

        if !self.question.open && self.slash.active() {
            let rect = self.slash.popup_rect(history_area, input_area);
            self.slash.view(frame, rect);
        }

        let connection = self.provider.as_ref().map(|p| {
            let mut spans = vec![Span::raw(p.clone()).fg(theme::TEXT)];
            if let Some(m) = &self.model {
                spans.push(Span::raw(":").fg(theme::TEXT_MUTED));
                spans.push(Span::raw(m.clone()).fg(theme::TEXT_DIM));
            }
            Line::from(spans)
        });
        let [status_area, conn_area] = match &connection {
            Some(line) => {
                let width = line.width() as u16;
                Layout::horizontal([Constraint::Min(0), Constraint::Length(width)])
                    .areas(status_area)
            }
            None => [status_area, Rect::ZERO],
        };
        if let Some(line) = connection {
            frame.render_widget(Paragraph::new(line).alignment(Alignment::Right), conn_area);
        }

        let status = match self.status.as_deref() {
            Some(status) => Some((status, self.busy_kind)),
            None if self.busy => Some(("Working...", BusyKind::Tool)),
            None => None,
        };
        if let Some(retry) = &self.retry {
            let remaining = retry
                .deadline
                .saturating_duration_since(std::time::Instant::now());
            let secs = (remaining.as_secs_f64()).ceil().max(1.0) as u64;
            let text = format!(
                "{}. Retry in {}s [{}/{}]",
                retry.reason, secs, retry.attempt, retry.max_attempts
            );
            let spans = vec![
                super::spinner::wait_spinner(),
                Span::raw(" "),
                Span::raw(text).fg(theme::ERROR),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        } else if let Some((status, kind)) = status {
            let mut spans = Vec::new();
            if self.busy {
                spans.push(match kind {
                    BusyKind::Generating => super::spinner::generating_spinner(),
                    BusyKind::Tool => super::spinner::tool_spinner(),
                    BusyKind::Waiting => super::spinner::wait_spinner(),
                });
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(status).fg(theme::TEXT_MUTED));
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        }

        if let Some(error) = &self.error {
            frame.render_widget(Paragraph::new(error.as_str()).fg(theme::ERROR), footer_area);
        } else {
            let recall = self
                .chat
                .has_steered()
                .then_some(("Alt+↑", "recall steered"));
            let footer = if !self.question.open && self.slash.active() {
                theme::help_line(&[("Tab", "complete"), ("↑↓", "select"), ("Esc", "dismiss")])
            } else if self.chat.is_streaming() {
                let ctrl_c = if self.input.is_empty() {
                    "stop"
                } else {
                    "clear"
                };
                let mut bindings = vec![("Ctrl+C", ctrl_c), ("Ctrl+M", "commands")];
                bindings.extend(recall);
                theme::help_line(&bindings)
            } else {
                let ctrl_c = if self.input.is_empty() {
                    "quit"
                } else {
                    "clear"
                };
                let mut bindings = vec![
                    ("Enter", "send"),
                    ("Ctrl+M", "commands"),
                    ("Ctrl+C", ctrl_c),
                ];
                bindings.extend(recall);
                theme::help_line(&bindings)
            };
            frame.render_widget(Paragraph::new(footer).fg(theme::TEXT_MUTED), footer_area);
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
    RunCommand(CommandAction),
    RecallSteered {
        stacked: bool,
    },
}

/// Rows the working-todos strip occupies below the title bar: one per
/// in-progress item up to [`MAX_WORKING_ROWS`], plus an overflow hint row.
fn working_rows(todos: &[shuvarie_core::tools::todos::TodoItem]) -> u16 {
    let n = todos.len();
    if n == 0 {
        0
    } else {
        (n.min(MAX_WORKING_ROWS) + usize::from(n > MAX_WORKING_ROWS)) as u16
    }
}

/// The strip's lines: `~ #id text` per in-progress todo, truncated to the
/// strip width, then an overflow hint when more items are working.
fn working_todo_lines(
    todos: &[shuvarie_core::tools::todos::TodoItem],
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in todos.iter().take(MAX_WORKING_ROWS) {
        let prefix = format!("~ #{} ", item.id);
        let text_width = usize::from(width).saturating_sub(prefix.chars().count() + 1);
        lines.push(Line::from(vec![
            Span::raw("~").fg(theme::WARNING),
            Span::raw(format!(" #{} ", item.id)).fg(theme::TEXT_MUTED),
            elided_span(&item.text, text_width, theme::TEXT),
        ]));
    }
    let overflow = todos.len().saturating_sub(MAX_WORKING_ROWS);
    if overflow > 0 {
        lines.push(Line::from(format!("… +{overflow} more in progress")).fg(theme::TEXT_MUTED));
    }
    lines
}

/// A span truncated to `max_width` display columns, ending with `…` when
/// characters were dropped.
fn elided_span(text: &str, max_width: usize, color: Color) -> Span<'static> {
    if UnicodeWidthStr::width(text) <= max_width {
        return Span::raw(text.to_string()).fg(color);
    }
    if max_width == 0 {
        return Span::raw(String::new()).fg(color);
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw >= max_width {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    Span::raw(out).fg(color)
}

impl Default for SessionScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use shuvarie_core::tool_record::ToolRecord;
    use termina::event::Modifiers;

    fn todo_record(args_json: &str) -> ToolRecord {
        ToolRecord {
            name: "todo".into(),
            args_json: args_json.into(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            worker: None,
            message_id: 1,
            message_seq: 0,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 0,
        }
    }

    fn todo_finish(output: &str) -> SessionMessage {
        SessionMessage::Chat(ChatMessage::ToolFinished {
            name: "todo".into(),
            ok: true,
            output: output.into(),
            worker: None,
            file_change: None,
            streams: None,
            duration_ms: 0,
            call_id: None,
        })
    }

    fn draw(screen: &SessionScreen, w: u16, h: u16) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| screen.view(frame, frame.area()))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    /// Row text of the content pane only (sidebar is 30 cols + 1 gutter).
    fn content_row_text(buf: &ratatui::buffer::Buffer, y: u16) -> String {
        (31..buf.area().width)
            .map(|x| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn todo_tool_finish_fills_working_strip() {
        let mut screen = SessionScreen::new();
        screen.update(todo_finish(
            "Todos (0/2 done)\n  #1 [ ] plan it\n  #2 [~] write tests",
        ));
        assert_eq!(screen.working_todos.len(), 1);
        assert_eq!(screen.working_todos[0].id, 2);
        assert_eq!(screen.working_todos[0].text, "write tests");
    }

    #[test]
    fn todo_tool_finish_with_empty_list_clears_state() {
        let mut screen = SessionScreen::new();
        screen.update(todo_finish("Todos (0/1 done)\n  #1 [~] write tests"));
        screen.update(todo_finish("Removed #1\n\nTodos (none)"));
        assert!(screen.working_todos.is_empty());
        assert_eq!(
            (screen.sidebar.todos_done, screen.sidebar.todos_total),
            (0, 0)
        );
    }

    #[test]
    fn loaded_session_replays_working_todos() {
        let mut screen = SessionScreen::new();
        let mut session = shuvarie_core::Session::new();
        session.push_user("go");
        session.push_assistant("ok");
        session.tool_records = vec![todo_record(
            r#"{"op":"add","text":"write tests","status":"in_progress"}"#,
        )];
        screen.update(SessionMessage::Loaded {
            id: uuid::Uuid::new_v4(),
            title: "t".into(),
            session,
        });
        assert_eq!(screen.working_todos.len(), 1);
        assert_eq!(screen.working_todos[0].text, "write tests");
    }

    #[test]
    fn reset_clears_working_todos() {
        let mut screen = SessionScreen::new();
        screen.update(todo_finish("Todos (0/1 done)\n  #1 [~] write tests"));
        screen.update(SessionMessage::Reset);
        assert!(screen.working_todos.is_empty());
    }

    #[test]
    fn working_strip_renders_below_title_and_takes_layout_space() {
        let mut screen = SessionScreen::new();
        screen.session_title = Some("T".into());
        screen.chat.update(ChatMessage::BeginUserTurn {
            content: "first prompt".into(),
        });
        screen.update(todo_finish(
            "Todos (0/1 done)\n  #1 [~] refactor the parser",
        ));
        let buf = draw(&screen, 80, 24);
        assert!(row_text(&buf, 0).contains("Shuvarie · T"), "title row");
        let strip = row_text(&buf, 1);
        assert!(strip.contains("~ #1"), "strip row 1: {strip:?}");
        assert!(
            strip.contains("refactor the parser"),
            "strip row 1: {strip:?}"
        );
        let chat_row = row_text(&buf, 3);
        assert!(
            chat_row.contains("first prompt"),
            "history starts below the strip: {chat_row:?}"
        );
    }

    #[test]
    fn provider_model_renders_right_aligned_on_status_row() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::UpdateConfig {
            provider: Some("Anthropic".into()),
            model: Some("claude-sonnet-4-5".into()),
            context_length: Some(200_000),
        });
        assert_eq!(screen.provider.as_deref(), Some("Anthropic"));
        assert_eq!(screen.model.as_deref(), Some("claude-sonnet-4-5"));

        let buf = draw(&screen, 80, 24);
        let status_row = content_row_text(&buf, 22);
        assert!(
            status_row.ends_with("Anthropic:claude-sonnet-4-5"),
            "status row: {status_row:?}"
        );
        let sidebar_row = row_text(&buf, 2);
        assert!(
            !sidebar_row.contains("Anthropic"),
            "sidebar must not show the provider: {sidebar_row:?}"
        );
    }

    #[test]
    fn working_strip_hidden_when_nothing_in_progress() {
        let mut screen = SessionScreen::new();
        screen.chat.update(ChatMessage::BeginUserTurn {
            content: "first prompt".into(),
        });
        let buf = draw(&screen, 80, 24);
        assert!(
            content_row_text(&buf, 1).trim().is_empty(),
            "no strip row when no todos: {:?}",
            content_row_text(&buf, 1)
        );
        let chat_row = content_row_text(&buf, 2);
        assert!(
            chat_row.contains("first prompt"),
            "history starts right below the title: {chat_row:?}"
        );
    }

    #[test]
    fn working_strip_caps_rows_with_overflow_hint() {
        let mut screen = SessionScreen::new();
        let rows = (1..=4)
            .map(|i| format!("  #{i} [~] task number {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        screen.update(todo_finish(&format!("Todos (0/4 done)\n{rows}")));
        assert_eq!(screen.working_todos.len(), 4);
        let buf = draw(&screen, 80, 24);
        let strip = (1..5)
            .map(|y| row_text(&buf, y as u16))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            strip.contains("task number 3"),
            "capped at 3 items: {strip:?}"
        );
        assert!(
            !strip.contains("task number 4"),
            "capped at 3 items: {strip:?}"
        );
        assert!(strip.contains("+1 more in progress"), "overflow: {strip:?}");
    }

    #[test]
    fn working_strip_truncates_long_text() {
        let mut screen = SessionScreen::new();
        screen.update(todo_finish(
            "Todos (0/1 done)\n  #1 [~] a very long todo text that should be cut off before the end",
        ));
        let buf = draw(&screen, 60, 24);
        let strip = content_row_text(&buf, 1);
        assert!(strip.contains('…'), "elided: {strip:?}");
        assert!(
            !strip.contains("before the end"),
            "truncated to strip width: {strip:?}"
        );
    }

    fn retry_screen(seconds: u64, attempt: usize) -> SessionScreen {
        let mut screen = SessionScreen::new();
        screen.retry = Some(RetryWait {
            reason: "Connection reset".into(),
            attempt,
            max_attempts: 10,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(seconds),
        });
        screen
    }

    fn find_row(buf: &ratatui::buffer::Buffer, needle: &str) -> (u16, String) {
        (0..buf.area().height)
            .map(|y| (y, row_text(buf, y)))
            .find(|(_, text)| text.contains(needle))
            .unwrap_or_else(|| panic!("row containing {needle:?} should render"))
    }

    #[test]
    fn retry_countdown_renders_in_status_row() {
        let buf = draw(&retry_screen(3, 1), 100, 24);
        let (y, text) = find_row(&buf, "Retry in");
        assert!(
            text.contains("Connection reset. Retry in 3s [1/10]"),
            "row {y}: {text:?}"
        );
    }

    #[test]
    fn retry_countdown_ceils_remaining_seconds() {
        let buf = draw(&retry_screen(4, 2), 100, 24);
        let (_, text) = find_row(&buf, "Retry in");
        assert!(text.contains("Retry in 4s [2/10]"), "ceil: {text:?}");
    }

    #[test]
    fn retry_countdown_is_red() {
        let buf = draw(&retry_screen(3, 1), 100, 24);
        let (y, text) = find_row(&buf, "Retry in");
        let col = text.find("Connection reset").unwrap() as u16;
        assert_eq!(
            buf[(col, y)].fg,
            theme::ERROR,
            "countdown text renders in ERROR color"
        );
    }

    #[test]
    fn retry_scheduled_sets_busy_and_state() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::RetryScheduled {
            reason: "Connection timed out".into(),
            attempt: 3,
            max_attempts: 10,
            delay_ms: 10_000,
        });
        let retry = screen.retry.as_ref().expect("retry state");
        assert_eq!(retry.reason, "Connection timed out");
        assert_eq!(retry.attempt, 3);
        assert_eq!(retry.max_attempts, 10);
        assert!(screen.busy);
        assert_eq!(screen.busy_kind, BusyKind::Waiting);
    }

    #[test]
    fn compaction_events_drive_busy_state() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::Chat(ChatMessage::StreamError {
            error: "context budget exceeded — compacting session history and continuing".into(),
        }));
        assert!(!screen.busy);

        screen.update(SessionMessage::CompactionStarted);
        assert!(screen.busy);
        assert_eq!(screen.busy_kind, BusyKind::Waiting);
        assert_eq!(screen.status.as_deref(), Some("Compacting context..."));

        screen.update(SessionMessage::CompactionFinished);
        assert!(screen.busy);
        assert_eq!(screen.busy_kind, BusyKind::Generating);
        assert_eq!(
            screen.status.as_deref(),
            Some("Resuming after compaction...")
        );
    }

    #[test]
    fn compaction_busy_stays_armed_across_turn_reverted() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::CompactionStarted);
        screen.update(SessionMessage::CompactionFinished);

        screen.update(SessionMessage::TurnReverted {
            session: shuvarie_core::Session::new(),
        });
        assert!(screen.busy);
        assert_eq!(
            screen.status.as_deref(),
            Some("Resuming after compaction...")
        );
    }

    #[test]
    fn stream_activity_after_compaction_refreshes_status() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::CompactionStarted);
        screen.update(SessionMessage::CompactionFinished);

        screen.update(SessionMessage::Chat(ChatMessage::ToolStarted {
            name: "read_file".into(),
            args: serde_json::json!({}),
            worker: None,
            call_id: None,
        }));
        assert!(screen.busy);
        assert_eq!(screen.busy_kind, BusyKind::Tool);
        assert_eq!(screen.status.as_deref(), Some("Calling tool: read_file"));
    }

    #[test]
    fn stream_error_after_compaction_clears_busy() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::CompactionStarted);
        screen.update(SessionMessage::CompactionFinished);
        assert!(screen.busy);

        screen.update(SessionMessage::Chat(ChatMessage::StreamError {
            error: "provider unreachable".into(),
        }));
        assert!(!screen.busy);
        assert_eq!(
            screen.status.as_deref(),
            Some("error: provider unreachable")
        );
    }

    #[test]
    fn stream_error_clears_retry() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::RetryScheduled {
            reason: "Connection reset".into(),
            attempt: 1,
            max_attempts: 10,
            delay_ms: 3_000,
        });
        screen.update(SessionMessage::Chat(ChatMessage::StreamError {
            error: "Connection reset".into(),
        }));
        assert!(screen.retry.is_none());
        assert!(!screen.busy);
        assert_eq!(screen.status.as_deref(), Some("error: Connection reset"));
    }

    #[test]
    fn reset_and_turn_reverted_clear_retry() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::RetryScheduled {
            reason: "Connection reset".into(),
            attempt: 1,
            max_attempts: 10,
            delay_ms: 3_000,
        });
        screen.update(SessionMessage::Reset);
        assert!(screen.retry.is_none());

        screen.update(SessionMessage::RetryScheduled {
            reason: "Connection reset".into(),
            attempt: 2,
            max_attempts: 10,
            delay_ms: 5_000,
        });
        screen.update(SessionMessage::TurnReverted {
            session: shuvarie_core::Session::new(),
        });
        assert!(screen.retry.is_none());
    }

    fn queued(content: &str) -> SessionMessage {
        SessionMessage::Chat(ChatMessage::SteeredQueued {
            content: content.into(),
        })
    }

    #[test]
    fn alt_up_maps_recall_and_alt_shift_up_stacks() {
        let screen = SessionScreen::new();
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Up, Modifiers::ALT)),
            Some(SessionMessage::RecallSteered { stacked: false })
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::new(
                KeyCode::Up,
                Modifiers::ALT | Modifiers::SHIFT
            )),
            Some(SessionMessage::RecallSteered { stacked: true })
        ));
    }

    #[test]
    fn recall_steered_requests_recall_effect() {
        let mut screen = SessionScreen::new();
        assert!(matches!(
            screen.update(SessionMessage::RecallSteered { stacked: false }),
            Some(SessionEffect::RecallSteered { stacked: false })
        ));
    }

    #[test]
    fn steered_recall_overwrites_input() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("draft text");
        screen.update(queued("steered one"));
        screen.update(SessionMessage::SteeredRecalled {
            stacked: false,
            content: Some("steered one".into()),
        });
        assert_eq!(screen.input.buffer.value, "steered one");
        assert!(!screen.chat.has_steered());
    }

    #[test]
    fn steered_stacked_recall_prepends_with_blank_lines() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("draft text");
        screen.update(queued("first"));
        screen.update(queued("second"));
        screen.update(SessionMessage::SteeredRecalled {
            stacked: true,
            content: Some("first".into()),
        });
        assert_eq!(screen.input.buffer.value, "first\n\ndraft text");
        assert!(screen.chat.has_steered());

        screen.update(SessionMessage::SteeredRecalled {
            stacked: true,
            content: Some("second".into()),
        });
        assert_eq!(screen.input.buffer.value, "second\n\nfirst\n\ndraft text");
        assert!(!screen.chat.has_steered());
    }

    #[test]
    fn steered_stacked_recall_into_empty_input_has_no_leading_blank_lines() {
        let mut screen = SessionScreen::new();
        screen.update(queued("solo"));
        screen.update(SessionMessage::SteeredRecalled {
            stacked: true,
            content: Some("solo".into()),
        });
        assert_eq!(screen.input.buffer.value, "solo");
    }

    #[test]
    fn steered_recall_with_empty_queue_is_a_noop() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("draft text");
        screen.update(SessionMessage::SteeredRecalled {
            stacked: false,
            content: None,
        });
        assert_eq!(screen.input.buffer.value, "draft text");
    }

    #[test]
    fn turn_started_renders_user_turn_and_busy() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::TurnStarted {
            content: "hello".into(),
            steered: false,
        });
        assert!(screen.busy);
        assert_eq!(screen.busy_kind, BusyKind::Generating);
        assert_eq!(screen.status.as_deref(), Some("Thinking..."));
        assert!(screen.chat.has_messages());
    }

    #[test]
    fn turn_started_steered_drops_first_queued_entry() {
        let mut screen = SessionScreen::new();
        screen.update(queued("one"));
        screen.update(queued("two"));
        screen.update(SessionMessage::TurnStarted {
            content: "one".into(),
            steered: true,
        });
        assert!(screen.chat.has_steered(), "one entry should remain");
        assert!(screen.busy);
    }

    #[test]
    fn steered_cleared_event_empties_display() {
        let mut screen = SessionScreen::new();
        screen.update(queued("one"));
        screen.update(queued("two"));
        screen.update(SessionMessage::Chat(ChatMessage::SteeredCleared));
        assert!(!screen.chat.has_steered());
    }

    #[test]
    fn steered_prompts_render_in_chat_pane() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::Chat(ChatMessage::BeginUserTurn {
            content: "running task".into(),
        }));
        screen.update(SessionMessage::Chat(ChatMessage::SteeredQueued {
            content: "queued prompt".into(),
        }));
        let buf = draw(&screen, 100, 24);
        let (_, header) = find_row(&buf, "steered");
        assert!(
            header.contains("sends after the current action"),
            "header: {header:?}"
        );
        let (_, body) = find_row(&buf, "queued prompt");
        assert!(!body.is_empty());
    }

    #[test]
    fn footer_hints_recall_when_steered_present() {
        let mut screen = SessionScreen::new();
        let buf = draw(&screen, 100, 24);
        assert!(!row_text(&buf, buf.area().height - 1).contains("recall"));

        screen.update(queued("one"));
        let buf = draw(&screen, 100, 24);
        let (_, footer) = find_row(&buf, "recall steered");
        assert!(footer.contains("Alt+↑"), "footer: {footer:?}");
    }
}
