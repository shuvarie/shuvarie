use std::collections::HashMap;

use ratatui::prelude::*;
use shuvarie_core::{Connections, Event as CoreEvent, Model};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use tokio::sync::mpsc::Sender;

use crate::tui::event::Event;
use crate::tui::utils::ctrl;

use super::add_provider::{AddProviderForm, AddProviderMessage, AddProviderOutcome};
use super::command_menu::{CommandMenu, CommandMenuMessage};
use super::commands::CommandAction;
use super::components::TextAreaMessage;
use super::confirm_quit::{ConfirmQuit, ConfirmQuitEffect, ConfirmQuitMessage};
use super::context::UpdateCtx;
use super::history_search::{HistorySearch, HistorySearchEffect, HistorySearchMessage};
use super::model_picker::{ModelPicker, ModelPickerEffect, ModelPickerMessage};
use super::session::{ChatMessage, SessionEffect, SessionMessage, SessionScreen};
use super::session_picker::{SessionPicker, SessionPickerEffect, SessionPickerMessage};
use super::sidebar::SidebarMessage;
use super::spinner::SpinnerKind;
use super::warning::{WarningMessage, WarningPopup};
use super::welcome::{Welcome, WelcomeEffect, WelcomeMessage};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Welcome,
    AddProvider,
    ModelPicker,
    CommandMenu,
    ConfirmQuit,
    SessionPicker,
    HistorySearch,
}

pub enum AppMessage {
    OpenCommandMenu,
    RequestQuit,
    ConfirmQuit,
    CancelQuit,
    Resized {
        rows: u16,
        cols: u16,
    },
    Session(SessionMessage),
    AddProvider(AddProviderMessage),
    ModelPicker(ModelPickerMessage),
    CommandMenu(CommandMenuMessage),
    Welcome(WelcomeMessage),
    SessionPicker(SessionPickerMessage),
    HistorySearch(HistorySearchMessage),
    ConfigSaved,
    ConfigError {
        error: String,
    },
    ModelsLoaded {
        provider_name: String,
        models: Vec<Model>,
    },
    ModelsError {
        provider_name: String,
        error: String,
    },
    SessionsLoaded {
        sessions: Vec<shuvarie_core::SessionSummary>,
    },
    SessionLoaded {
        id: uuid::Uuid,
        title: String,
        session: shuvarie_core::Session,
    },
    SessionCreated {
        id: uuid::Uuid,
        title: String,
    },
    SessionDeleted {
        id: uuid::Uuid,
    },
    SessionError {
        error: String,
    },
    LspStatus {
        servers: Vec<shuvarie_core::LspStatus>,
    },
    LspDiagnostics {
        path: String,
        diagnostics: Vec<shuvarie_core::DiagnosticInfo>,
    },
    LspError {
        error: String,
    },
    SkillsLoaded {
        skills: Vec<shuvarie_core::Skill>,
        warnings: Vec<shuvarie_core::SkillWarning>,
    },
    ShellWarning {
        message: String,
    },
    Warning(WarningMessage),
}

#[derive(Debug)]
pub enum AppEffect {
    Quit,
}

pub struct App {
    pub ctx: UpdateCtx,
    pub overlay: Overlay,
    pub session: SessionScreen,
    pub command_menu: CommandMenu,
    pub welcome: Welcome,
    pub confirm_quit: ConfirmQuit,
    pub add_provider_form: Option<AddProviderForm>,
    pub model_picker: ModelPicker,
    pub session_picker: SessionPicker,
    pub history_search: HistorySearch,
    pub warning: WarningPopup,
    pub models: HashMap<String, Vec<Model>>,
    pending_model_pick: Option<String>,
    quit: bool,
}

/// Resolve the active connection's model context window from the Selune
/// catalog — the same lookup the core task uses for its context budget.
fn catalog_context_length(connections: &Connections) -> Option<u64> {
    let active = connections.active.as_ref()?;
    let model = active.model.as_deref()?;
    let id = connections
        .providers
        .get(&active.provider)
        .and_then(|pc| pc.catalog_id())?;
    let providers = shuvarie_core::catalog::providers();
    let provider = shuvarie_core::catalog::find_provider(&providers, id)?;
    shuvarie_core::catalog::context_length(provider, model).map(|c| c.max(0) as u64)
}

