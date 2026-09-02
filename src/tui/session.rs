use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    pub session_id: Option<uuid::Uuid>,
    pub session_title: Option<String>,
    pub error: Option<String>,
    working_todos: Vec<shuvarie_core::todos::TodoItem>,
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
            working_todos: Vec::new(),
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
                self.sync_todos(Vec::new());
                None
            }
            SessionMessage::Loaded { id, title, session } => {
                let (usage, cost) = usage_of(&session);
                self.session_id = Some(id);
                self.session_title = Some(title);
                self.status = None;
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sync_todos(shuvarie_core::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::Load { session });
                None
            }
            SessionMessage::TurnReverted { session } => {
                let (usage, cost) = usage_of(&session);
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sync_todos(shuvarie_core::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::TurnReverted { session });
                None
            }
            SessionMessage::TurnRestored { session } => {
                let (usage, cost) = usage_of(&session);
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sync_todos(shuvarie_core::todos::replay(&session.tool_records));
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

    /// Apply a full todo list: sidebar counts plus the working-items strip
    /// (the in-progress items shown under the title bar).
    fn sync_todos(&mut self, items: Vec<shuvarie_core::todos::TodoItem>) {
        let (done, total) = shuvarie_core::todos::done_total(&items);
        self.sidebar
            .update(SidebarMessage::SetTodos { done, total });
        self.working_todos = items
            .into_iter()
            .filter(|item| item.status == shuvarie_core::todos::TodoStatus::InProgress)
            .collect();
    }

    /// Parse the finished `todo` tool call's list output. A successful call
    /// always carries the full list, so no item rows means the list is empty
    /// (`Todos (none)`).
    fn sync_todo_output(&mut self, output: &str) {
        self.sync_todos(shuvarie_core::todos::parse_items(output).unwrap_or_default());
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

/// Rows the working-todos strip occupies below the title bar: one per
/// in-progress item up to [`MAX_WORKING_ROWS`], plus an overflow hint row.
fn working_rows(todos: &[shuvarie_core::todos::TodoItem]) -> u16 {
    let n = todos.len();
    if n == 0 {
        0
    } else {
        (n.min(MAX_WORKING_ROWS) + usize::from(n > MAX_WORKING_ROWS)) as u16
    }
}

/// The strip's lines: `~ #id text` per in-progress todo, truncated to the
/// strip width, then an overflow hint when more items are working.
fn working_todo_lines(todos: &[shuvarie_core::todos::TodoItem], width: u16) -> Vec<Line<'static>> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use shuvarie_core::tool_record::ToolRecord;

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
}
