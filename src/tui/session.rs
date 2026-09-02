use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::commands::{self, CommandAction};
use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::question::{QuestionEffect, QuestionMessage, QuestionUI};
use super::sidebar::{Sidebar, SidebarMessage};
use super::slash::{SlashMenu, SlashMessage};
use super::theme;

pub mod blocks;
pub mod chat;
pub mod segment;

pub use chat::ChatMessage;

pub enum SessionMessage {
    Text(TextAreaMessage),
    Chat(ChatMessage),
    CancelRequested,
    ShowError {
        error: String,
    },
    ClearError,
    UsageUpdate {
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyKind {
    Generating,
    Tool,
}

/// The session screen: sidebar, chat history pane (a [`chat::Chat`] TEA
/// model), input, question prompt, slash menu, status row, and footer.
pub struct SessionScreen {
    pub input: TextArea,
    pub question: QuestionUI,
    slash: SlashMenu,
    pub chat: chat::Chat,
    pub busy: bool,
    busy_kind: BusyKind,
    pub status: Option<String>,
    pub sidebar: Sidebar,
    pub session_id: Option<uuid::Uuid>,
    pub session_title: Option<String>,
    pub error: Option<String>,
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
            session_id: None,
            session_title: None,
            error: None,
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.chat.is_streaming()
    }

    pub fn has_messages(&self) -> bool {
        self.chat.has_messages()
    }

    pub fn is_interrupted(&self) -> bool {
        self.chat.is_interrupted()
    }

    /// Mark the chat dirty so an animated spinner re-renders.
    pub fn mark_spinner_dirty(&self) {
        self.chat.mark_spinner_dirty();
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
        let interrupted = self.chat.is_interrupted() && !self.chat.is_streaming();
        self.slash
            .set_availability(CommandAction::UndoLastTurn, has_messages);
        self.slash
            .set_availability(CommandAction::Redo, has_messages);
        self.slash
            .set_availability(CommandAction::Replay, has_messages);
        self.slash
            .set_availability(CommandAction::Resume, interrupted);
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
                            let content = commands::unescape(&content).to_string();
                            self.chat.update(ChatMessage::BeginUserTurn {
                                content: content.clone(),
                            });
                            self.busy = true;
                            self.busy_kind = BusyKind::Generating;
                            self.status = Some("Thinking...".to_string());
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
            SessionMessage::UsageUpdate { usage, cost } => {
                self.sidebar
                    .update(SidebarMessage::UpdateUsage { usage, cost });
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
            SessionMessage::Reset => {
                self.chat.update(ChatMessage::Reset);
                self.busy = false;
                self.busy_kind = BusyKind::Generating;
                self.status = None;
                self.session_id = None;
                self.session_title = None;
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage::default(),
                    cost: 0.0,
                });
                None
            }
            SessionMessage::Loaded { id, title, session } => {
                let (usage, cost) = usage_of(&session);
                self.session_id = Some(id);
                self.session_title = Some(title);
                self.status = None;
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.chat.update(ChatMessage::Load { session });
                None
            }
            SessionMessage::TurnReverted { session } => {
                let (usage, cost) = usage_of(&session);
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.chat.update(ChatMessage::TurnReverted { session });
                None
            }
            SessionMessage::TurnRestored { session } => {
                let (usage, cost) = usage_of(&session);
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.chat.update(ChatMessage::TurnRestored { session });
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
                self.status = Some("Streaming...".to_string());
            }
            ChatMessage::ReasoningReceived { .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Thinking...".to_string());
            }
            ChatMessage::ToolStarted { name, .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Tool;
                self.status = Some(format!("Calling tool: {name}"));
            }
            ChatMessage::WorkerStarted { name, .. } => {
                self.busy = true;
                self.busy_kind = BusyKind::Tool;
                self.status = Some(format!("Spawned worker: {name}"));
            }
            ChatMessage::ToolFinished { .. } | ChatMessage::WorkerFinished { .. } => {
                self.status = None;
            }
            ChatMessage::StreamDone => {
                self.busy = false;
                self.status = None;
            }
            ChatMessage::StreamError { error } => {
                self.busy = false;
                self.status = Some(format!("error: {error}"));
            }
            ChatMessage::StreamCancelled => {
                self.busy = false;
                self.status = None;
            }
            _ => {}
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
        let [
            title_area,
            history_area,
            input_area,
            status_area,
            footer_area,
        ] = Layout::vertical([
            Length(1),
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

        let status = match self.status.as_deref() {
            Some(status) => Some((status, self.busy_kind)),
            None if self.busy => Some(("Working...", BusyKind::Tool)),
            None => None,
        };
        if let Some((status, kind)) = status {
            let mut spans = Vec::new();
            if self.busy {
                spans.push(match kind {
                    BusyKind::Generating => super::spinner::generating_spinner(),
                    BusyKind::Tool => super::spinner::tool_spinner(),
                });
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(status).fg(theme::TEXT_MUTED));
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        }

        if let Some(error) = &self.error {
            frame.render_widget(Paragraph::new(error.as_str()).fg(theme::ERROR), footer_area);
        } else {
            let footer = if !self.question.open && self.slash.active() {
                theme::help_line(&[("Tab", "complete"), ("↑↓", "select"), ("Esc", "dismiss")])
            } else if self.chat.is_streaming() {
                theme::help_line(&[("Ctrl+C", "stop"), ("Ctrl+M", "commands")])
            } else if self.chat.is_interrupted() {
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
}

fn usage_of(session: &shuvarie_core::Session) -> (TokenUsage, f64) {
    (
        TokenUsage {
            input_tokens: session.input_tokens,
            output_tokens: session.output_tokens,
            total_tokens: session.tokens,
            cached_input_tokens: session.cached_tokens,
            reasoning_tokens: session.reasoning_tokens,
            ..Default::default()
        },
        session.cost,
    )
}

impl Default for SessionScreen {
    fn default() -> Self {
        Self::new()
    }
}