impl App {
    pub fn new(connections: Connections, cmd_tx: Sender<shuvarie_core::Command>) -> Self {
        let mut welcome = Welcome::new();
        if !connections.has_connected_providers() {
            welcome.open();
        }
        let initial_provider = connections.active.as_ref().map(|a| a.provider.clone());
        let initial_model = connections.active.as_ref().and_then(|a| a.model.clone());
        let initial_display = initial_provider
            .as_deref()
            .and_then(|id| connections.providers.get(id).map(|p| p.name.clone()));
        let initial_context_length = catalog_context_length(&connections);
        Self {
            ctx: UpdateCtx::new(connections, cmd_tx),
            overlay: if welcome.open {
                Overlay::Welcome
            } else {
                Overlay::None
            },
            session: {
                let mut s = SessionScreen::new();
                s.update(SessionMessage::UpdateConfig {
                    provider: initial_display,
                    model: initial_model,
                    context_length: initial_context_length,
                });
                s
            },
            command_menu: CommandMenu::new(),
            welcome,
            confirm_quit: ConfirmQuit::new(),
            add_provider_form: None,
            model_picker: ModelPicker::new(),
            session_picker: SessionPicker::new(),
            history_search: HistorySearch::new(),
            warning: WarningPopup::new(),
            models: HashMap::new(),
            pending_model_pick: None,
            quit: false,
        }
    }

    /// Record that the app should quit (set when `update` returns `Quit`).
    pub fn mark_quit(&mut self) {
        self.quit = true;
    }

    /// Whether the render loop should terminate.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Tab title shown by the terminal emulator: the active session title
    /// (first line only) once a session exists, otherwise the app name with
    /// the active provider/model when one is configured.
    pub fn window_title(&self) -> String {
        let session = self
            .session
            .session_title
            .as_deref()
            .and_then(|t| t.lines().next())
            .map(str::trim)
            .filter(|t| !t.is_empty());
        if let Some(session) = session {
            return format!("Shuvarie — {session}");
        }
        let active = self.ctx.connections.active.as_ref();
        let model = active.and_then(|a| a.model.as_deref());
        let provider = active
            .and_then(|a| self.ctx.connections.providers.get(&a.provider))
            .map(|p| p.name.as_str());
        match (model, provider) {
            (Some(model), Some(provider)) => format!("Shuvarie — {model} · {provider}"),
            (Some(model), None) => format!("Shuvarie — {model}"),
            (None, Some(provider)) => format!("Shuvarie — {provider}"),
            (None, None) => "Shuvarie".to_string(),
        }
    }

