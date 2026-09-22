use std::cell::Cell;
use std::time::{Duration, Instant};

use ratatui::layout::{Alignment, Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_db::StoredScroll;
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent, KeyEventKind};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::tui::utils::{alt, alt_shift, ctrl};

use super::commands::{self, CommandAction, CommandRef};
use super::components::{TextArea, TextAreaEffect, TextAreaMessage};
use super::permission::{PermissionEffect, PermissionMessage, PermissionUI};
use super::question::{QuestionEffect, QuestionMessage, QuestionUI};
use super::sidebar::{Sidebar, SidebarMessage};
use super::slash::{SlashMenu, SlashMessage};
use super::spinner::SpinnerKind;
use super::theme;
use shuvarie_core::Skill;

pub mod bash;
pub mod blocks;
pub mod chat;
pub mod md_cache;
pub mod search;
pub mod segment;
pub mod tree;
pub mod virtualizer;

pub use bash::BashMessage;
pub use chat::ChatMessage;
pub use search::{SearchMessage, SearchPrompt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Down,
    Drag,
    Up,
}

pub enum SessionMessage {
    Text(TextAreaMessage),
    Chat(ChatMessage),
    /// A tool call paused on an `ask` permission rule; `id` routes the
    /// decision back through [`SessionEffect::PermissionDecide`].
    PermissionRequested {
        id: u64,
        description: String,
        allow_session: bool,
    },
    Permission(PermissionMessage),
    /// Left-button mouse activity at a terminal cell. Routed by zone: the
    /// input area, the bash popup (swallowed), then the chat history pane.
    Mouse {
        kind: MouseKind,
        column: u16,
        row: u16,
    },
    /// A mouse-wheel scroll at a terminal cell. Routed by zone: the bash
    /// popup scrolls its own output, the chat history pane scrolls itself.
    Wheel {
        up: bool,
        column: u16,
        row: u16,
    },
    CopySelection,
    CutSelection,
    CancelRequested,
    EscapePressed,
    ShowError {
        error: String,
    },
    ClearError,
    /// A transient informational status line in the status row (e.g. the
    /// `/export` destination path); the next busy-state event replaces it.
    ShowStatus {
        status: String,
    },
    /// Replaces the session's skill list (mirrors what the sidebar shows).
    SetSkills {
        skills: Vec<Skill>,
    },
    /// Replaces the session's custom-command list; the slash menu mirrors it.
    SetCustomCommands {
        commands: Vec<shuvarie_core::CustomCommand>,
    },
    /// Run a custom command picked from the Ctrl+M menu: expand its template
    /// (menu launches carry no arguments) and send it.
    RunCustomCommand {
        name: String,
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
        variant: Option<String>,
        context_length: Option<u64>,
    },
    QuestionAsked {
        id: u64,
        questions: Vec<shuvarie_core::QuestionPrompt>,
    },
    Question(QuestionMessage),
    Slash(SlashMessage),
    /// Sidebar control: collapse toggle (Ctrl+W) and pref/width updates.
    Sidebar(SidebarMessage),
    /// `[ui] copy-on-select`: copy the chat selection on mouse-up.
    SetCopyOnSelect {
        enabled: bool,
    },
    /// A bash-mode (`!`) run's display-only popup: start, live output,
    /// finish, or Escape dismissal.
    Bash(bash::BashMessage),
    /// Chat search mode: edits in the floating tooltip update the live
    /// highlight needle; `SearchMessage::Exit` closes the mode.
    Search(SearchMessage),
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
    /// The session forked: the active path ends at a different node now
    /// (an `/undo` fork or a `/tree` fork); the chat pane rebuilds from the
    /// reloaded session and the forked-away prompt may be recalled into the
    /// input.
    Forked {
        session: shuvarie_core::Session,
        /// The forked-away user prompt, recalled into the input area; `None`
        /// when the fork re-sends automatically (replay / retry resume).
        prompt: Option<String>,
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
    /// The render loop's spinner wake: refresh the animated spinner renders
    /// (the chat's cached turns and the sidebar's cached lines).
    SpinnerUpdate,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum BusyKind {
    #[default]
    Idle,
    Generating,
    Tool,
    Waiting,
}

/// A scheduled turn retry, rendered as a red countdown in the status
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

const DOUBLE_ESCAPE_WINDOW: Duration = Duration::from_millis(500);

/// Minimum columns left for the workspace path+branch on the collapsed
/// footer line; below it the hints keep the whole row.
const MIN_WORKSPACE_COLS: usize = 12;

/// Columns left for the workspace path+branch on the collapsed footer line,
/// after the key hints and a two-column gap; `None` when that leaves less
/// than [`MIN_WORKSPACE_COLS`].
fn workspace_footer_budget(footer_width: u16, hints_width: usize) -> Option<usize> {
    let budget = usize::from(footer_width).saturating_sub(hints_width + 2);
    (budget >= MIN_WORKSPACE_COLS).then_some(budget)
}

/// The session screen: sidebar, title bar with the working-todos strip, chat
/// history pane (a [`chat::Chat`] TEA model), input, question prompt, slash
/// menu, status row, and footer.
pub struct SessionScreen {
    pub input: TextArea,
    pub question: QuestionUI,
    pub permission: PermissionUI,
    slash: SlashMenu,
    pub chat: chat::Chat,
    /// Floating display-only window for the latest bash-mode run.
    pub bash: bash::BashPopup,
    /// Chat search mode: the floating needle tooltip and its state.
    pub search: SearchPrompt,
    busy_kind: BusyKind,
    pub status: Option<String>,
    pub sidebar: Sidebar,
    provider: Option<String>,
    model: Option<String>,
    variant: Option<String>,
    pub session_id: Option<uuid::Uuid>,
    pub session_title: Option<String>,
    pub error: Option<String>,
    pub(crate) retry: Option<RetryWait>,
    last_escape: Option<Instant>,
    working_todos: Vec<shuvarie_core::tools::todos::TodoItem>,
    skills: Vec<Skill>,
    custom_commands: Vec<shuvarie_core::CustomCommand>,
    /// Last painted layout rects, for mouse zone routing between frames.
    input_area: Cell<Rect>,
    history_area: Cell<Rect>,
    /// Copy the chat selection on mouse-up (`[ui] copy-on-select`).
    copy_on_select: bool,
}

impl SessionScreen {
    pub fn new() -> Self {
        Self {
            input: TextArea::with_max_height("Type a message", 8),
            question: QuestionUI::new(),
            permission: PermissionUI::new(),
            slash: SlashMenu::new(),
            chat: chat::Chat::new(),
            bash: bash::BashPopup::new(),
            search: SearchPrompt::new(),
            busy_kind: BusyKind::Idle,
            status: None,
            sidebar: Sidebar::new(),
            provider: None,
            model: None,
            variant: None,
            session_id: None,
            session_title: None,
            error: None,
            retry: None,
            last_escape: None,
            working_todos: Vec::new(),
            skills: Vec::new(),
            custom_commands: Vec::new(),
            input_area: Cell::new(Rect::default()),
            history_area: Cell::new(Rect::default()),
            copy_on_select: false,
        }
    }

    pub fn is_streaming(&self) -> bool {
        self.chat.is_streaming()
    }

    pub fn is_busy(&self) -> bool {
        self.busy_kind != BusyKind::Idle
    }

    /// The wide status-row spinner currently animating, if any.
    pub(crate) fn busy_spinner(&self) -> Option<SpinnerKind> {
        match self.busy_kind {
            BusyKind::Idle => None,
            BusyKind::Generating => Some(SpinnerKind::Generating),
            BusyKind::Tool => Some(SpinnerKind::Tool),
            BusyKind::Waiting => Some(SpinnerKind::Waiting),
        }
    }

    pub fn has_messages(&self) -> bool {
        self.chat.has_messages()
    }

    /// Whether the visible history contains at least one user prompt (drives
    /// the `gen-title` availability).
    pub fn has_user_turn(&self) -> bool {
        self.chat.has_user_turn()
    }

    /// The session to leave and its persisted chat scroll position; `None`
    /// when no session row is loaded yet, so there is nothing to save.
    pub fn scroll_save(&self) -> Option<(uuid::Uuid, StoredScroll)> {
        Some((self.session_id?, self.chat.scroll_save()))
    }

    /// Bash mode: the prompt starts with `!`, so a submit runs the rest as a
    /// local shell command instead of prompting the agent.
    pub fn is_bash_mode(&self) -> bool {
        self.input.buffer.value.starts_with('!')
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

    /// Expand a `/<name> [args]` submit into the custom command's prompt
    /// content plus its `model` override. `Ok(None)` when the text is not a
    /// custom invocation (no custom command claims the name, so the submit
    /// falls through to skill parsing and a literal send); `Err(reason)` for
    /// an unreadable `command.md`.
    fn expand_custom(&self, content: &str) -> Result<Option<(String, Option<String>)>, String> {
        let Some(invocation) = commands::parse_custom_invocation(content) else {
            return Ok(None);
        };
        let Some(command) = self
            .custom_commands
            .iter()
            .find(|c| c.name == invocation.name)
        else {
            return Ok(None);
        };
        command
            .invocation_content(invocation.args.as_deref())
            .map(|content| Some((content, command.model.clone())))
            .map_err(|e| format!("failed to read command {}: {e}", invocation.name))
    }

    /// Run a slash-menu / command-menu selection: builtins dispatch through
    /// [`SessionEffect::RunCommand`]; custom commands expand immediately
    /// (menu launches carry no args).
    fn run_command_ref(&mut self, action: CommandRef) -> Option<SessionEffect> {
        match action {
            CommandRef::Builtin(action) => Some(SessionEffect::RunCommand {
                action: CommandRef::Builtin(action),
                args: None,
            }),
            CommandRef::Custom { name, model: _ } => self.run_custom(&name, None),
        }
    }

    /// Expand and send a custom command by name; `args` feed the
    /// `{{arguments}}` placeholder (or trail the template).
    fn run_custom(&mut self, name: &str, args: Option<String>) -> Option<SessionEffect> {
        let Some(command) = self.custom_commands.iter().find(|c| c.name == name) else {
            self.error = Some(format!("unknown command: {name}"));
            return None;
        };
        match command.invocation_content(args.as_deref()) {
            Ok(content) => Some(SessionEffect::SendMessage {
                content,
                model: command.model.clone(),
            }),
            Err(error) => {
                self.error = Some(format!("failed to read command {name}: {error}"));
                None
            }
        }
    }

    /// Enter chat search mode: open the floating tooltip seeded with
    /// `initial` (the `/search <args>` form; `None` reopens with the previous
    /// text) and highlight it in the chat right away.
    pub fn open_search(&mut self, initial: Option<&str>) {
        self.search.open(initial);
        self.chat.set_search(self.search.needle());
    }

    /// Leave chat search mode and drop the live highlight (Escape, or the
    /// session changing underneath).
    fn close_search(&mut self) {
        self.search.close();
        self.chat.set_search(None);
    }

    /// Leave chat search mode on a session change: also drop the remembered
    /// needle text so the next `/search` starts clean.
    fn reset_search(&mut self) {
        self.search.reset();
        self.chat.set_search(None);
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SessionMessage> {
        if self.permission.open {
            return self
                .permission
                .map_event(key)
                .map(SessionMessage::Permission);
        }
        if self.question.open {
            return self.question.map_event(key).map(SessionMessage::Question);
        }
        // The search tooltip floats above the chat; while it is open keys
        // edit the needle (Escape exits) before anything underneath sees
        // them. Scroll keys fall through to the chat so matches can be
        // browsed while highlighting stays live.
        if self.search.is_open() {
            if let Some(m) = self.search.map_event(key) {
                return Some(SessionMessage::Search(m));
            }
            return match key.code {
                KeyCode::Up => Some(SessionMessage::Chat(ChatMessage::ScrollUp)),
                KeyCode::Down => Some(SessionMessage::Chat(ChatMessage::ScrollDown)),
                KeyCode::Char('n') if ctrl(key) => {
                    Some(SessionMessage::Chat(ChatMessage::ScrollDown))
                }
                KeyCode::Char('p') if ctrl(key) => {
                    Some(SessionMessage::Chat(ChatMessage::ScrollUp))
                }
                _ => Some(SessionMessage::Search(SearchMessage::Swallow)),
            };
        }
        // The bash popup floats above the chat; Escape dismisses it before
        // the event reaches anything underneath.
        if let Some(m) = self.bash.map_event(key) {
            return Some(SessionMessage::Bash(m));
        }
        if self.slash.active()
            && let Some(m) = self.slash.map_event(key)
        {
            return Some(SessionMessage::Slash(m));
        }
        if ctrl(key) {
            if let Some(m) = self.sidebar.map_event(key) {
                return Some(SessionMessage::Sidebar(m));
            }
            return match key.code {
                KeyCode::Char('n') => {
                    if self.input.wants_recall_down() {
                        Some(SessionMessage::Text(TextAreaMessage::CursorDown))
                    } else {
                        Some(SessionMessage::Chat(ChatMessage::ScrollDown))
                    }
                }
                KeyCode::Char('p') => {
                    if self.input.wants_recall_up() {
                        Some(SessionMessage::Text(TextAreaMessage::CursorUp))
                    } else {
                        Some(SessionMessage::Chat(ChatMessage::ScrollUp))
                    }
                }
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
            KeyCode::Up if self.input_is_multiline() || self.input.wants_recall_up() => {
                self.input.map_event(key).map(SessionMessage::Text)
            }
            KeyCode::Down if self.input_is_multiline() || self.input.wants_recall_down() => {
                self.input.map_event(key).map(SessionMessage::Text)
            }
            KeyCode::Up => Some(SessionMessage::Chat(ChatMessage::ScrollUp)),
            KeyCode::Down => Some(SessionMessage::Chat(ChatMessage::ScrollDown)),
            KeyCode::Escape if key.kind == KeyEventKind::Press => {
                if self.chat.is_streaming()
                    && self
                        .last_escape
                        .is_some_and(|at| at.elapsed() <= DOUBLE_ESCAPE_WINDOW)
                {
                    return Some(SessionMessage::CancelRequested);
                }
                Some(SessionMessage::EscapePressed)
            }
            _ => self.input.map_event(key).map(SessionMessage::Text),
        }
    }

    fn input_is_multiline(&self) -> bool {
        let width = self.input.width.get().max(1);
        let inner_w = width.saturating_sub(4).max(1);
        self.input.buffer.row_count(inner_w) > 1
    }

    /// Bracketed-paste routing, mirroring `map_event`'s popup priority. A
    /// question popup captures the paste for its custom-answer field; search
    /// mode captures it for the needle; the bash popup and slash menu leave
    /// the paste to the input area.
    pub fn map_paste(&self, text: &str) -> Option<SessionMessage> {
        if self.permission.open {
            return None;
        }
        if self.question.open {
            return Some(SessionMessage::Question(QuestionMessage::CustomPaste(
                text.to_string(),
            )));
        }
        if self.search.is_open() {
            return Some(SessionMessage::Search(SearchMessage::Paste(
                text.to_string(),
            )));
        }
        Some(SessionMessage::Text(TextAreaMessage::Paste(
            text.to_string(),
        )))
    }

    fn sync_slash(&mut self) {
        let has_messages = self.chat.has_messages();
        self.slash
            .set_availability(CommandAction::UndoLastTurn, has_messages);
        self.slash
            .set_availability(CommandAction::Replay, has_messages);
        self.slash
            .set_availability(CommandAction::OpenTree, self.session_id.is_some());
        self.slash
            .set_availability(CommandAction::OpenScenePicker, !self.chat.is_streaming());
        self.slash
            .set_availability(CommandAction::EditTitle, self.session_id.is_some());
        self.slash.set_availability(
            CommandAction::GenTitle,
            self.session_id.is_some() && self.chat.has_user_turn(),
        );
        self.slash
            .set_availability(CommandAction::Export, self.session_id.is_some());
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
                            if let Some(raw) = content.strip_prefix('!') {
                                let command = raw.trim().to_string();
                                self.sync_slash();
                                if command.is_empty() {
                                    return None;
                                }
                                return Some(SessionEffect::RunBash { command });
                            }
                            if let Some(cmd) = commands::parse_command(&content) {
                                self.sync_slash();
                                return Some(SessionEffect::RunCommand {
                                    action: CommandRef::Builtin(cmd.action),
                                    args: cmd.args,
                                });
                            }
                            // A custom command: expand its template (the
                            // `{{arguments}}` placeholder / trailing args)
                            // and send it, carrying the command's `model`
                            // override for this turn. An unknown name falls
                            // through to skill parsing and then to a literal
                            // send.
                            match self.expand_custom(&content) {
                                Ok(Some((expanded, model))) => {
                                    self.input.remember_sent(&content);
                                    self.sync_slash();
                                    return Some(SessionEffect::SendMessage {
                                        content: expanded,
                                        model,
                                    });
                                }
                                Ok(None) => {}
                                Err(error) => {
                                    self.error = Some(error);
                                    return None;
                                }
                            }
                            let expanded = match self.expand_skill(&content) {
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
                            self.input.remember_sent(&content);
                            self.sync_slash();
                            return Some(SessionEffect::SendMessage {
                                content: expanded,
                                model: None,
                            });
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
            SessionMessage::Mouse { kind, column, row } => self.handle_mouse(kind, column, row),
            SessionMessage::Wheel { up, column, row } => {
                let input_area = self.input_area.get();
                let history_area = self.history_area.get();
                let over_popup = self
                    .bash
                    .rect_for(history_area, input_area)
                    .is_some_and(|rect| rect.contains(ratatui::layout::Position::new(column, row)));
                if over_popup {
                    self.bash.update(if up {
                        BashMessage::ScrollUp
                    } else {
                        BashMessage::ScrollDown
                    });
                } else {
                    self.chat.update(ChatMessage::Wheel { up, column, row });
                }
                None
            }
            SessionMessage::CopySelection => {
                if let Some(text) = self.input.buffer.selected_text() {
                    return Some(SessionEffect::CopyToClipboard { text });
                }
                if let Some(text) = self.chat.selected_text() {
                    return Some(SessionEffect::CopyToClipboard { text });
                }
                None
            }
            SessionMessage::CutSelection => {
                if let Some(text) = self.input.buffer.selected_text() {
                    self.input.buffer.delete_selection();
                    return Some(SessionEffect::CopyToClipboard { text });
                }
                None
            }
            SessionMessage::Bash(m) => {
                self.bash.update(m);
                None
            }
            SessionMessage::Search(SearchMessage::Exit) => {
                self.close_search();
                None
            }
            SessionMessage::Search(m) => {
                if self.search.update(m) {
                    self.chat.set_search(self.search.needle());
                }
                None
            }
            SessionMessage::Sidebar(m) => {
                self.sidebar.update(m);
                None
            }
            SessionMessage::SetCopyOnSelect { enabled } => {
                self.copy_on_select = enabled;
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
                        let text =
                            format!("{}{} ", self.slash.trigger_char(), action.slash_alias());
                        self.input.buffer.set(&text);
                    }
                    self.sync_slash();
                    None
                }
                SlashMessage::Run => {
                    if let Some(action) = self.slash.selected_action() {
                        self.input.buffer.clear();
                        self.sync_slash();
                        return self.run_command_ref(action);
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
                    self.last_escape = None;
                    return Some(SessionEffect::CancelStream);
                }
                None
            }
            SessionMessage::EscapePressed => {
                if self.chat.is_streaming() {
                    self.last_escape = Some(Instant::now());
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
            SessionMessage::ShowStatus { status } => {
                self.status = Some(status);
                None
            }
            SessionMessage::SetSkills { skills } => {
                self.skills = skills;
                None
            }
            SessionMessage::SetCustomCommands { commands } => {
                self.custom_commands = commands.clone();
                self.slash.set_custom_commands(&commands);
                None
            }
            SessionMessage::RunCustomCommand { name } => self.run_custom(&name, None),
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
                variant,
                context_length,
            } => {
                self.provider = provider;
                self.model = model;
                self.variant = variant;
                self.sidebar
                    .update(SidebarMessage::UpdateConfig { context_length });
                None
            }
            SessionMessage::QuestionAsked { id, questions } => {
                self.question.open(id, questions);
                self.busy_kind = BusyKind::Waiting;
                self.status = Some("Waiting for answer...".to_string());
                None
            }
            SessionMessage::PermissionRequested {
                id,
                description,
                allow_session,
            } => {
                self.permission.open(id, description, allow_session);
                self.busy_kind = BusyKind::Waiting;
                self.status = Some("Waiting for permission...".to_string());
                None
            }
            SessionMessage::Permission(m) => {
                if let Some(effect) = self.permission.update(m) {
                    match effect {
                        PermissionEffect::Decide { id, decision } => {
                            self.permission.close();
                            self.busy_kind = BusyKind::Tool;
                            self.status = Some("Calling tool".to_string());
                            return Some(SessionEffect::PermissionDecide { id, decision });
                        }
                    }
                }
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
                self.busy_kind = BusyKind::Waiting;
                self.status = None;
                None
            }
            SessionMessage::CompactionStarted => {
                self.busy_kind = BusyKind::Waiting;
                self.status = Some("Compacting context...".to_string());
                self.retry = None;
                // The post-compaction context is unknown until the replay's
                // next response; the stale pre-compaction footprint would
                // overstate occupancy.
                self.sidebar
                    .update(SidebarMessage::SetContextRequest { usage: None });
                None
            }
            SessionMessage::CompactionFinished => {
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Resuming after compaction...".to_string());
                self.retry = None;
                None
            }
            SessionMessage::Reset => {
                self.chat.update(ChatMessage::Reset);
                self.reset_search();
                self.busy_kind = BusyKind::Idle;
                self.status = None;
                self.retry = None;
                self.last_escape = None;
                self.session_id = None;
                self.session_title = None;
                self.permission.close();
                self.sidebar.update(SidebarMessage::SetUsage {
                    usage: TokenUsage::default(),
                    cost: 0.0,
                });
                self.sidebar
                    .update(SidebarMessage::SetContextRequest { usage: None });
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
                self.last_escape = None;
                self.reset_search();
                self.permission.close();
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sidebar.update(SidebarMessage::SetContextRequest {
                    usage: session.last_usage,
                });
                self.sync_todos(shuvarie_core::tools::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::Load { session });
                None
            }
            SessionMessage::Forked { session, prompt } => {
                let usage = session.usage();
                let cost = session.cost;
                self.retry = None;
                self.last_escape = None;
                self.reset_search();
                if prompt.is_some() {
                    self.busy_kind = BusyKind::Idle;
                    self.status = None;
                }
                self.sidebar
                    .update(SidebarMessage::SetUsage { usage, cost });
                self.sidebar.update(SidebarMessage::SetContextRequest {
                    usage: session.last_usage,
                });
                self.sync_todos(shuvarie_core::tools::todos::replay(&session.tool_records));
                self.chat.update(ChatMessage::Forked { session });
                if let Some(prompt) = prompt {
                    self.input.stash_draft();
                    self.input.buffer.set(&prompt);
                    self.sync_slash();
                }
                None
            }
            SessionMessage::TurnStarted { content, steered } => {
                // The start of a turn re-reads the git branch, so checkouts
                // made between turns (e.g. in another terminal) update the
                // sidebar and the collapsed footer.
                self.sidebar.update(SidebarMessage::RefreshBranch);
                if steered {
                    self.chat.update(ChatMessage::SteeredDispatched);
                }
                self.chat.update(ChatMessage::BeginUserTurn { content });
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Thinking...".to_string());
                self.retry = None;
                self.last_escape = None;
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
                    self.input.stash_draft();
                    self.input.buffer.set(&content);
                }
                self.sync_slash();
                None
            }
            SessionMessage::SpinnerUpdate => {
                self.chat.update(ChatMessage::SpinnerUpdate);
                self.sidebar.update(SidebarMessage::SpinnerUpdate);
                None
            }
        }
    }

    /// Mirror the streaming lifecycle onto the screen's busy/status row while
    /// the chat model owns the rendering-side state.
    fn observe_chat(&mut self, msg: &ChatMessage) {
        match msg {
            ChatMessage::TokenReceived { .. } => {
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Yappin'...".to_string());
                self.retry = None;
            }
            ChatMessage::ReasoningReceived { .. } => {
                self.busy_kind = BusyKind::Generating;
                self.status = Some("Thinking...".to_string());
                self.retry = None;
            }
            ChatMessage::ToolStarted { name, .. } => {
                self.busy_kind = BusyKind::Tool;
                self.status = Some(format!("Calling tool: {name}"));
                self.retry = None;
            }
            ChatMessage::WorkerStarted { name, .. } => {
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
                self.busy_kind = BusyKind::Idle;
                self.status = None;
                self.retry = None;
                self.last_escape = None;
            }
            ChatMessage::StreamError { error } => {
                self.busy_kind = BusyKind::Idle;
                self.status = Some(format!("error: {error}"));
                self.retry = None;
                self.last_escape = None;
            }
            ChatMessage::StreamCancelled => {
                self.busy_kind = BusyKind::Idle;
                self.status = None;
                self.retry = None;
                self.last_escape = None;
                self.permission.close();
                self.question.close();
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

    /// Route a left-button mouse event by zone: the input area feeds the
    /// text area, the bash popup swallows (its content is display-only),
    /// everything else inside the chat pane goes to the chat model.
    fn handle_mouse(&mut self, kind: MouseKind, column: u16, row: u16) -> Option<SessionEffect> {
        let input_area = self.input_area.get();
        if input_area.contains(ratatui::layout::Position::new(column, row)) {
            if !self.question.open && !self.permission.open {
                let msg = match kind {
                    MouseKind::Down => TextAreaMessage::MouseDown { column, row },
                    MouseKind::Drag => TextAreaMessage::MouseDrag { column, row },
                    MouseKind::Up => TextAreaMessage::MouseUp,
                };
                self.input.update(msg);
            }
            return None;
        }
        let history_area = self.history_area.get();
        if let Some(popup) = self.bash.rect_for(history_area, input_area)
            && popup.contains(ratatui::layout::Position::new(column, row))
        {
            return None;
        }
        self.chat.update(ChatMessage::Mouse { kind, column, row });
        if self.copy_on_select
            && let Some(text) = self.chat.take_pending_copy()
        {
            return Some(SessionEffect::CopyToClipboard { text });
        }
        None
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let collapsed = self.sidebar.collapsed_at(area.width);
        let content_area = if collapsed {
            area
        } else {
            let [sidebar_area, content_area] = Layout::horizontal([Length(30), Min(0)])
                .spacing(1)
                .areas(area);
            self.sidebar.view(frame, sidebar_area);
            content_area
        };

        let title = match &self.session_title {
            Some(t) if !t.is_empty() => format!("Shuvarie · {t}"),
            _ => format!(
                "Shuvarie · {}:{}",
                self.provider.as_deref().unwrap_or("?"),
                self.model.as_deref().unwrap_or("?")
            ),
        };
        let bash_mode = self.is_bash_mode();
        let input_height = if self.permission.open {
            self.permission.desired_height(content_area.width as usize)
        } else if self.question.open {
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
            info_area,
            footer_area,
        ] = Layout::vertical([
            Length(1),
            Length(working_rows),
            Min(0),
            Length(input_height),
            Length(1),
            Length(u16::from(collapsed)),
            Length(1),
        ])
        .areas(content_area);
        self.history_area.set(history_area);
        self.input_area.set(input_area);

        frame.render_widget(
            Paragraph::new(theme::title_header(&title))
                .bg(theme::surface())
                .alignment(Alignment::Center),
            title_area,
        );

        if working_rows > 0 {
            let todos_block = Block::new()
                .bg(theme::surface())
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

        if self.permission.open {
            self.permission.view(frame, input_area);
        } else if self.question.open {
            self.question.view(frame, input_area);
        } else {
            let text_color = if bash_mode {
                theme::accent()
            } else {
                theme::text()
            };
            self.input.view(frame, input_area, text_color);
        }

        if !self.question.open && self.slash.active() {
            let rect = self.slash.popup_rect(history_area, input_area);
            self.slash.view(frame, rect);
        }

        // The bash popup floats over the chat content, directly above the
        // input area; painted last so it sits on top of everything else in
        // the content column.
        if !self.question.open {
            self.bash.view(frame, history_area, input_area);
        }

        // The search tooltip floats over the chat's top-right corner; painted
        // after everything else in the content column so it stays on top. The
        // chat's paint pass reported the visible match count this frame.
        self.search
            .view(frame, history_area, self.chat.search_matches.get());

        let connection = self.provider.as_ref().map(|p| {
            let mut spans = vec![Span::raw(p.clone()).fg(theme::text())];
            if let Some(m) = &self.model {
                spans.push(Span::raw(":").fg(theme::text_muted()));
                spans.push(Span::raw(m.clone()).fg(theme::text_dim()));
                if let Some(v) = &self.variant {
                    spans.push(Span::raw(":").fg(theme::text_muted()));
                    spans.push(Span::raw(v.clone()).fg(theme::accent()));
                }
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
            None if self.is_busy() => Some(("Working...", BusyKind::Tool)),
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
                Span::raw(text).fg(theme::error()),
            ];
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        } else if let Some((status, kind)) = status {
            let mut spans = Vec::new();
            let spinner_span = match kind {
                BusyKind::Idle => None,
                BusyKind::Generating => Some(super::spinner::generating_spinner()),
                BusyKind::Tool => Some(super::spinner::tool_spinner()),
                BusyKind::Waiting => Some(super::spinner::wait_spinner()),
            };

            if let Some(spinner_span) = spinner_span {
                spans.push(spinner_span);
                spans.push(Span::raw(" "));
            }
            spans.push(Span::raw(status).fg(theme::text_muted()));
            frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
        }

        if collapsed {
            let info = self.sidebar.collapsed_line(usize::from(info_area.width));
            frame.render_widget(Paragraph::new(info), info_area);
        }

        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).fg(theme::error()),
                footer_area,
            );
        } else {
            let recall = self
                .chat
                .has_steered()
                .then_some(("Alt+↑", "recall steered"));
            let input_selection = self.input.buffer.selection().is_some();
            let ctrl_c = if input_selection || self.chat.has_selection() {
                "copy"
            } else if self.input.is_empty() {
                "quit"
            } else {
                "clear"
            };
            let cut = input_selection.then_some(("Ctrl+X", "cut"));
            let footer = if !self.question.open && self.slash.active() {
                theme::help_line(&[("Tab", "complete"), ("↑↓", "select"), ("Esc", "dismiss")])
            } else if self.chat.is_streaming() {
                let mut bindings = vec![
                    ("Esc×2", "interrupt"),
                    ("Ctrl+M", "commands"),
                    ("Ctrl+C", ctrl_c),
                ];
                if bash_mode {
                    bindings.insert(0, ("Enter", "run"));
                }
                bindings.extend(cut);
                bindings.extend(recall);
                theme::help_line(&bindings)
            } else {
                let enter = if bash_mode { "run" } else { "send" };
                let mut bindings =
                    vec![("Enter", enter), ("Ctrl+M", "commands"), ("Ctrl+C", ctrl_c)];
                bindings.extend(cut);
                bindings.extend(recall);
                theme::help_line(&bindings)
            };
            let workspace = if collapsed {
                workspace_footer_budget(footer_area.width, footer.width())
                    .map(|budget| self.sidebar.workspace_line(budget))
            } else {
                None
            }
            .filter(|line| line.width() > 0);
            if let Some(workspace) = workspace {
                let [hints_area, _, workspace_area] =
                    Layout::horizontal([Length(footer.width() as u16), Length(2), Min(0)])
                        .areas(footer_area);
                frame.render_widget(Paragraph::new(footer).fg(theme::text_muted()), hints_area);
                frame.render_widget(
                    Paragraph::new(workspace).alignment(Alignment::Right),
                    workspace_area,
                );
            } else {
                frame.render_widget(Paragraph::new(footer).fg(theme::text_muted()), footer_area);
            }
        }
    }
}

#[derive(Debug)]
pub enum SessionEffect {
    /// Send a prompt. `model` is the per-turn streaming override
    /// (`<provider_type>/<model>` from a custom command's frontmatter);
    /// `None` streams on the active provider.
    SendMessage {
        content: String,
        model: Option<String>,
    },
    /// Run a bash-mode (`!`-prefixed) command locally through the resolved
    /// shell. Never persisted, never sent to the model.
    RunBash {
        command: String,
    },
    CancelStream,
    AnswerQuestion {
        id: u64,
        answers: Option<Vec<Vec<String>>>,
    },
    /// Resolve a pending permission ask (`id` from `PermissionRequested`).
    PermissionDecide {
        id: u64,
        decision: shuvarie_core::PermissionAnswer,
    },
    RunCommand {
        action: CommandRef,
        /// Free-form arguments after the command name (e.g. `/title My
        /// title`); `None` for menu launches (Ctrl+M or the slash menu).
        args: Option<String>,
    },
    RecallSteered {
        stacked: bool,
    },
    /// Store `text` on the system clipboard (written as OSC 52 by the render
    /// loop).
    CopyToClipboard {
        text: String,
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
            Span::raw("~").fg(theme::warning()),
            Span::raw(format!(" #{} ", item.id)).fg(theme::text_muted()),
            elided_span(&item.text, text_width, theme::text()),
        ]));
    }
    let overflow = todos.len().saturating_sub(MAX_WORKING_ROWS);
    if overflow > 0 {
        lines.push(Line::from(format!("… +{overflow} more in progress")).fg(theme::text_muted()));
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

    use crate::tui::workspace::WorkspaceInfo;

    fn todo_record(args_json: &str) -> ToolRecord {
        ToolRecord {
            name: "todo".into(),
            args_json: args_json.into(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            killed: false,
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

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
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
    fn loaded_session_restores_context_metrics_in_sidebar() {
        let mut screen = SessionScreen::new();
        screen.sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        let mut session = shuvarie_core::Session::new();
        session.push_user("go");
        session.push_assistant("ok");
        session.last_usage = Some(TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            total_tokens: 20_200,
            cached_input_tokens: 19_400,
            ..Default::default()
        });
        screen.update(SessionMessage::Loaded {
            id: uuid::Uuid::now_v7(),
            title: "t".into(),
            session,
        });

        let rendered = text(screen.sidebar.rendered_lines());
        assert!(
            rendered.contains("20.2k/200k (10%)"),
            "restored request seeds the anchor: {rendered}"
        );
        assert!(rendered.contains("R20k"), "body: {rendered}");
        assert!(rendered.contains("CH97%"), "body: {rendered}");

        screen.update(SessionMessage::Reset);
        let rendered = text(screen.sidebar.rendered_lines());
        assert!(
            !rendered.contains("R20k") && !rendered.contains("CH97%"),
            "reset drops the restored metrics: {rendered}"
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
            id: uuid::Uuid::now_v7(),
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
    fn gen_title_availability_tracks_session_and_first_prompt() {
        fn available(screen: &SessionScreen, action: CommandAction) -> bool {
            screen.slash.available(action)
        }

        let mut screen = SessionScreen::new();
        // A fresh screen has neither a session nor a user prompt.
        assert!(screen.session_id.is_none());
        screen.update(SessionMessage::SpinnerUpdate);
        assert!(!available(&screen, CommandAction::GenTitle));

        // A loaded session with a user prompt enables the command.
        let mut session = shuvarie_core::Session::new();
        session.push_user("go");
        session.push_assistant("ok");
        screen.update(SessionMessage::Loaded {
            id: uuid::Uuid::now_v7(),
            title: "t".into(),
            session,
        });
        // Availability resyncs on the next update.
        screen.update(SessionMessage::SpinnerUpdate);
        assert!(available(&screen, CommandAction::GenTitle));

        // Resetting back to a fresh screen hides it again.
        screen.update(SessionMessage::Reset);
        screen.update(SessionMessage::SpinnerUpdate);
        assert!(!available(&screen, CommandAction::GenTitle));
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
            variant: None,
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
    fn variant_renders_after_the_model_on_status_row() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::UpdateConfig {
            provider: Some("Anthropic".into()),
            model: Some("claude-sonnet-4-5".into()),
            variant: Some("high".into()),
            context_length: Some(200_000),
        });

        let buf = draw(&screen, 80, 24);
        let status_row = content_row_text(&buf, 22);
        assert!(
            status_row.ends_with("Anthropic:claude-sonnet-4-5:high"),
            "status row: {status_row:?}"
        );

        screen.update(SessionMessage::UpdateConfig {
            provider: Some("Anthropic".into()),
            model: Some("claude-sonnet-4-5".into()),
            variant: None,
            context_length: Some(200_000),
        });
        let buf = draw(&screen, 80, 24);
        let status_row = content_row_text(&buf, 22);
        assert!(
            status_row.ends_with("Anthropic:claude-sonnet-4-5"),
            "variant dropped from the display when unset: {status_row:?}"
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
            theme::error(),
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
        assert!(screen.is_busy());
        assert_eq!(screen.busy_kind, BusyKind::Waiting);
    }

    #[test]
    fn compaction_events_drive_busy_state() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::Chat(ChatMessage::StreamError {
            error: "context budget exceeded — compacting session history and continuing".into(),
        }));
        assert!(!screen.is_busy());

        screen.update(SessionMessage::CompactionStarted);
        assert!(screen.is_busy());
        assert_eq!(screen.busy_kind, BusyKind::Waiting);
        assert_eq!(screen.status.as_deref(), Some("Compacting context..."));

        screen.update(SessionMessage::CompactionFinished);
        assert!(screen.is_busy());
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

        screen.update(SessionMessage::Forked {
            session: shuvarie_core::Session::new(),
            prompt: None,
        });
        assert!(screen.is_busy());
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
        assert!(screen.is_busy());
        assert_eq!(screen.busy_kind, BusyKind::Tool);
        assert_eq!(screen.status.as_deref(), Some("Calling tool: read_file"));
    }

    #[test]
    fn stream_error_after_compaction_clears_busy() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::CompactionStarted);
        screen.update(SessionMessage::CompactionFinished);
        assert!(screen.is_busy());

        screen.update(SessionMessage::Chat(ChatMessage::StreamError {
            error: "provider unreachable".into(),
        }));
        assert!(!screen.is_busy());
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
        assert!(!screen.is_busy());
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
        screen.update(SessionMessage::Forked {
            session: shuvarie_core::Session::new(),
            prompt: None,
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
    fn steered_recall_stashes_displaced_draft() {
        let mut screen = SessionScreen::new();
        screen.input.width.set(40);
        screen.input.buffer.set("draft text");
        screen.update(queued("steered one"));
        screen.update(SessionMessage::SteeredRecalled {
            stacked: false,
            content: Some("steered one".into()),
        });
        assert_eq!(screen.input.buffer.value, "steered one");
        screen.input.update(TextAreaMessage::CursorUp);
        assert_eq!(screen.input.buffer.value, "draft text");
    }

    #[test]
    fn turn_reverted_recalls_undone_prompt_into_input() {
        let mut screen = SessionScreen::new();
        screen.input.width.set(40);
        screen.input.buffer.set("in-progress draft");
        screen.update(SessionMessage::Forked {
            session: shuvarie_core::Session::new(),
            prompt: Some("undone prompt".into()),
        });
        assert_eq!(screen.input.buffer.value, "undone prompt");
        screen.input.update(TextAreaMessage::CursorUp);
        assert_eq!(screen.input.buffer.value, "in-progress draft");

        screen.input.buffer.set("other draft");
        screen.update(SessionMessage::Forked {
            session: shuvarie_core::Session::new(),
            prompt: None,
        });
        assert_eq!(screen.input.buffer.value, "other draft");
    }

    #[test]
    fn plain_submit_records_prompt_history() {
        let mut screen = SessionScreen::new();
        screen.input.width.set(40);
        screen.input.buffer.set("first");
        screen.update(SessionMessage::Text(TextAreaMessage::Submit));
        screen.input.buffer.set("second");
        screen.update(SessionMessage::Text(TextAreaMessage::Submit));
        screen.input.buffer.set("draft");
        screen.input.update(TextAreaMessage::CursorUp);
        assert_eq!(screen.input.buffer.value, "second");
        screen.input.update(TextAreaMessage::CursorUp);
        assert_eq!(screen.input.buffer.value, "first");
        screen.input.update(TextAreaMessage::CursorDown);
        assert_eq!(screen.input.buffer.value, "second");
        screen.input.update(TextAreaMessage::CursorDown);
        assert_eq!(screen.input.buffer.value, "draft");
    }

    #[test]
    fn bash_and_slash_submits_skip_prompt_history() {
        let mut screen = SessionScreen::new();
        screen.input.width.set(40);
        screen.input.buffer.set("!ls");
        screen.update(SessionMessage::Text(TextAreaMessage::Submit));
        screen.input.buffer.set("/quit");
        screen.update(SessionMessage::Text(TextAreaMessage::Submit));
        assert!(
            !screen.input.wants_recall_up(),
            "bash and slash submits skip prompt history"
        );
    }

    #[test]
    fn ctrl_p_n_recall_only_when_stacks_hold_entries() {
        let mut screen = SessionScreen::new();
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL)),
            Some(SessionMessage::Chat(ChatMessage::ScrollUp))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL)),
            Some(SessionMessage::Chat(ChatMessage::ScrollDown))
        ));

        screen.input.width.set(40);
        screen.input.remember_sent("sent");
        screen.input.buffer.set("draft");
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL)),
            Some(SessionMessage::Text(TextAreaMessage::CursorUp))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL)),
            Some(SessionMessage::Chat(ChatMessage::ScrollDown))
        ));
        screen.input.update(TextAreaMessage::CursorUp);
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Char('n'), Modifiers::CONTROL)),
            Some(SessionMessage::Text(TextAreaMessage::CursorDown))
        ));
    }

    #[test]
    fn plain_up_down_route_to_input_when_stacks_hold_entries() {
        let mut screen = SessionScreen::new();
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Up, Modifiers::empty())),
            Some(SessionMessage::Chat(ChatMessage::ScrollUp))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Down, Modifiers::empty())),
            Some(SessionMessage::Chat(ChatMessage::ScrollDown))
        ));

        screen.input.remember_sent("sent");
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Up, Modifiers::empty())),
            Some(SessionMessage::Text(TextAreaMessage::CursorUp))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Down, Modifiers::empty())),
            Some(SessionMessage::Chat(ChatMessage::ScrollDown))
        ));
        screen.input.update(TextAreaMessage::CursorUp);
        assert!(matches!(
            screen.map_event(&KeyEvent::new(KeyCode::Down, Modifiers::empty())),
            Some(SessionMessage::Text(TextAreaMessage::CursorDown))
        ));
    }

    #[test]
    fn turn_started_renders_user_turn_and_busy() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::TurnStarted {
            content: "hello".into(),
            steered: false,
        });
        assert!(screen.is_busy());
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
        assert!(screen.is_busy());
    }

    #[test]
    fn turn_started_refreshes_the_git_branch() {
        use crate::tui::workspace::testing::seed_git_repo;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut screen = SessionScreen::new();
        screen.sidebar.update(SidebarMessage::SetWorkspace {
            workspace: WorkspaceInfo {
                path: dir.path().to_path_buf(),
                home: None,
                branch: None,
            },
        });
        let body = |screen: &SessionScreen| {
            screen
                .sidebar
                .rendered_lines()
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(!body(&screen).contains("⎇"), "no branch before the turn");

        // The user checked out another branch since startup; the next turn
        // start should pick it up.
        seed_git_repo(dir.path(), "turn-branch");
        screen.update(SessionMessage::TurnStarted {
            content: "hello".into(),
            steered: false,
        });
        assert!(
            body(&screen).contains("⎇ turn-branch"),
            "branch refreshed at turn start: {}",
            body(&screen)
        );
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

    #[test]
    fn bash_mode_submit_strips_prefix_and_runs_local() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("!cargo build --release");
        assert!(screen.is_bash_mode());
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::RunBash { command })
                if command == "cargo build --release"
        ));
        assert!(screen.input.buffer.value.is_empty());
        assert!(!screen.chat.has_messages(), "bash runs never create turns");
    }

    #[test]
    fn bash_mode_submit_trims_after_bang() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("!  ls -la\n");
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::RunBash { command }) if command == "ls -la"
        ));
    }

    #[test]
    fn bash_mode_bare_exclamation_is_ignored() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("!");
        assert!(screen.is_bash_mode());
        assert!(
            screen
                .update(SessionMessage::Text(TextAreaMessage::Submit))
                .is_none()
        );
    }

    #[test]
    fn plain_submit_still_sends_message() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("hello agent");
        assert!(!screen.is_bash_mode());
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::SendMessage { content, .. })
                if content == "hello agent"
        ));
    }

    fn custom_command_fixture(
        name: &str,
        contents: &str,
    ) -> (tempfile::TempDir, shuvarie_core::CustomCommand) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::create_dir_all(&path).expect("mkdir");
        std::fs::write(path.join("command.md"), contents).expect("write");
        let command = shuvarie_core::CustomCommand {
            name: name.to_string(),
            title: name.to_string(),
            model: Some("openai/gpt-test".to_string()),
            path: path.clone(),
        };
        (dir, command)
    }

    #[test]
    fn custom_command_submit_expands_template_and_model() {
        let (_dir, command) = custom_command_fixture(
            "commit",
            "---\ntitle: Commit\nmodel: anthropic/claude-x\n---\n\nCommit with {{arguments}} please\n",
        );
        let mut screen = SessionScreen::new();
        let effect = screen.update(SessionMessage::SetCustomCommands {
            commands: vec![command],
        });
        assert!(effect.is_none());
        screen.input.buffer.set("/commit tidy the tests");
        match screen.update(SessionMessage::Text(TextAreaMessage::Submit)) {
            Some(SessionEffect::SendMessage { content, model }) => {
                assert_eq!(content, "Commit with tidy the tests please");
                assert_eq!(model.as_deref(), Some("openai/gpt-test"));
            }
            other => panic!("expected a custom command send, got {other:?}"),
        }
    }

    #[test]
    fn custom_command_submit_without_args_clears_placeholder() {
        let (_dir, command) = custom_command_fixture(
            "commit",
            "---\nmodel: openai/gpt-test\n---\n\nCommit with {{arguments}} please\n",
        );
        let mut screen = SessionScreen::new();
        let effect = screen.update(SessionMessage::SetCustomCommands {
            commands: vec![command],
        });
        assert!(effect.is_none());
        screen.input.buffer.set("/commit");
        match screen.update(SessionMessage::Text(TextAreaMessage::Submit)) {
            Some(SessionEffect::SendMessage { content, model }) => {
                assert_eq!(content, "Commit with  please");
                assert_eq!(model.as_deref(), Some("openai/gpt-test"));
            }
            other => panic!("expected a custom command send, got {other:?}"),
        }
    }

    #[test]
    fn custom_command_submit_appends_args_without_placeholder() {
        let (_dir, command) = custom_command_fixture("review", "Review it\n");
        let mut screen = SessionScreen::new();
        let effect = screen.update(SessionMessage::SetCustomCommands {
            commands: vec![command],
        });
        assert!(effect.is_none());
        screen.input.buffer.set("/review focus on the TUI");
        match screen.update(SessionMessage::Text(TextAreaMessage::Submit)) {
            Some(SessionEffect::SendMessage { content, .. }) => {
                assert_eq!(content, "Review it\n\nfocus on the TUI");
            }
            other => panic!("expected a custom command send, got {other:?}"),
        }
    }

    #[test]
    fn unknown_slash_name_falls_through_to_literal_send() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("/nonexistent-command");
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::SendMessage { content, .. })
                if content == "/nonexistent-command"
        ));
    }

    #[test]
    fn custom_command_with_builtin_name_needs_custom_prefix() {
        let (_dir, command) = custom_command_fixture("model", "Override the model\n");
        let mut screen = SessionScreen::new();
        let effect = screen.update(SessionMessage::SetCustomCommands {
            commands: vec![command],
        });
        assert!(effect.is_none());
        // `/model` stays the builtin (the model picker); the custom command
        // answers to `/custom:model`.
        screen.input.buffer.set("/model");
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::RunCommand { .. })
        ));
        screen.input.buffer.set("/custom:model");
        match screen.update(SessionMessage::Text(TextAreaMessage::Submit)) {
            Some(SessionEffect::SendMessage { content, .. }) => {
                assert_eq!(content, "Override the model");
            }
            other => panic!("expected a custom command send, got {other:?}"),
        }
    }
    #[test]
    fn slash_menu_runs_custom_command() {
        let (_dir, command) = custom_command_fixture("commit", "Commit {{arguments}}\n");
        let mut screen = SessionScreen::new();
        let effect = screen.update(SessionMessage::SetCustomCommands {
            commands: vec![command],
        });
        assert!(effect.is_none());
        let effect = screen.update(SessionMessage::RunCustomCommand {
            name: "commit".into(),
        });
        assert!(matches!(effect, Some(SessionEffect::SendMessage { .. })));
    }

    #[test]
    fn footer_shows_run_hint_in_bash_mode() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("!echo hi");
        let buf = draw(&screen, 100, 24);
        let (_, footer) = find_row(&buf, "Enter");
        assert!(footer.contains("run"), "footer: {footer:?}");
        assert!(!footer.contains("send"), "footer: {footer:?}");
    }

    #[test]
    fn bash_events_open_the_popup_and_escape_dismisses_it() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::Bash(BashMessage::Started {
            id: 7,
            command: "cargo build".into(),
        }));
        assert!(screen.bash.open());
        assert!(screen.bash.running());

        screen.update(SessionMessage::Bash(BashMessage::Output {
            id: 7,
            stdout: "Compiling…".into(),
            stderr: String::new(),
        }));
        let buf = draw(&screen, 100, 24);
        let (_, header) = find_row(&buf, "$ cargo build");
        let (_, body) = find_row(&buf, "Compiling");
        assert!(body < header, "output rows render below the command header");

        screen.update(SessionMessage::Bash(BashMessage::Finished {
            id: 7,
            ok: true,
            exit: Some(0),
            stdout: "done".into(),
            stderr: String::new(),
            duration_ms: 120,
        }));
        assert!(screen.bash.open(), "finished run stays visible");

        screen.update(SessionMessage::Bash(BashMessage::Dismiss));
        assert!(!screen.bash.open());
    }

    #[test]
    fn wheel_routes_to_the_popup_over_it_and_leaves_it_elsewhere() {
        let mut screen = SessionScreen::new();
        let body: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        screen.update(SessionMessage::Bash(BashMessage::Started {
            id: 7,
            command: "seq 40".into(),
        }));
        screen.update(SessionMessage::Bash(BashMessage::Output {
            id: 7,
            stdout: body,
            stderr: String::new(),
        }));
        draw(&screen, 100, 30);
        let input_area = screen.input_area.get();
        let history_area = screen.history_area.get();
        let popup = screen
            .bash
            .rect_for(history_area, input_area)
            .expect("popup is on screen");
        assert!(screen.bash.follow.get(), "the tail is followed by default");

        let inside =
            ratatui::layout::Position::new(popup.x + popup.width / 2, popup.y + popup.height / 2);
        for _ in 0..30 {
            screen.update(SessionMessage::Wheel {
                up: true,
                column: inside.x,
                row: inside.y,
            });
        }
        assert!(
            !screen.bash.follow.get(),
            "wheeling up over the popup scrolls it"
        );
        assert_eq!(screen.bash.scroll.get(), 0);

        let buf = draw(&screen, 100, 30);
        let popup_area = screen
            .bash
            .rect_for(history_area, input_area)
            .expect("popup is on screen");
        let (help, _) = find_row(&buf, "dismiss");
        assert!(
            help >= popup_area.y && help < popup_area.y + popup_area.height,
            "the help line renders inside the popup (row {help})"
        );

        let outside = ratatui::layout::Position::new(input_area.x + 1, input_area.y);
        assert!(
            !popup.contains(outside),
            "the probe point sits below the popup"
        );
        for _ in 0..3 {
            screen.update(SessionMessage::Wheel {
                up: true,
                column: outside.x,
                row: outside.y,
            });
        }
        assert_eq!(
            screen.bash.scroll.get(),
            0,
            "a wheel elsewhere never moves the popup"
        );
        assert!(!screen.bash.follow.get());
    }

    #[test]
    fn escape_reaches_the_bash_popup_before_the_input() {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::Bash(BashMessage::Started {
            id: 0,
            command: "ls".into(),
        }));
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::Bash(BashMessage::Dismiss))
        ));
    }

    fn streaming_screen() -> SessionScreen {
        let mut screen = SessionScreen::new();
        screen.update(SessionMessage::TurnStarted {
            content: "hi".into(),
            steered: false,
        });
        screen.update(SessionMessage::Chat(ChatMessage::TokenReceived {
            content: "answer".into(),
        }));
        screen
    }

    #[test]
    fn double_escape_while_streaming_requests_cancel() {
        let mut screen = streaming_screen();
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
        screen.update(SessionMessage::EscapePressed);
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::CancelRequested)
        ));
        assert!(matches!(
            screen.update(SessionMessage::CancelRequested),
            Some(SessionEffect::CancelStream)
        ));
        assert!(screen.last_escape.is_none(), "window closes after a cancel");
    }

    #[test]
    fn single_escape_while_streaming_does_not_cancel() {
        let screen = streaming_screen();
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
    }

    #[test]
    fn escape_window_closes_when_the_turn_ends() {
        let mut screen = streaming_screen();
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
        screen.update(SessionMessage::EscapePressed);
        screen.update(SessionMessage::Chat(ChatMessage::StreamDone));
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
    }

    #[test]
    fn escape_window_resets_when_a_new_turn_starts() {
        let mut screen = streaming_screen();
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
        screen.update(SessionMessage::EscapePressed);
        screen.update(SessionMessage::Chat(ChatMessage::StreamDone));
        screen.update(SessionMessage::TurnStarted {
            content: "next".into(),
            steered: false,
        });
        screen.update(SessionMessage::Chat(ChatMessage::TokenReceived {
            content: "a".into(),
        }));
        assert!(matches!(
            screen.map_event(&KeyCode::Escape.into()),
            Some(SessionMessage::EscapePressed)
        ));
    }

    #[test]
    fn streaming_footer_hints_double_escape_interrupt() {
        let screen = streaming_screen();
        let buf = draw(&screen, 100, 24);
        let (_, footer) = find_row(&buf, "Esc×2");
        assert!(footer.contains("interrupt"), "footer: {footer:?}");
        assert!(footer.contains("Ctrl+C quit"), "footer: {footer:?}");
    }

    #[test]
    fn idle_footer_hints_ctrl_c_clear_with_draft() {
        let mut screen = SessionScreen::new();
        screen.input.buffer.set("draft");
        let buf = draw(&screen, 100, 24);
        let (_, footer) = find_row(&buf, "Ctrl+C");
        assert!(footer.contains("clear"), "footer: {footer:?}");
        assert!(!footer.contains("quit"), "footer: {footer:?}");
    }

    #[test]
    fn streaming_footer_hints_ctrl_c_clear_with_draft() {
        let mut screen = streaming_screen();
        screen.input.buffer.set("draft");
        let buf = draw(&screen, 100, 24);
        let (_, footer) = find_row(&buf, "Ctrl+C");
        assert!(footer.contains("clear"), "footer: {footer:?}");
        assert!(!footer.contains("quit"), "footer: {footer:?}");
    }

    fn info_screen() -> SessionScreen {
        let mut screen = SessionScreen::new();
        screen.sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        screen.sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 10_100,
                output_tokens: 12_300,
                ..Default::default()
            },
            cost: 0.125,
            context_tokens: Some(84_000),
        });
        screen.sidebar.update(SidebarMessage::UpdateLsp {
            servers: vec![shuvarie_core::LspStatus {
                name: "rust-analyzer".into(),
                language: "rust".into(),
                status: shuvarie_core::ServerStatus::Running,
                pid: None,
                diagnostics: 3,
                error: None,
            }],
        });
        screen
    }

    #[test]
    fn narrow_screen_collapses_sidebar_and_shows_info_line() {
        let screen = info_screen();
        let buf = draw(&screen, 79, 24);
        let full: String = (0..24).map(|y| row_text(&buf, y)).collect();
        assert!(!full.contains("Skills"), "sidebar hidden: {full:?}");
        let (y, info) = find_row(&buf, "rust-analyzer");
        assert!(info.contains("↑10.1k ↓12.3k"), "row {y}: {info:?}");
        assert!(info.contains("84k/200k (42%)"), "row {y}: {info:?}");
        assert!(info.contains("$0.12"), "row {y}: {info:?}");
        assert!(info.contains("⚑3"), "row {y}: {info:?}");
        let (footer_y, _) = find_row(&buf, "Ctrl+M");
        assert_eq!(footer_y + 1, buf.area().height, "footer is the last row");
        assert_eq!(y, footer_y - 1, "info line sits between status and footer");
    }

    #[test]
    fn wide_screen_keeps_sidebar_and_skips_info_line() {
        let screen = info_screen();
        let buf = draw(&screen, 80, 24);
        let (y, _) = find_row(&buf, "rust-analyzer");
        assert!(y < 20, "LSP renders in the sidebar column: row {y}");
        let (info_y, _) = find_row(&buf, "84k/200k");
        assert_ne!(info_y, y, "window fraction lives in the sidebar panel");
        assert!(
            !row_text(&buf, buf.area().height - 2).contains("rust-analyzer"),
            "no standalone info line above the footer"
        );
        assert!(
            find_row(&buf, "Context").1.contains("Context"),
            "sidebar Context panel renders"
        );
    }

    #[test]
    fn collapsed_footer_shows_workspace_path_and_branch_on_the_right() {
        let mut screen = info_screen();
        screen.sidebar.update(SidebarMessage::SetWorkspace {
            workspace: WorkspaceInfo {
                path: "/home/user/repos/shuvarie".into(),
                home: Some("/home/user".into()),
                branch: Some("main".into()),
            },
        });
        let buf = draw(&screen, 79, 24);
        let (footer_y, footer) = find_row(&buf, "Ctrl+M");
        assert_eq!(footer_y, buf.area().height - 1, "footer is the last row");
        assert!(footer.contains("Ctrl+C quit"), "hints render: {footer:?}");
        assert!(
            footer.contains("~/repos/shuvarie ⎇ main"),
            "path and branch render right-aligned: {footer:?}"
        );
        assert!(
            footer.trim_end().ends_with("⎇ main"),
            "path sits at the right edge: {footer:?}"
        );
    }

    #[test]
    fn wide_footer_leaves_workspace_to_the_sidebar() {
        let mut screen = info_screen();
        screen.sidebar.update(SidebarMessage::SetWorkspace {
            workspace: WorkspaceInfo {
                path: "/home/user/repos/shuvarie".into(),
                home: Some("/home/user".into()),
                branch: Some("main".into()),
            },
        });
        let buf = draw(&screen, 80, 24);
        let (footer_y, footer) = find_row(&buf, "Ctrl+M");
        assert_eq!(footer_y, buf.area().height - 1, "footer is the last row");
        assert!(!footer.contains("⎇"), "footer stays hint-only: {footer:?}");
        let (path_y, path) = find_row(&buf, "~/repos/shuvarie");
        assert!(path_y < footer_y, "sidebar shows the path: {path:?}");
    }

    #[test]
    fn sidebar_toggle_message_flips_collapse() {
        let mut screen = SessionScreen::new();
        screen
            .sidebar
            .update(SidebarMessage::SetWidth { cols: 200 });
        assert!(!screen.sidebar.collapsed_at(200));
        screen.update(SessionMessage::Sidebar(SidebarMessage::Toggle));
        assert!(screen.sidebar.collapsed_at(200));
        screen.update(SessionMessage::Sidebar(SidebarMessage::Toggle));
        assert!(!screen.sidebar.collapsed_at(200));
    }

    #[test]
    fn ctrl_w_maps_to_sidebar_toggle() {
        let screen = SessionScreen::new();
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('w'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            screen.map_event(&key),
            Some(SessionMessage::Sidebar(SidebarMessage::Toggle))
        ));
    }

    #[test]
    fn search_submit_parses_with_args() {
        let mut screen = SessionScreen::new();
        screen.input.width.set(40);
        screen.input.buffer.set("/search foo bar");
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::RunCommand {
                action: CommandRef::Builtin(CommandAction::Search),
                args: Some(args)
            }) if args == "foo bar"
        ));
        screen.input.buffer.set("/search");
        assert!(matches!(
            screen.update(SessionMessage::Text(TextAreaMessage::Submit)),
            Some(SessionEffect::RunCommand {
                action: CommandRef::Builtin(CommandAction::Search),
                args: None
            })
        ));
    }

    #[test]
    fn escape_exits_search_and_clears_the_highlight() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("hi"));
        assert!(screen.search.is_open());
        assert_eq!(screen.chat.search_needle().as_deref(), Some("hi"));

        let msg = screen.map_event(&KeyEvent::from(KeyCode::Escape));
        assert!(matches!(
            msg,
            Some(SessionMessage::Search(SearchMessage::Exit))
        ));
        screen.update(msg.expect("mapped escape"));
        assert!(!screen.search.is_open());
        assert!(screen.chat.search_needle().is_none());
    }

    #[test]
    fn session_change_resets_search_text() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("stale"));
        screen.update(SessionMessage::Reset);
        assert!(!screen.search.is_open());
        assert!(screen.chat.search_needle().is_none());

        screen.open_search(Some("stale"));
        screen.update(SessionMessage::Forked {
            session: shuvarie_core::Session::new(),
            prompt: None,
        });
        assert!(!screen.search.is_open());
        assert!(screen.chat.search_needle().is_none());
    }

    #[test]
    fn search_edits_flow_into_the_chat_needle() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("ab"));
        screen.update(SessionMessage::Search(SearchMessage::Insert('c')));
        assert_eq!(screen.chat.search_needle().as_deref(), Some("abc"));
        screen.update(SessionMessage::Search(SearchMessage::Backspace));
        assert_eq!(screen.chat.search_needle().as_deref(), Some("ab"));
    }

    #[test]
    fn search_mode_routes_keys_away_from_the_input() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("a"));

        assert!(matches!(
            screen.map_event(&KeyEvent::from(KeyCode::Char('x'))),
            Some(SessionMessage::Search(SearchMessage::Insert('x')))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::from(KeyCode::Tab)),
            Some(SessionMessage::Search(SearchMessage::Swallow))
        ));
        assert!(matches!(
            screen.map_event(&KeyEvent::from(KeyCode::Left)),
            Some(SessionMessage::Search(SearchMessage::CursorLeft))
        ));
        assert!(
            matches!(
                screen.map_event(&KeyEvent::from(KeyCode::Up)),
                Some(SessionMessage::Chat(ChatMessage::ScrollUp))
            ),
            "scroll keys still reach the chat"
        );
        assert!(matches!(
            screen.map_event(&KeyEvent::from(KeyCode::Down)),
            Some(SessionMessage::Chat(ChatMessage::ScrollDown))
        ));
        assert_eq!(screen.input.buffer.value, "", "no leak into the prompt");
    }

    #[test]
    fn search_mode_captures_paste() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("a"));
        assert!(matches!(
            screen.map_paste("xyz"),
            Some(SessionMessage::Search(SearchMessage::Paste(p))) if p == "xyz"
        ));
    }

    #[test]
    fn search_tooltip_paints_over_the_history_top_right() {
        let mut screen = SessionScreen::new();
        screen.open_search(Some("zz"));
        let buf = draw(&screen, 100, 30);
        // The tooltip floats over the history pane's top-right corner: the
        // needle row and the exit hint, with the "search" title above it.
        let title = content_row_text(&buf, 1);
        assert!(title.contains("search"), "title row: {title:?}");
        let text_row = content_row_text(&buf, 3);
        assert!(text_row.contains("zz"), "needle row: {text_row:?}");
        assert!(
            text_row.contains("no matches"),
            "match label on an empty chat: {text_row:?}"
        );
        assert!(
            title.trim_end().ends_with("search"),
            "pinned to the right edge: {title:?}"
        );
    }
}