    pub fn map_event(&self, ev: Event) -> Option<AppMessage> {
        match ev {
            Event::Terminal(term_ev) => match term_ev {
                TermEvent::WindowResized(size) => Some(AppMessage::Resized {
                    rows: size.rows,
                    cols: size.cols,
                }),
                TermEvent::Mouse(mouse) if self.overlay == Overlay::None => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => Some(AppMessage::Session(
                        SessionMessage::Chat(ChatMessage::Click {
                            column: mouse.column,
                            row: mouse.row,
                        }),
                    )),
                    MouseEventKind::ScrollUp => Some(AppMessage::Session(SessionMessage::Chat(
                        ChatMessage::Wheel {
                            up: true,
                            column: mouse.column,
                            row: mouse.row,
                        },
                    ))),
                    MouseEventKind::ScrollDown => Some(AppMessage::Session(SessionMessage::Chat(
                        ChatMessage::Wheel {
                            up: false,
                            column: mouse.column,
                            row: mouse.row,
                        },
                    ))),
                    _ => None,
                },
                TermEvent::Key(key) => {
                    // Transient warning popup: swallows one key press and
                    // dismisses, leaving the underlying overlay untouched.
                    if self.warning.open {
                        return self.warning.map_event(&key).map(AppMessage::Warning);
                    }

                    // Overlay events
                    match self.overlay {
                        Overlay::CommandMenu => {
                            return self
                                .command_menu
                                .map_event(&key)
                                .map(AppMessage::CommandMenu);
                        }
                        Overlay::AddProvider => {
                            return self
                                .add_provider_form
                                .as_ref()
                                .and_then(|f| f.map_event(&key))
                                .map(AppMessage::AddProvider);
                        }
                        Overlay::ModelPicker => {
                            return self
                                .model_picker
                                .map_event(&key)
                                .map(AppMessage::ModelPicker);
                        }
                        Overlay::Welcome => {
                            return self.welcome.map_event(&key).map(AppMessage::Welcome);
                        }
                        Overlay::ConfirmQuit => {
                            return self.confirm_quit.map_event(&key).map(|m| match m {
                                ConfirmQuitMessage::Confirm => AppMessage::ConfirmQuit,
                                ConfirmQuitMessage::Cancel => AppMessage::CancelQuit,
                            });
                        }
                        Overlay::SessionPicker => {
                            return self
                                .session_picker
                                .map_event(&key)
                                .map(AppMessage::SessionPicker);
                        }
                        Overlay::HistorySearch => {
                            return self
                                .history_search
                                .map_event(&key)
                                .map(AppMessage::HistorySearch);
                        }
                        Overlay::None => {}
                    }

                    // App key event
                    #[allow(clippy::single_match)]
                    match key.kind {
                        KeyEventKind::Press => match key.code {
                            KeyCode::Char('c') if ctrl(&key) => {
                                if !self.session.input.is_empty() {
                                    return Some(AppMessage::Session(SessionMessage::Text(
                                        TextAreaMessage::Clear,
                                    )));
                                }
                                if self.session.is_streaming() {
                                    return Some(AppMessage::Session(
                                        SessionMessage::CancelRequested,
                                    ));
                                }
                                return Some(AppMessage::RequestQuit);
                            }
                            KeyCode::Char('m') if ctrl(&key) => {
                                return Some(AppMessage::OpenCommandMenu);
                            }
                            KeyCode::Char('r') if ctrl(&key) => {
                                return Some(AppMessage::HistorySearch(HistorySearchMessage::Open));
                            }
                            _ => {}
                        },
                        _ => {}
                    }

                    self.session.map_event(&key).map(AppMessage::Session)
                }
                _ => None,
            },
            Event::Core(core_ev) => match core_ev {
                CoreEvent::Pong => Some(AppMessage::CommandMenu(CommandMenuMessage::Close)),
                CoreEvent::ModelsLoaded {
                    provider_name,
                    models,
                } => Some(AppMessage::ModelsLoaded {
                    provider_name,
                    models,
                }),
                CoreEvent::ModelsError {
                    provider_name,
                    error,
                } => Some(AppMessage::ModelsError {
                    provider_name,
                    error,
                }),
                CoreEvent::ConfigSaved => Some(AppMessage::ConfigSaved),
                CoreEvent::ConfigError { error } => Some(AppMessage::ConfigError { error }),
                CoreEvent::SessionStarted => None,
                CoreEvent::SessionCreated { id, title } => {
                    Some(AppMessage::SessionCreated { id, title })
                }
                CoreEvent::TokenReceived { content } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::TokenReceived { content }),
                )),
                CoreEvent::ReasoningReceived { content } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::ReasoningReceived { content }),
                )),
                CoreEvent::ContextLoaded { paths } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::ContextLoaded { paths }),
                )),
                CoreEvent::ToolStarted {
                    name,
                    args,
                    worker,
                    call_id,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::ToolStarted {
                        name,
                        args,
                        worker,
                        call_id: Some(call_id),
                    },
                ))),
                CoreEvent::ToolFinished {
                    name,
                    ok,
                    output,
                    worker,
                    file_change,
                    streams,
                    duration_ms,
                    call_id,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::ToolFinished {
                        name,
                        ok,
                        output,
                        worker,
                        file_change,
                        streams,
                        duration_ms,
                        call_id: Some(call_id),
                    },
                ))),
                CoreEvent::ToolOutput {
                    tool,
                    worker,
                    stdout,
                    stderr,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::ToolOutput {
                        tool,
                        worker,
                        stdout,
                        stderr,
                    },
                ))),
                CoreEvent::WorkerStarted {
                    name,
                    args,
                    call_id,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::WorkerStarted {
                        name,
                        args,
                        call_id: Some(call_id),
                    },
                ))),
                CoreEvent::WorkerFinished {
                    name,
                    ok,
                    output,
                    duration_ms,
                    call_id,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::WorkerFinished {
                        name,
                        ok,
                        output,
                        duration_ms,
                        call_id: Some(call_id),
                    },
                ))),
                CoreEvent::StreamDone { .. } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::StreamDone,
                ))),
                CoreEvent::PromptSteered { content } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::SteeredQueued { content }),
                )),
                CoreEvent::TurnStarted { content, steered } => {
                    Some(AppMessage::Session(SessionMessage::TurnStarted {
                        content,
                        steered,
                    }))
                }
                CoreEvent::SteeredRecalled { stacked, content } => {
                    Some(AppMessage::Session(SessionMessage::SteeredRecalled {
                        stacked,
                        content,
                    }))
                }
                CoreEvent::SteeredCleared => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::SteeredCleared,
                ))),
                CoreEvent::StreamError { error } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::StreamError { error }),
                )),
                CoreEvent::StreamCancelled => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::StreamCancelled,
                ))),
                CoreEvent::CompactionStarted => {
                    Some(AppMessage::Session(SessionMessage::CompactionStarted))
                }
                CoreEvent::CompactionFinished => {
                    Some(AppMessage::Session(SessionMessage::CompactionFinished))
                }
                CoreEvent::RetryScheduled {
                    reason,
                    attempt,
                    max_attempts,
                    delay_ms,
                    ..
                } => Some(AppMessage::Session(SessionMessage::RetryScheduled {
                    reason,
                    attempt,
                    max_attempts,
                    delay_ms,
                })),
                CoreEvent::UsageUpdate {
                    usage,
                    cost,
                    context_tokens,
                } => Some(AppMessage::Session(SessionMessage::UsageUpdate {
                    usage,
                    cost,
                    context_tokens,
                })),
                CoreEvent::UsageSnapshot { usage, cost } => {
                    Some(AppMessage::Session(SessionMessage::UsageSnapshot {
                        usage,
                        cost,
                    }))
                }
                CoreEvent::SessionsLoaded { sessions } => {
                    Some(AppMessage::SessionsLoaded { sessions })
                }
                CoreEvent::SessionLoaded { id, title, session } => {
                    Some(AppMessage::SessionLoaded { id, title, session })
                }
                CoreEvent::SessionDeleted { id } => Some(AppMessage::SessionDeleted { id }),
                CoreEvent::SessionError { error } => Some(AppMessage::SessionError { error }),
                CoreEvent::TurnReverted { session } => {
                    Some(AppMessage::Session(SessionMessage::TurnReverted {
                        session,
                    }))
                }
                CoreEvent::TurnRestored { session } => {
                    Some(AppMessage::Session(SessionMessage::TurnRestored {
                        session,
                    }))
                }
                CoreEvent::SearchResults { hits } => {
                    Some(AppMessage::HistorySearch(HistorySearchMessage::Results {
                        hits,
                    }))
                }
                CoreEvent::SearchError { error } => {
                    Some(AppMessage::HistorySearch(HistorySearchMessage::Error {
                        error,
                    }))
                }
                CoreEvent::QuestionAsked { id, questions } => {
                    Some(AppMessage::Session(SessionMessage::QuestionAsked {
                        id,
                        questions,
                    }))
                }
                CoreEvent::LspStatus { servers } => Some(AppMessage::LspStatus { servers }),
                CoreEvent::LspDiagnostics { path, diagnostics } => {
                    Some(AppMessage::LspDiagnostics { path, diagnostics })
                }
                CoreEvent::LspError { error } => Some(AppMessage::LspError { error }),
                CoreEvent::SkillsLoaded { skills, warnings } => {
                    Some(AppMessage::SkillsLoaded { skills, warnings })
                }
                CoreEvent::ShellWarning { message } => Some(AppMessage::ShellWarning { message }),
            },
        }
    }

    pub fn update(&mut self, msg: AppMessage) -> Option<AppEffect> {
        match msg {
            AppMessage::OpenCommandMenu => {
                self.session.update(SessionMessage::ClearError);
                self.update_command_availability();
                self.command_menu.open();
                self.overlay = Overlay::CommandMenu;
            }
            AppMessage::RequestQuit => {
                self.confirm_quit.open();
                self.overlay = Overlay::ConfirmQuit;
            }
            AppMessage::ConfirmQuit => {
                if let Some(ConfirmQuitEffect::Confirm) =
                    self.confirm_quit.update(ConfirmQuitMessage::Confirm)
                {
                    return Some(AppEffect::Quit);
                }
            }
            AppMessage::CancelQuit => {
                self.confirm_quit.update(ConfirmQuitMessage::Cancel);
                self.overlay = Overlay::None;
            }
            AppMessage::Session(m) => {
                if let Some(effect) = self.session.update(m) {
                    match effect {
                        SessionEffect::SendMessage { content } => {
                            if !self.session.busy && self.session.session_id.is_none() {
                                self.ctx.send(shuvarie_core::Command::StartSession);
                            }
                            self.ctx
                                .send(shuvarie_core::Command::SendMessage { content });
                        }
                        SessionEffect::CancelStream => {
                            self.ctx.send(shuvarie_core::Command::CancelStream);
                        }
                        SessionEffect::RecallSteered { stacked } => {
                            self.ctx
                                .send(shuvarie_core::Command::RecallSteered { stacked });
                        }
                        SessionEffect::AnswerQuestion { id, answers } => {
                            self.ctx
                                .send(shuvarie_core::Command::AnswerQuestion { id, answers });
                        }
                        SessionEffect::RunCommand(action) => {
                            if let Some(effect) = self.run_command(action) {
                                return Some(effect);
                            }
                        }
                    }
                }
            }
            AppMessage::SessionPicker(m) => {
                if let Some(effect) = self.session_picker.update(m) {
                    match effect {
                        SessionPickerEffect::LoadSession { id } => {
                            self.ctx.send(shuvarie_core::Command::LoadSession { id });
                            self.close_overlay();
                        }
                        SessionPickerEffect::DeleteSession { id } => {
                            self.ctx.send(shuvarie_core::Command::DeleteSession { id });
                        }
                        SessionPickerEffect::NewSession => {
                            self.session.update(SessionMessage::Reset);
                            self.ctx.send(shuvarie_core::Command::NewSession);
                            self.close_overlay();
                        }
                        SessionPickerEffect::Close => {
                            self.close_overlay();
                        }
                    }
                }
            }
            AppMessage::AddProvider(m) => {
                if let Some(form) = &mut self.add_provider_form {
                    let outcome = form.update(m);
                    match outcome {
                        AddProviderOutcome::Cancel => {
                            self.close_overlay();
                        }
                        AddProviderOutcome::Submit {
                            kind,
                            catalog,
                            name,
                            api_key,
                            base_url,
                        } => {
                            let id = uuid::Uuid::new_v4().to_string();
                            let pc = shuvarie_core::ProviderConfig::new(
                                name.clone(),
                                kind,
                                api_key,
                                base_url,
                            )
                            .with_catalog(catalog);
                            self.pending_model_pick = Some(id.clone());
                            self.ctx.send(shuvarie_core::Command::AddProvider {
                                id: id.clone(),
                                config: pc,
                            });
                            self.ctx.send(shuvarie_core::Command::SetActiveProvider {
                                name: id.clone(),
                            });
                            self.ctx
                                .send(shuvarie_core::Command::ListModels { provider_name: id });
                            self.close_overlay();
                        }
                        AddProviderOutcome::None => {}
                    }
                }
            }
            AppMessage::ModelPicker(m) => {
                if let Some(effect) = self.model_picker.update(m) {
                    match effect {
                        ModelPickerEffect::Selected { model } => {
                            self.ctx
                                .send(shuvarie_core::Command::SetActiveModel { model });
                        }
                        ModelPickerEffect::Close => {
                            if self
                                .ctx
                                .connections
                                .active
                                .as_ref()
                                .is_none_or(|a| a.model.is_none())
                                && let Some(first) = self.model_picker.models.first()
                            {
                                self.ctx.send(shuvarie_core::Command::SetActiveModel {
                                    model: first.id.clone(),
                                });
                            }
                        }
                    }
                    self.close_overlay();
                }
            }
            AppMessage::CommandMenu(m) => {
                if let Some(action) = self.command_menu.update(m)
                    && let Some(effect) = self.run_command(action)
                {
                    return Some(effect);
                }
                if !self.command_menu.open && self.overlay == Overlay::CommandMenu {
                    self.overlay = Overlay::None;
                }
            }
            AppMessage::HistorySearch(m) => {
                if let Some(effect) = self.history_search.update(m) {
                    match effect {
                        HistorySearchEffect::Open => {
                            self.overlay = Overlay::HistorySearch;
                        }
                        HistorySearchEffect::Search { query } => {
                            self.ctx
                                .send(shuvarie_core::Command::SearchHistory { query });
                        }
                        HistorySearchEffect::LoadSession { id } => {
                            self.ctx.send(shuvarie_core::Command::LoadSession { id });
                            self.close_overlay();
                        }
                        HistorySearchEffect::Close => {
                            self.close_overlay();
                        }
                    }
                }
            }
            AppMessage::Welcome(m) => {
                if let Some(effect) = self.welcome.update(m) {
                    match effect {
                        WelcomeEffect::AddProvider => {
                            let names: Vec<String> = self
                                .ctx
                                .connections
                                .providers
                                .values()
                                .map(|p| p.name.clone())
                                .collect();
                            let providers = shuvarie_core::catalog::providers();
                            self.add_provider_form = Some(AddProviderForm::new(providers, &names));
                            self.overlay = Overlay::AddProvider;
                        }
                    }
                }
            }
            AppMessage::Resized { rows, cols } => {
                let area = Rect::new(0, 0, cols, rows);
                match self.overlay {
                    Overlay::CommandMenu => {
                        if let Some(h) = command_menu_list_height(area) {
                            self.command_menu
                                .update(CommandMenuMessage::Resize { viewport_height: h });
                        }
                    }
                    Overlay::ModelPicker => {
                        if let Some(h) = model_picker_list_height(area) {
                            self.model_picker
                                .update(ModelPickerMessage::Resize { viewport_height: h });
                        }
                    }
                    Overlay::AddProvider => {
                        if let Some(h) = add_provider_kind_list_height(area)
                            && let Some(form) = &mut self.add_provider_form
                        {
                            form.update(AddProviderMessage::Resize { viewport_height: h });
                        }
                    }
                    Overlay::SessionPicker => {
                        if let Some(h) = session_picker_list_height(area) {
                            self.session_picker
                                .update(SessionPickerMessage::Resize { viewport_height: h });
                        }
                    }
                    Overlay::HistorySearch => {
                        if let Some(h) = history_search_list_height(area) {
                            self.history_search
                                .update(HistorySearchMessage::Resize { viewport_height: h });
                        }
                    }
                    Overlay::None | Overlay::Welcome | Overlay::ConfirmQuit => {}
                }
            }
            AppMessage::ConfigSaved => {
                self.reload_config();
            }
            AppMessage::ConfigError { error } => {
                if let Some(form) = &mut self.add_provider_form {
                    form.error = Some(error);
                }
            }
            AppMessage::ModelsLoaded {
                provider_name,
                models,
            } => {
                if self.pending_model_pick.as_deref() == Some(provider_name.as_str()) {
                    self.pending_model_pick = None;
                    if models.is_empty() {
                        self.session.update(SessionMessage::ShowError {
                            error: format!("{provider_name}: no models found"),
                        });
                    } else {
                        self.model_picker.open(&models);
                        self.overlay = Overlay::ModelPicker;
                    }
                    return None;
                }
                let empty = models.is_empty();
                self.models.insert(provider_name.clone(), models);
                if empty {
                    return None;
                }
                if Some(provider_name.as_str())
                    == self
                        .ctx
                        .connections
                        .active
                        .as_ref()
                        .map(|a| a.provider.as_str())
                {
                    let models = self.models.get(&provider_name).unwrap();
                    let current = self
                        .ctx
                        .connections
                        .active
                        .as_ref()
                        .and_then(|a| a.model.as_deref());
                    let chosen = current
                        .filter(|c| models.iter().any(|m| m.id == *c))
                        .map(|c| c.to_string())
                        .or_else(|| models.first().map(|m| m.id.clone()));
                    if let Some(model) = chosen
                        && self
                            .ctx
                            .connections
                            .active
                            .as_ref()
                            .and_then(|a| a.model.as_deref())
                            != Some(model.as_str())
                    {
                        self.ctx
                            .send(shuvarie_core::Command::SetActiveModel { model });
                    } else {
                        self.session.update(SessionMessage::UpdateConfig {
                            provider: self.provider_display_name(&provider_name),
                            model: current.map(|m| m.to_string()),
                            context_length: self.active_context_length(),
                        });
                    }
                }
            }
            AppMessage::ModelsError {
                provider_name,
                error,
            } => {
                if self.pending_model_pick.as_deref() == Some(provider_name.as_str()) {
                    self.pending_model_pick = None;
                    self.session.update(SessionMessage::ShowError {
                        error: format!("{provider_name}: {error}"),
                    });
                } else if let Some(form) = &mut self.add_provider_form {
                    form.error = Some(format!("{provider_name}: {error}"));
                }
            }
            AppMessage::SessionCreated { id, title } => {
                self.session.session_id = Some(id);
                self.session.session_title = Some(title);
                self.session_picker.active_id = Some(id);
            }
            AppMessage::SessionsLoaded { sessions } => {
                self.session_picker.set_sessions(sessions);
            }
            AppMessage::SessionLoaded { id, title, session } => {
                self.session
                    .update(SessionMessage::Loaded { id, title, session });
                self.session_picker.active_id = Some(id);
            }
            AppMessage::SessionDeleted { id } => {
                let was_active = self.session.session_id == Some(id);
                if was_active {
                    if let Some(rid) = self.session_picker.session_after(id) {
                        self.session.update(SessionMessage::Reset);
                        self.ctx
                            .send(shuvarie_core::Command::LoadSession { id: rid });
                        self.close_overlay();
                    } else {
                        self.session.update(SessionMessage::Reset);
                        self.close_overlay();
                    }
                } else {
                    self.refresh_sessions();
                }
            }
            AppMessage::SessionError { error } => {
                self.session.update(SessionMessage::ShowError { error });
            }
            AppMessage::LspStatus { servers } => {
                self.session
                    .sidebar
                    .update(SidebarMessage::UpdateLsp { servers });
            }
            AppMessage::LspDiagnostics { path, diagnostics } => {
                self.session
                    .update(SessionMessage::Chat(ChatMessage::LspDiagnostics {
                        path,
                        diagnostics,
                    }));
            }
            AppMessage::LspError { error } => {
                self.session.update(SessionMessage::ShowError { error });
            }
            AppMessage::SkillsLoaded { skills, warnings } => {
                self.session.sidebar.update(SidebarMessage::UpdateSkills {
                    skills: skills.clone(),
                    warnings,
                });
                self.session.update(SessionMessage::SetSkills { skills });
            }
            AppMessage::ShellWarning { message } => {
                self.warning.open(message);
            }
            AppMessage::Warning(m) => {
                self.warning.update(m);
            }
        }
        None
    }

    fn refresh_sessions(&mut self) {
        self.session_picker.loading = true;
        self.ctx.send(shuvarie_core::Command::ListSessions);
    }

    /// The spinners currently animating, so the render loop can wake at the
    /// earliest next frame change.
    pub fn active_spinners(&self) -> impl Iterator<Item = SpinnerKind> + '_ {
        let inline = self.session.busy
            || self.session.chat.has_running_tool_blocks()
            || self.history_search.loading
            || self.session_picker.loading
            || self.session.sidebar.lsp_servers.iter().any(|s| {
                matches!(
                    s.status,
                    shuvarie_core::ServerStatus::Starting | shuvarie_core::ServerStatus::Stopping
                )
            });
        self.session
            .busy_spinner()
            .into_iter()
            .chain(inline.then_some(SpinnerKind::Inline))
    }

    /// Mark the views that are animating dirty so the next frame re-renders them.
    pub fn mark_spinners_dirty(&self) {
        if self.session.busy || self.session.chat.has_running_tool_blocks() {
            self.session.mark_spinner_dirty();
        }
        if self.session.sidebar.lsp_servers.iter().any(|s| {
            matches!(
                s.status,
                shuvarie_core::ServerStatus::Starting | shuvarie_core::ServerStatus::Stopping
            )
        }) {
            self.session.sidebar.mark_dirty();
        }
    }

    fn update_command_availability(&mut self) {
        let has_messages = self.session.has_messages();
        self.command_menu
            .set_availability(CommandAction::UndoLastTurn, has_messages);
        self.command_menu
            .set_availability(CommandAction::Redo, has_messages);
        self.command_menu
            .set_availability(CommandAction::Replay, has_messages);
        let can_continue = self.session.can_continue();
        self.command_menu
            .set_availability(CommandAction::Continue, can_continue);
    }

    /// Runs a command action (from the Ctrl+M menu or the inline slash menu).
    /// Run a command action. Returns `Some(AppEffect)` when the action needs
    /// to escalate to the parent (quit).
    fn run_command(&mut self, action: CommandAction) -> Option<AppEffect> {
        match action {
            CommandAction::OpenModelSelect => {
                let models = self
                    .models
                    .get(
                        self.ctx
                            .connections
                            .active
                            .as_ref()
                            .map(|a| a.provider.as_str())
                            .unwrap_or(""),
                    )
                    .cloned()
                    .unwrap_or_default();
                self.model_picker.open(&models);
                self.overlay = Overlay::ModelPicker;
            }
            CommandAction::AddProvider => {
                let names: Vec<String> = self
                    .ctx
                    .connections
                    .providers
                    .values()
                    .map(|p| p.name.clone())
                    .collect();
                let providers = shuvarie_core::catalog::providers();
                self.add_provider_form = Some(AddProviderForm::new(providers, &names));
                self.overlay = Overlay::AddProvider;
            }
            CommandAction::OpenSessionPicker => {
                self.session_picker.open(self.session.session_id);
                self.refresh_sessions();
                self.overlay = Overlay::SessionPicker;
            }
            CommandAction::NewSession => {
                self.session.update(SessionMessage::Reset);
                self.ctx.send(shuvarie_core::Command::NewSession);
            }
            CommandAction::UndoLastTurn => {
                self.ctx.send(shuvarie_core::Command::UndoLastTurn);
            }
            CommandAction::Redo => {
                self.ctx.send(shuvarie_core::Command::Redo);
            }
            CommandAction::Replay => {
                self.ctx.send(shuvarie_core::Command::Replay);
            }
            CommandAction::Continue => {
                self.session.begin_continue();
                self.ctx.send(shuvarie_core::Command::Continue);
            }
            CommandAction::Reload => {
                self.ctx.send(shuvarie_core::Command::Reload);
            }
            CommandAction::Quit => {
                if self.session.is_streaming() {
                    self.ctx.send(shuvarie_core::Command::CancelStream);
                }
                return Some(AppEffect::Quit);
            }
        }
        None
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.command_menu.close();
        self.add_provider_form = None;
        self.model_picker.close();
        self.session_picker.close();
        self.history_search.close();
        self.confirm_quit.close();
        if self.welcome.open {
            self.welcome.close();
        }
    }

    fn active_context_length(&self) -> Option<u64> {
        catalog_context_length(&self.ctx.connections).or_else(|| {
            let active = self.ctx.connections.active.as_ref()?;
            let model = active.model.as_deref()?;
            self.models
                .get(&active.provider)?
                .iter()
                .find(|m| m.id == model)
                .and_then(|m| m.context_length.map(u64::from))
        })
    }

    fn provider_display_name(&self, id: &str) -> Option<String> {
        self.ctx
            .connections
            .providers
            .get(id)
            .map(|p| p.name.clone())
    }

    fn reload_config(&mut self) {
        if let Ok(fresh) = Connections::load() {
            self.ctx.connections = fresh;
            if self.ctx.connections.has_connected_providers() && self.welcome.open {
                self.welcome.close();
                self.overlay = Overlay::None;
            }
            self.session.update(SessionMessage::UpdateConfig {
                provider: self
                    .ctx
                    .connections
                    .active
                    .as_ref()
                    .and_then(|a| self.provider_display_name(&a.provider)),
                model: self
                    .ctx
                    .connections
                    .active
                    .as_ref()
                    .and_then(|a| a.model.clone()),
                context_length: self.active_context_length(),
            });
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        self.session.view(frame, area);

        self.welcome.view(frame, area);
        if let Some(form) = &self.add_provider_form {
            form.view(frame, area);
        }
        self.model_picker.view(frame, area);
        self.session_picker.view(frame, area);
        self.history_search.view(frame, area);
        self.command_menu.view(frame, area);
        self.confirm_quit.view(frame, area);
        self.warning.view(frame, area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}

fn overlay_inner_list_height(
    area: Rect,
    percent_x: u16,
    percent_y: u16,
    heading_len: u16,
) -> Option<u16> {
    let popup = centered_rect(percent_x, percent_y, area);
    if popup.width < 2 || popup.height < 2 {
        return None;
    }
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    if inner.height <= heading_len + 1 {
        return None;
    }
    Some(inner.height.saturating_sub(heading_len + 1))
}

fn command_menu_list_height(area: Rect) -> Option<u16> {
    overlay_inner_list_height(area, 60, 40, 1)
}

fn model_picker_list_height(area: Rect) -> Option<u16> {
    overlay_inner_list_height(area, 55, 50, 1)
}

fn add_provider_kind_list_height(area: Rect) -> Option<u16> {
    overlay_inner_list_height(area, 50, 55, 3)
}

fn session_picker_list_height(area: Rect) -> Option<u16> {
    overlay_inner_list_height(area, 64, 36, 1)
}

fn history_search_list_height(area: Rect) -> Option<u16> {
    overlay_inner_list_height(area, 64, 40, 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(connections: Connections) -> App {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        App::new(connections, tx)
    }

    fn connected() -> Connections {
        let mut connections = Connections::default();
        let provider = shuvarie_core::ProviderConfig::new("Anthropic", "anthropic", None, None);
        connections.providers.insert("anthropic".into(), provider);
        connections.active = Some(shuvarie_core::Active {
            provider: "anthropic".into(),
            model: Some("claude-sonnet-4-5".into()),
            variant: None,
        });
        connections
    }

    #[test]
    fn catalog_context_length_maps_catalog_and_alias_model_id() {
        let app = app_with(connected());
        assert_eq!(
            catalog_context_length(&app.ctx.connections),
            Some(200_000),
            "connection model alias `claude-sonnet-4-5` must map to the dated catalog entry"
        );
    }

    #[test]
    fn catalog_context_length_is_none_without_active_model() {
        let mut connections = connected();
        connections.active.as_mut().unwrap().model = None;
        assert_eq!(catalog_context_length(&connections), None);
    }

    #[test]
    fn catalog_context_length_uses_connection_catalog_field() {
        let mut connections = connected();
        let pc = connections.providers.get_mut("anthropic").unwrap();
        *pc = pc.clone().with_catalog(Some("anthropic"));
        assert_eq!(
            catalog_context_length(&connections),
            Some(200_000),
            "explicit `catalog` field must drive the Selune lookup"
        );
    }

    #[test]
    fn catalog_context_length_resolves_tagged_ollama_cloud_variant() {
        let mut connections = Connections::default();
        let provider = shuvarie_core::ProviderConfig::new("Ollama Cloud", "ollama", None, None)
            .with_catalog(Some("ollama-cloud"));
        connections
            .providers
            .insert("ollama-cloud".into(), provider);
        connections.active = Some(shuvarie_core::Active {
            provider: "ollama-cloud".into(),
            model: Some("glm-5.3-flash".into()),
            variant: None,
        });
        assert_eq!(
            catalog_context_length(&connections),
            Some(1_310_720),
            "untagged connection model `glm-5.3-flash` must map to the tagged catalog entry"
        );
    }

    #[test]
    fn window_title_falls_back_to_active_provider_and_model() {
        let app = app_with(connected());
        assert_eq!(
            app.window_title(),
            "Shuvarie — claude-sonnet-4-5 · Anthropic"
        );
    }

    #[test]
    fn window_title_shows_app_name_without_connections() {
        let app = app_with(Connections::default());
        assert_eq!(app.window_title(), "Shuvarie");
    }

    #[test]
    fn window_title_prefers_session_title() {
        let mut app = app_with(connected());
        app.session.session_title = Some("Fix the login bug".into());
        assert_eq!(app.window_title(), "Shuvarie — Fix the login bug");
    }

    #[test]
    fn window_title_uses_first_line_of_session_title() {
        let mut app = app_with(Connections::default());
        app.session.session_title = Some("multi\nline".into());
        assert_eq!(app.window_title(), "Shuvarie — multi");
    }

    #[test]
    fn window_title_falls_back_when_session_title_is_blank() {
        let mut app = app_with(connected());
        app.session.session_title = Some("\n".into());
        assert_eq!(
            app.window_title(),
            "Shuvarie — claude-sonnet-4-5 · Anthropic"
        );
    }
}
