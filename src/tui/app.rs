use std::collections::HashMap;

use ratatui::prelude::*;
use shuvarie_core::{Connections, Event as CoreEvent, Model, RegistryEntry, UiPrefs};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use tokio::sync::mpsc::Sender;

use crate::tui::event::Event;
use crate::tui::utils::ctrl;

use super::add_provider::{
    AddProviderForm, AddProviderMessage, AddProviderOutcome, AddProviderStage,
};
use super::auth::{AuthMessage, AuthPopup};
use super::command_menu::{CommandMenu, CommandMenuMessage};
use super::commands::CommandAction;
use super::components::TextAreaMessage;
use super::confirm_quit::{ConfirmQuit, ConfirmQuitEffect, ConfirmQuitMessage};
use super::context::UpdateCtx;
use super::history_search::{HistorySearch, HistorySearchEffect, HistorySearchMessage};
use super::model_picker::{ModelPicker, ModelPickerEffect, ModelPickerMessage};
use super::scene;
use super::search::SearchMessage;
use super::session::tree::{TreeEffect, TreeMessage, TreePopup};
use super::session::{
    BashMessage, ChatMessage, MouseKind, SessionEffect, SessionMessage, SessionScreen,
};
use super::session_picker::{SessionPicker, SessionPickerEffect, SessionPickerMessage};
use super::sidebar::SidebarMessage;
use super::spinner::SpinnerKind;
use super::theme;
use super::title::{TitleEffect, TitleMessage, TitlePopup};
use super::variant;
use super::warning::{WarningMessage, WarningPopup};
use super::welcome::{Welcome, WelcomeEffect, WelcomeMessage};
use super::workspace::WorkspaceInfo;

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
    TitleEdit,
    Tree,
    Scene,
    Variant,
}

pub enum AppMessage {
    OpenCommandMenu,
    /// Cycle the active model's reasoning-effort variant (Ctrl+T).
    CycleModelVariant,
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
    Tree(TreeMessage),
    Scene(scene::SceneMessage),
    Variant(variant::VariantMessage),
    /// The configured scene set (startup), for the switcher; `warnings`
    /// carries one message per same-level scene conflict.
    ScenesLoaded {
        scenes: Vec<shuvarie_core::scenes::SceneListEntry>,
        default: Option<String>,
        warnings: Vec<String>,
    },
    /// The session's scene changed (`None` = built-in Default).
    SceneChanged {
        name: Option<String>,
    },
    HistorySearch(HistorySearchMessage),
    TitlePopup(TitleMessage),
    ConfigSaved,
    ConfigError {
        error: String,
    },
    /// The on-demand hosted registry fetch (from either selector popup)
    /// succeeded.
    RegistryLoaded {
        providers: Vec<selune::Provider>,
    },
    /// The on-demand hosted registry fetch failed.
    RegistryError {
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
    /// The freshly loaded session tree for the `/tree` popup.
    SessionTree {
        session: shuvarie_core::Session,
    },
    SessionLoaded {
        id: uuid::Uuid,
        title: String,
        session: shuvarie_core::Session,
    },
    SessionCreated {
        id: uuid::Uuid,
        title: String,
        scene: Option<String>,
    },
    SessionTitleChanged {
        id: uuid::Uuid,
        title: String,
    },
    SessionDeleted {
        id: uuid::Uuid,
    },
    SessionLocked,
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
    McpStatus {
        servers: Vec<shuvarie_core::McpStatus>,
    },
    McpError {
        error: String,
    },
    SkillsLoaded {
        skills: Vec<shuvarie_core::Skill>,
        warnings: Vec<shuvarie_core::SkillWarning>,
    },
    ShellWarning {
        message: String,
    },
    Auth(AuthMessage),
    Warning(WarningMessage),
    /// The render loop's spinner wake: refresh the animated spinner renders.
    SpinnerUpdate,
    PickerRefresh,
}

#[derive(Debug)]
pub enum AppEffect {
    Quit,
    CopyToClipboard(String),
    /// Launch the OAuth verification URL in the platform browser.
    OpenBrowser(String),
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
    pub tree_popup: TreePopup,
    pub scene_picker: scene::ScenePicker,
    pub variant_picker: variant::VariantPicker,
    pub history_search: HistorySearch,
    pub title_popup: TitlePopup,
    pub warning: WarningPopup,
    pub auth: AuthPopup,
    pub models: HashMap<String, Vec<Model>>,
    registry: RegistryEntry,
    pending_model_pick: Option<String>,
    /// The configured scene set (built-in Default first) for the switcher.
    scene_entries: Vec<shuvarie_core::scenes::SceneListEntry>,
    /// The scene new sessions start under (`None` = built-in Default).
    scene_default: Option<String>,
    /// The session's current scene (`None` = built-in Default).
    current_scene: Option<String>,
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

/// The active model's reasoning-effort variants from the Selune catalog, in
/// declared order — the list the `/variant` selector and its argument
/// validate against. `None` without an active provider/model or when the
/// catalog model declares no variants.
fn catalog_variants(connections: &Connections) -> Option<Vec<String>> {
    let active = connections.active.as_ref()?;
    let model = active.model.as_deref()?;
    let id = connections
        .providers
        .get(&active.provider)
        .and_then(|pc| pc.catalog_id())?;
    let providers = shuvarie_core::catalog::providers();
    let provider = shuvarie_core::catalog::find_provider(&providers, id)?;
    let entry = shuvarie_core::catalog::find_model(provider, model)?;
    let variants = shuvarie_core::catalog::model_variants(entry);
    (!variants.is_empty()).then(|| variants.to_vec())
}

impl App {
    pub fn new(
        ui: UiPrefs,
        theme: shuvarie_core::ResolvedTheme,
        registry: RegistryEntry,
        connections: Connections,
        cmd_tx: Sender<shuvarie_core::Command>,
        viewport_cols: u16,
        workspace: WorkspaceInfo,
    ) -> Self {
        let mut welcome = Welcome::new();
        if !shuvarie_core::catalog::has_connected_providers(&connections) {
            welcome.open();
        }
        let mut warning = WarningPopup::new();
        if !theme.warnings.is_empty() {
            warning.open(theme.warnings.join("\n"));
        }
        theme::init(theme);

        let initial_provider = connections.active.as_ref().map(|a| a.provider.clone());
        let initial_model = connections.active.as_ref().and_then(|a| a.model.clone());
        let initial_variant = connections.active.as_ref().and_then(|a| a.variant.clone());
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
                    variant: initial_variant,
                    context_length: initial_context_length,
                });
                s.sidebar
                    .update(SidebarMessage::SetPref { pref: ui.sidebar });
                s.update(SessionMessage::SetCopyOnSelect {
                    enabled: ui.copy_on_select,
                });
                s.sidebar.update(SidebarMessage::SetWidth {
                    cols: viewport_cols,
                });
                s.sidebar.update(SidebarMessage::SetWorkspace { workspace });
                s
            },
            command_menu: CommandMenu::new(),
            welcome,
            confirm_quit: ConfirmQuit::new(),
            add_provider_form: None,
            model_picker: ModelPicker::new(),
            session_picker: SessionPicker::new(),
            tree_popup: TreePopup::new(),
            scene_picker: scene::ScenePicker::new(),
            variant_picker: variant::VariantPicker::new(),
            history_search: HistorySearch::new(),
            title_popup: TitlePopup::new(),
            warning,
            auth: AuthPopup::new(),
            models: HashMap::new(),
            registry,
            pending_model_pick: None,
            scene_entries: Vec::new(),
            scene_default: None,
            current_scene: None,
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
                    MouseEventKind::Down(MouseButton::Left) => {
                        Some(AppMessage::Session(SessionMessage::Mouse {
                            kind: MouseKind::Down,
                            column: mouse.column,
                            row: mouse.row,
                        }))
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        Some(AppMessage::Session(SessionMessage::Mouse {
                            kind: MouseKind::Drag,
                            column: mouse.column,
                            row: mouse.row,
                        }))
                    }
                    MouseEventKind::Up(MouseButton::Left) => {
                        Some(AppMessage::Session(SessionMessage::Mouse {
                            kind: MouseKind::Up,
                            column: mouse.column,
                            row: mouse.row,
                        }))
                    }
                    MouseEventKind::ScrollUp => Some(AppMessage::Session(SessionMessage::Wheel {
                        up: true,
                        column: mouse.column,
                        row: mouse.row,
                    })),
                    MouseEventKind::ScrollDown => {
                        Some(AppMessage::Session(SessionMessage::Wheel {
                            up: false,
                            column: mouse.column,
                            row: mouse.row,
                        }))
                    }
                    _ => None,
                },
                TermEvent::Paste(text) => self.map_paste(&text),
                TermEvent::Key(key) => {
                    // Transient OAuth device-flow prompt: swallows one key
                    // press (o/Enter opens the browser, others dismiss),
                    // leaving the underlying overlay untouched.
                    if self.auth.open {
                        return self.auth.map_event(&key).map(AppMessage::Auth);
                    }

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
                        Overlay::Tree => {
                            return self.tree_popup.map_event(&key).map(AppMessage::Tree);
                        }
                        Overlay::Scene => {
                            return self.scene_picker.map_event(&key).map(AppMessage::Scene);
                        }
                        Overlay::Variant => {
                            return self.variant_picker.map_event(&key).map(AppMessage::Variant);
                        }
                        Overlay::HistorySearch => {
                            return self
                                .history_search
                                .map_event(&key)
                                .map(AppMessage::HistorySearch);
                        }
                        Overlay::TitleEdit => {
                            return self.title_popup.map_event(&key).map(AppMessage::TitlePopup);
                        }
                        Overlay::None => {}
                    }

                    // App key event
                    #[allow(clippy::single_match)]
                    match key.kind {
                        KeyEventKind::Press => match key.code {
                            KeyCode::Char('c') if ctrl(&key) => {
                                if self.session.input.buffer.selection().is_some()
                                    || self.session.chat.has_selection()
                                {
                                    return Some(AppMessage::Session(
                                        SessionMessage::CopySelection,
                                    ));
                                }
                                if !self.session.input.is_empty() {
                                    return Some(AppMessage::Session(SessionMessage::Text(
                                        TextAreaMessage::Clear,
                                    )));
                                }
                                return Some(AppMessage::RequestQuit);
                            }
                            KeyCode::Char('x') if ctrl(&key) => {
                                return Some(AppMessage::Session(SessionMessage::CutSelection));
                            }
                            KeyCode::Char('m') if ctrl(&key) => {
                                return Some(AppMessage::OpenCommandMenu);
                            }
                            KeyCode::Char('r') if ctrl(&key) => {
                                return Some(AppMessage::HistorySearch(HistorySearchMessage::Open));
                            }
                            KeyCode::Char('t') | KeyCode::Char('T') if ctrl(&key) => {
                                return Some(AppMessage::CycleModelVariant);
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
                CoreEvent::RegistryLoaded { providers } => {
                    Some(AppMessage::RegistryLoaded { providers })
                }
                CoreEvent::RegistryError { error } => Some(AppMessage::RegistryError { error }),
                CoreEvent::SessionStarted => Some(AppMessage::SceneChanged {
                    name: self.scene_default.clone(),
                }),
                CoreEvent::SessionCreated { id, title, scene } => {
                    Some(AppMessage::SessionCreated { id, title, scene })
                }
                CoreEvent::SessionTitleChanged { id, title } => {
                    Some(AppMessage::SessionTitleChanged { id, title })
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
                    call_id,
                    stdout,
                    stderr,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::ToolOutput {
                        tool,
                        worker,
                        call_id,
                        stdout,
                        stderr,
                    },
                ))),
                CoreEvent::BashStarted { id, command } => Some(AppMessage::Session(
                    SessionMessage::Bash(BashMessage::Started { id, command }),
                )),
                CoreEvent::BashOutput { id, stdout, stderr } => Some(AppMessage::Session(
                    SessionMessage::Bash(BashMessage::Output { id, stdout, stderr }),
                )),
                CoreEvent::BashFinished {
                    id,
                    ok,
                    exit,
                    stdout,
                    stderr,
                    duration_ms,
                } => Some(AppMessage::Session(SessionMessage::Bash(
                    BashMessage::Finished {
                        id,
                        ok,
                        exit,
                        stdout,
                        stderr,
                        duration_ms,
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
                CoreEvent::SessionLocked { .. } => Some(AppMessage::SessionLocked),
                CoreEvent::SessionError { error } => Some(AppMessage::SessionError { error }),
                CoreEvent::Forked { session, prompt } => {
                    Some(AppMessage::Session(SessionMessage::Forked {
                        session,
                        prompt,
                    }))
                }
                CoreEvent::SessionTree { session } => Some(AppMessage::SessionTree { session }),
                CoreEvent::SessionExported { path } => {
                    Some(AppMessage::Session(SessionMessage::ShowStatus {
                        status: format!("Exported to {}", path.display()),
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
                CoreEvent::PermissionRequested {
                    id,
                    description,
                    allow_session,
                } => Some(AppMessage::Session(SessionMessage::PermissionRequested {
                    id,
                    description,
                    allow_session,
                })),
                CoreEvent::LspStatus { servers } => Some(AppMessage::LspStatus { servers }),
                CoreEvent::LspDiagnostics { path, diagnostics } => {
                    Some(AppMessage::LspDiagnostics { path, diagnostics })
                }
                CoreEvent::LspError { error } => Some(AppMessage::LspError { error }),
                CoreEvent::McpStatus { servers } => Some(AppMessage::McpStatus { servers }),
                CoreEvent::McpError { error } => Some(AppMessage::McpError { error }),
                CoreEvent::SkillsLoaded { skills, warnings } => {
                    Some(AppMessage::SkillsLoaded { skills, warnings })
                }
                CoreEvent::ScenesLoaded {
                    scenes,
                    default,
                    warnings,
                } => Some(AppMessage::ScenesLoaded {
                    scenes,
                    default,
                    warnings,
                }),
                CoreEvent::SceneChanged { name } => Some(AppMessage::SceneChanged { name }),
                CoreEvent::SceneError { error } => {
                    Some(AppMessage::Session(SessionMessage::ShowError { error }))
                }
                CoreEvent::ShellWarning { message } => Some(AppMessage::ShellWarning { message }),
                CoreEvent::AuthPrompt {
                    provider,
                    verification_uri,
                    user_code,
                } => Some(AppMessage::Auth(AuthMessage::Prompt {
                    provider,
                    verification_uri,
                    user_code,
                })),
                CoreEvent::AuthSuccess { provider } => {
                    Some(AppMessage::Auth(AuthMessage::Succeeded { provider }))
                }
                CoreEvent::AuthFailed { provider, error } => {
                    Some(AppMessage::Auth(AuthMessage::Failed { provider, error }))
                }
            },
        }
    }

    /// Bracketed-paste payload routing. Text-entry surfaces accept the paste;
    /// navigation-only overlays and the transient popups drop it.
    fn map_paste(&self, text: &str) -> Option<AppMessage> {
        if self.auth.open || self.warning.open {
            return None;
        }
        match self.overlay {
            Overlay::None => self.session.map_paste(text).map(AppMessage::Session),
            Overlay::HistorySearch => Some(AppMessage::HistorySearch(HistorySearchMessage::Paste(
                text.to_string(),
            ))),
            Overlay::AddProvider => self
                .add_provider_form
                .as_ref()
                .map(|_| AppMessage::AddProvider(AddProviderMessage::Paste(text.to_string()))),
            Overlay::Welcome
            | Overlay::CommandMenu
            | Overlay::ConfirmQuit
            | Overlay::SessionPicker
            | Overlay::Tree
            | Overlay::Scene
            | Overlay::Variant => None,
            Overlay::ModelPicker => {
                let flat = super::components::flatten_newlines(text);
                let mut msg = None;
                for c in flat.chars() {
                    msg = Some(AppMessage::ModelPicker(ModelPickerMessage::Search(
                        SearchMessage::Input(c),
                    )));
                }
                msg
            }
            Overlay::TitleEdit => Some(AppMessage::TitlePopup(TitleMessage::Paste(
                text.to_string(),
            ))),
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
            AppMessage::CycleModelVariant => {
                self.ctx.send(shuvarie_core::Command::CycleVariant);
            }
            AppMessage::RequestQuit => {
                self.confirm_quit.open();
                self.overlay = Overlay::ConfirmQuit;
            }
            AppMessage::ConfirmQuit => {
                if let Some(ConfirmQuitEffect::Confirm) =
                    self.confirm_quit.update(ConfirmQuitMessage::Confirm)
                {
                    self.save_scroll();
                    return Some(AppEffect::Quit);
                }
            }
            AppMessage::CancelQuit => {
                self.confirm_quit.update(ConfirmQuitMessage::Cancel);
                self.overlay = Overlay::None;
            }
            AppMessage::Session(m) => {
                let effect = self.session.update(m);
                if self.tree_popup.open {
                    self.tree_popup.set_busy(self.session.is_busy());
                }
                if let Some(effect) = effect {
                    match effect {
                        SessionEffect::SendMessage { content } => {
                            self.ctx
                                .send(shuvarie_core::Command::SendMessage { content });
                        }
                        SessionEffect::RunBash { command } => {
                            self.ctx.send(shuvarie_core::Command::RunBash { command });
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
                        SessionEffect::PermissionDecide { id, decision } => {
                            self.ctx
                                .send(shuvarie_core::Command::PermissionDecide { id, decision });
                        }
                        SessionEffect::RunCommand { action, args } => {
                            if let Some(effect) = self.run_command(action, args) {
                                return Some(effect);
                            }
                        }
                        SessionEffect::CopyToClipboard { text } => {
                            return Some(AppEffect::CopyToClipboard(text));
                        }
                    }
                }
            }
            AppMessage::SessionTree { session } => {
                if self.tree_popup.open {
                    self.tree_popup.set_rows(&session);
                } else {
                    self.tree_popup.open(&session);
                    self.overlay = Overlay::Tree;
                }
                self.tree_popup.set_busy(self.session.is_busy());
            }
            AppMessage::Scene(m) => {
                if let Some(effect) = self.scene_picker.update(m) {
                    match effect {
                        scene::SceneEffect::Switch { name } => {
                            self.close_overlay();
                            self.ctx.send(shuvarie_core::Command::SwitchScene { name });
                        }
                        scene::SceneEffect::Close => self.close_overlay(),
                    }
                }
            }
            AppMessage::Variant(m) => {
                if let Some(effect) = self.variant_picker.update(m) {
                    match effect {
                        variant::VariantEffect::Set { variant } => {
                            self.close_overlay();
                            self.ctx
                                .send(shuvarie_core::Command::SelectVariant { variant });
                        }
                        variant::VariantEffect::Close => self.close_overlay(),
                    }
                }
            }
            AppMessage::SessionPicker(m) => {
                if let Some(effect) = self.session_picker.update(m) {
                    match effect {
                        SessionPickerEffect::LoadSession { id } => {
                            self.save_scroll();
                            self.ctx.send(shuvarie_core::Command::LoadSession { id });
                            self.close_overlay();
                        }
                        SessionPickerEffect::DeleteSession { id } => {
                            self.ctx.send(shuvarie_core::Command::DeleteSession { id });
                        }
                        SessionPickerEffect::NewSession => {
                            self.save_scroll();
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
            AppMessage::Tree(m) => {
                if let Some(effect) = self.tree_popup.update(m) {
                    match effect {
                        TreeEffect::Fork { node, summarize } => {
                            self.close_overlay();
                            self.ctx.send(shuvarie_core::Command::ForkSession {
                                node: Some(node),
                                summarize,
                            });
                        }
                        TreeEffect::DeleteBranch { node } => {
                            self.ctx.send(shuvarie_core::Command::DeleteBranch { node });
                        }
                        TreeEffect::Close => self.close_overlay(),
                    }
                }
            }
            AppMessage::TitlePopup(m) => {
                if let Some(effect) = self.title_popup.update(m) {
                    match effect {
                        TitleEffect::Set { title } => {
                            self.close_overlay();
                            self.ctx.send(shuvarie_core::Command::SetTitle { title });
                        }
                        TitleEffect::Close => self.close_overlay(),
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
                        AddProviderOutcome::FetchRegistry => {
                            self.ctx.send(shuvarie_core::Command::FetchRegistry);
                        }
                        AddProviderOutcome::Submit {
                            kind,
                            catalog,
                            name,
                            api_key,
                            base_url,
                        } => {
                            let id = uuid::Uuid::now_v7().to_string();
                            let pc = shuvarie_core::ProviderConfig::new(
                                name.clone(),
                                kind,
                                api_key,
                                base_url,
                            )
                            .with_catalog(catalog);
                            // OAuth2 device-flow providers sign in right
                            // away, so the verification URL + user code
                            // surface immediately.
                            let oauth_login = shuvarie_core::catalog::oauth_device_login(&pc);
                            self.pending_model_pick = Some(id.clone());
                            self.ctx.send(shuvarie_core::Command::AddProvider {
                                id: id.clone(),
                                config: pc,
                            });
                            self.ctx.send(shuvarie_core::Command::SetActiveProvider {
                                name: id.clone(),
                            });
                            self.ctx.send(shuvarie_core::Command::ListModels {
                                provider_name: id.clone(),
                            });
                            if oauth_login {
                                self.ctx
                                    .send(shuvarie_core::Command::AuthProviderLogin { name: id });
                            }
                            self.close_overlay();
                        }
                        AddProviderOutcome::None => {}
                    }
                }
            }
            AppMessage::ModelPicker(m) => {
                if let Some(effect) = self.model_picker.update(m) {
                    match effect {
                        ModelPickerEffect::Selected { provider, model } => {
                            if let Some(provider) = provider
                                && self
                                    .ctx
                                    .connections
                                    .active
                                    .as_ref()
                                    .map(|a| a.provider.as_str())
                                    != Some(provider.as_str())
                            {
                                self.ctx.send(shuvarie_core::Command::SetActiveProvider {
                                    name: provider,
                                });
                            }
                            self.ctx
                                .send(shuvarie_core::Command::SetActiveModel { model });
                        }
                        ModelPickerEffect::FetchRegistry => {
                            self.ctx.send(shuvarie_core::Command::FetchRegistry);
                        }
                        ModelPickerEffect::Close => {
                            if self
                                .ctx
                                .connections
                                .active
                                .as_ref()
                                .is_none_or(|a| a.model.is_none())
                                && let Some(first) = self.model_picker.active_default_model()
                            {
                                self.ctx
                                    .send(shuvarie_core::Command::SetActiveModel { model: first });
                            }
                        }
                    }
                    self.close_overlay();
                }
            }
            AppMessage::CommandMenu(m) => {
                if let Some(action) = self.command_menu.update(m)
                    && let Some(effect) = self.run_command(action, None)
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
                            self.save_scroll();
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
                        WelcomeEffect::AddProvider => self.open_add_provider(),
                    }
                }
            }
            AppMessage::Resized { rows, cols } => {
                self.session
                    .sidebar
                    .update(SidebarMessage::SetWidth { cols });
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
                        if let Some(form) = &mut self.add_provider_form {
                            let heading_len = match form.stage {
                                AddProviderStage::Select => Some(3),
                                AddProviderStage::KindList => Some(1),
                                AddProviderStage::Details => None,
                            };
                            if let Some(h) =
                                heading_len.and_then(|h| add_provider_list_height(area, h))
                            {
                                form.update(AddProviderMessage::Resize { viewport_height: h });
                            }
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
                    Overlay::None
                    | Overlay::Welcome
                    | Overlay::ConfirmQuit
                    | Overlay::TitleEdit
                    | Overlay::Tree
                    | Overlay::Scene
                    | Overlay::Variant => {}
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
            AppMessage::RegistryLoaded { providers } => match self.overlay {
                Overlay::AddProvider => {
                    if let Some(form) = &mut self.add_provider_form {
                        form.update(AddProviderMessage::RegistryLoaded { providers });
                    }
                }
                Overlay::ModelPicker => {
                    self.model_picker
                        .update(ModelPickerMessage::RegistryLoaded { providers });
                }
                _ => {}
            },
            AppMessage::RegistryError { error } => match self.overlay {
                Overlay::AddProvider => {
                    if let Some(form) = &mut self.add_provider_form {
                        form.update(AddProviderMessage::RegistryError {
                            error: error.clone(),
                        });
                    }
                }
                Overlay::ModelPicker => {
                    self.model_picker.source.on_error(error);
                }
                _ => {}
            },
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
                        self.models.insert(provider_name.clone(), models.clone());
                        self.open_model_picker();
                    }
                    return None;
                }
                let empty = models.is_empty();
                self.models.insert(provider_name.clone(), models);
                if self.model_picker.open {
                    self.model_picker
                        .update(ModelPickerMessage::ProviderModels {
                            provider_name: provider_name.clone(),
                            models: self.models.get(&provider_name).cloned().unwrap_or_default(),
                        });
                }
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
                            variant: self
                                .ctx
                                .connections
                                .active
                                .as_ref()
                                .and_then(|a| a.variant.clone()),
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
                } else if self.model_picker.open {
                    self.model_picker
                        .update(ModelPickerMessage::ProviderModels {
                            provider_name,
                            models: Vec::new(),
                        });
                } else if let Some(form) = &mut self.add_provider_form {
                    form.error = Some(format!("{provider_name}: {error}"));
                }
            }
            AppMessage::SessionCreated { id, title, scene } => {
                self.session.session_id = Some(id);
                self.session.session_title = Some(title);
                self.session_picker.active_id = Some(id);
                self.set_scene(scene);
            }
            AppMessage::SessionTitleChanged { id, title } => {
                // A late background generation for a switched-away session
                // must not clobber the active session's header.
                if self.session.session_id == Some(id) {
                    self.session.session_title = Some(title);
                }
            }
            AppMessage::SessionsLoaded { sessions } => {
                self.session_picker.set_sessions(sessions);
            }
            AppMessage::SessionLoaded { id, title, session } => {
                let scene = session.scene.clone();
                self.session
                    .update(SessionMessage::Loaded { id, title, session });
                self.session_picker.active_id = Some(id);
                self.set_scene(scene);
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
            AppMessage::SessionLocked => {
                if self.session_picker.open {
                    self.ctx.send(shuvarie_core::Command::ListSessions);
                } else {
                    self.warning
                        .open("This session is in use in another shuvarie instance.".into());
                }
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
            AppMessage::McpStatus { servers } => {
                self.session
                    .sidebar
                    .update(SidebarMessage::UpdateMcp { servers });
            }
            AppMessage::McpError { error } => {
                self.session.update(SessionMessage::ShowError { error });
            }
            AppMessage::SkillsLoaded { skills, warnings } => {
                self.session.sidebar.update(SidebarMessage::UpdateSkills {
                    skills: skills.clone(),
                    warnings,
                });
                self.session.update(SessionMessage::SetSkills { skills });
            }
            AppMessage::ScenesLoaded {
                scenes,
                default,
                warnings,
            } => {
                self.scene_entries = scenes;
                self.scene_default = default.clone();
                if self.session.session_id.is_none() {
                    self.set_scene(default);
                }
                if !warnings.is_empty() {
                    self.warning.open(warnings.join("\n"));
                }
            }
            AppMessage::SceneChanged { name } => {
                self.set_scene(name);
            }
            AppMessage::ShellWarning { message } => {
                self.warning.open(message);
            }
            AppMessage::Warning(m) => {
                self.warning.update(m);
            }
            AppMessage::Auth(AuthMessage::Prompt {
                provider,
                verification_uri,
                user_code,
            }) => {
                self.auth.start(provider, verification_uri, user_code);
            }
            AppMessage::Auth(AuthMessage::Succeeded { provider }) => {
                if self.auth.matches(&provider) {
                    self.auth.close();
                }
                self.warning.open(format!("Signed in with {provider}."));
            }
            AppMessage::Auth(AuthMessage::Failed { provider, error }) => {
                if self.auth.matches(&provider) {
                    self.auth.close();
                }
                self.warning
                    .open(format!("Sign-in with {provider} failed: {error}"));
            }
            AppMessage::Auth(AuthMessage::Dismiss) => self.auth.close(),
            AppMessage::Auth(AuthMessage::OpenBrowser { url }) => {
                // The popup stays up while the provider polls; the browser
                // launch is fire-and-forget.
                return Some(AppEffect::OpenBrowser(url));
            }
            AppMessage::Auth(AuthMessage::CopyUrl { url }) => {
                self.auth.copied("URL copied to clipboard");
                return Some(AppEffect::CopyToClipboard(url));
            }
            AppMessage::Auth(AuthMessage::CopyCode { code }) => {
                self.auth.copied("Code copied to clipboard");
                return Some(AppEffect::CopyToClipboard(code));
            }
            AppMessage::SpinnerUpdate => {
                self.session.update(SessionMessage::SpinnerUpdate);
            }
            AppMessage::PickerRefresh => {
                if self.session_picker.open {
                    self.ctx.send(shuvarie_core::Command::ListSessions);
                }
            }
        }
        None
    }

    /// Persists the chat pane's scroll position for the session being left
    /// (switch, reset, or quit) before the transition is dispatched; a no-op
    /// when no session row is loaded.
    fn save_scroll(&self) {
        if let Some((id, scroll)) = self.session.scroll_save() {
            self.ctx
                .send(shuvarie_core::Command::SaveScroll { id, scroll });
        }
    }

    /// Opens the provider selector popup, seeding its registry source from
    /// the `[registries] selune` entry.
    fn open_add_provider(&mut self) {
        let names: Vec<String> = self
            .ctx
            .connections
            .providers
            .values()
            .map(|p| p.name.clone())
            .collect();
        let catalog_ids: Vec<String> = self
            .ctx
            .connections
            .providers
            .values()
            .filter_map(|p| p.catalog_id().map(str::to_string))
            .collect();
        let form = AddProviderForm::new(&names, &catalog_ids, self.registry);
        if form.needs_fetch() {
            self.ctx.send(shuvarie_core::Command::FetchRegistry);
        }
        self.add_provider_form = Some(form);
        self.overlay = Overlay::AddProvider;
    }

    /// Opens the model selector popup over every configured provider.
    /// Catalog-less providers whose models are not cached yet get a live
    /// `ListModels` fetch.
    fn open_model_picker(&mut self) {
        self.model_picker
            .open(&self.ctx.connections, &self.models, self.registry);
        if self.model_picker.needs_fetch() {
            self.ctx.send(shuvarie_core::Command::FetchRegistry);
        }
        for provider_name in self.model_picker.pending_live_providers() {
            self.ctx
                .send(shuvarie_core::Command::ListModels { provider_name });
        }
        self.overlay = Overlay::ModelPicker;
    }

    fn refresh_sessions(&mut self) {
        self.session_picker.loading = true;
        self.ctx.send(shuvarie_core::Command::ListSessions);
    }

    /// Opens the variant selector, or with an argument (`/variant <name>`)
    /// picks it directly: `default` clears the variant, a declared value
    /// (case-insensitive) selects it in canonical casing, anything else shows
    /// an error listing what the model declares.
    fn open_variant_picker(&mut self, args: Option<String>) {
        let Some(active) = self.ctx.connections.active.as_ref() else {
            self.session.update(SessionMessage::ShowError {
                error: "no active model".into(),
            });
            return;
        };
        let Some(model) = active.model.clone() else {
            self.session.update(SessionMessage::ShowError {
                error: "no active model".into(),
            });
            return;
        };
        let current = active.variant.clone();
        match catalog_variants(&self.ctx.connections) {
            Some(variants) => match args {
                None => {
                    self.variant_picker.open(&variants, current.as_deref());
                    self.overlay = Overlay::Variant;
                }
                Some(arg) => {
                    if let Some(variant) = variant::resolve_arg(&variants, &arg) {
                        self.ctx
                            .send(shuvarie_core::Command::SelectVariant { variant });
                    } else {
                        self.session.update(SessionMessage::ShowError {
                            error: format!(
                                "unknown variant `{arg}` — available: {}, {}",
                                variant::DEFAULT_VARIANT,
                                variants.join(", ")
                            ),
                        });
                    }
                }
            },
            None => {
                self.session.update(SessionMessage::ShowError {
                    error: format!("model {model} has no variants"),
                });
            }
        }
    }

    /// Records the session's current scene (`None` = built-in Default) and
    /// updates the sidebar's scene line.
    fn set_scene(&mut self, name: Option<String>) {
        self.current_scene = name.clone();
        self.session
            .sidebar
            .update(SidebarMessage::SetScene { name });
    }

    /// The spinners currently animating, so the render loop can wake at the
    /// earliest next frame change.
    pub fn active_spinners(&self) -> impl Iterator<Item = SpinnerKind> + '_ {
        let inline = self.session.is_busy()
            || self.session.chat.has_running_tool_blocks()
            || self.session.bash.running()
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

    fn update_command_availability(&mut self) {
        let has_messages = self.session.has_messages();
        self.command_menu
            .set_availability(CommandAction::UndoLastTurn, has_messages);
        self.command_menu
            .set_availability(CommandAction::Replay, has_messages);
        // The tree stays openable on a cleared (empty) active path — it is the
        // way back to a forked-away branch.
        self.command_menu
            .set_availability(CommandAction::OpenTree, self.session.session_id.is_some());
        self.command_menu
            .set_availability(CommandAction::EditTitle, self.session.session_id.is_some());
        self.command_menu
            .set_availability(CommandAction::Export, self.session.session_id.is_some());
    }

    /// Runs a command action (from the Ctrl+M menu or the inline slash menu).
    /// Run a command action. Returns `Some(AppEffect)` when the action needs
    /// to escalate to the parent (quit).
    fn run_command(&mut self, action: CommandAction, args: Option<String>) -> Option<AppEffect> {
        match action {
            CommandAction::OpenModelSelect => {
                self.open_model_picker();
            }
            CommandAction::AddProvider => {
                self.open_add_provider();
            }
            CommandAction::OpenSessionPicker => {
                self.session_picker.open(self.session.session_id);
                self.refresh_sessions();
                self.overlay = Overlay::SessionPicker;
            }
            CommandAction::OpenTree => {
                if self.session.session_id.is_none() {
                    self.session.update(SessionMessage::ShowError {
                        error: "no active session".into(),
                    });
                } else {
                    self.ctx.send(shuvarie_core::Command::OpenTree);
                }
            }
            CommandAction::OpenScenePicker => {
                if let Some(name) = args {
                    let configured = self
                        .scene_entries
                        .iter()
                        .any(|entry| entry.id.as_deref() == Some(name.as_str()));
                    let name = if configured || name != shuvarie_core::scenes::DEFAULT_SCENE_NAME {
                        Some(name)
                    } else {
                        None
                    };
                    self.ctx.send(shuvarie_core::Command::SwitchScene { name });
                } else {
                    self.scene_picker.open(
                        self.scene_entries.clone(),
                        self.current_scene.as_deref(),
                        self.session.has_messages(),
                    );
                    self.overlay = Overlay::Scene;
                }
            }
            CommandAction::OpenVariantPicker => {
                self.open_variant_picker(args);
            }
            CommandAction::NewSession => {
                self.save_scroll();
                self.session.update(SessionMessage::Reset);
                self.ctx.send(shuvarie_core::Command::NewSession);
            }
            CommandAction::EditTitle => {
                if self.session.session_id.is_none() {
                    self.session.update(SessionMessage::ShowError {
                        error: "no active session".into(),
                    });
                } else if let Some(title) = args {
                    self.ctx.send(shuvarie_core::Command::SetTitle { title });
                } else {
                    self.title_popup.open(self.session.session_title.as_deref());
                    self.overlay = Overlay::TitleEdit;
                }
            }
            CommandAction::Export => {
                if self.session.session_id.is_none() {
                    self.session.update(SessionMessage::ShowError {
                        error: "no active session".into(),
                    });
                } else {
                    self.ctx.send(shuvarie_core::Command::ExportSession {
                        path: args.map(std::path::PathBuf::from),
                    });
                }
            }
            CommandAction::UndoLastTurn => {
                self.ctx.send(shuvarie_core::Command::ForkSession {
                    node: None,
                    summarize: false,
                });
            }
            CommandAction::Replay => {
                self.ctx.send(shuvarie_core::Command::Replay);
            }
            CommandAction::Reload => {
                self.ctx.send(shuvarie_core::Command::Reload);
            }
            CommandAction::McpServers => {
                self.ctx.send(shuvarie_core::Command::McpList);
            }
            CommandAction::McpReconnect => {
                match args.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                    Some(name) => {
                        self.ctx
                            .send(shuvarie_core::Command::McpReconnect { name: name.into() });
                    }
                    None => {
                        self.session.update(SessionMessage::ShowError {
                            error: "usage: /mcp-reconnect <server>".into(),
                        });
                    }
                }
            }
            CommandAction::ToggleSidebar => {
                self.session.sidebar.update(SidebarMessage::Toggle);
            }
            CommandAction::Quit => {
                if self.session.is_streaming() {
                    self.ctx.send(shuvarie_core::Command::CancelStream);
                }
                self.save_scroll();
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
        self.tree_popup.close();
        self.history_search.close();
        self.confirm_quit.close();
        self.title_popup.close();
        self.variant_picker.close();
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
            if shuvarie_core::catalog::has_connected_providers(&self.ctx.connections)
                && self.welcome.open
            {
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
                variant: self
                    .ctx
                    .connections
                    .active
                    .as_ref()
                    .and_then(|a| a.variant.clone()),
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
        self.tree_popup.view(frame, area);
        self.scene_picker.view(frame, area);
        self.variant_picker.view(frame, area);
        self.history_search.view(frame, area);
        self.command_menu.view(frame, area);
        self.title_popup.view(frame, area);
        self.confirm_quit.view(frame, area);
        self.auth.view(frame, area);
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
    overlay_inner_list_height(area, 60, 60, 2)
}

fn add_provider_list_height(area: Rect, heading_len: u16) -> Option<u16> {
    overlay_inner_list_height(area, 50, 55, heading_len)
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
        App::new(
            UiPrefs::default(),
            shuvarie_core::ResolvedTheme::faerun(),
            RegistryEntry::default(),
            connections,
            tx,
            120,
            WorkspaceInfo::default(),
        )
    }

    fn app_with_rx(
        connections: Connections,
    ) -> (App, tokio::sync::mpsc::Receiver<shuvarie_core::Command>) {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let app = App::new(
            UiPrefs::default(),
            shuvarie_core::ResolvedTheme::faerun(),
            RegistryEntry::default(),
            connections,
            tx,
            120,
            WorkspaceInfo::default(),
        );
        (app, rx)
    }

    fn active_session(app: &mut App) {
        app.welcome.close();
        app.overlay = Overlay::None;
        app.session.session_id = Some(uuid::Uuid::now_v7());
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

    fn oauth_connected() -> Connections {
        let mut connections = Connections::default();
        let provider = shuvarie_core::ProviderConfig::new("ChatGPT", "chatgpt", None, None);
        connections.providers.insert("chatgpt".into(), provider);
        connections.active = Some(shuvarie_core::Active {
            provider: "chatgpt".into(),
            model: None,
            variant: None,
        });
        connections
    }

    #[test]
    fn auth_prompt_opens_popup_and_outcome_closes_it_with_notice() {
        let mut app = app_with(oauth_connected());
        app.update(AppMessage::Auth(AuthMessage::Prompt {
            provider: "ChatGPT".into(),
            verification_uri: "https://auth.openai.com/codex/device".into(),
            user_code: "ABCD-1234".into(),
        }));
        assert!(app.auth.open, "prompt opens the popup");

        // Success closes the matching popup and surfaces a notice.
        app.update(AppMessage::Auth(AuthMessage::Succeeded {
            provider: "ChatGPT".into(),
        }));
        assert!(!app.auth.open, "success closes the popup");
        assert!(app.warning.open, "success surfaces a notice");
    }

    #[test]
    fn auth_failure_for_another_provider_leaves_the_popup_open() {
        let mut app = app_with(oauth_connected());
        app.update(AppMessage::Auth(AuthMessage::Prompt {
            provider: "ChatGPT".into(),
            verification_uri: "https://auth.openai.com/codex/device".into(),
            user_code: "ABCD-1234".into(),
        }));
        app.update(AppMessage::Auth(AuthMessage::Failed {
            provider: "GitHub Copilot".into(),
            error: "timed out".into(),
        }));
        assert!(app.auth.open, "an unrelated failure keeps the prompt up");

        app.update(AppMessage::Auth(AuthMessage::Failed {
            provider: "ChatGPT".into(),
            error: "timed out".into(),
        }));
        assert!(!app.auth.open);
        assert!(app.warning.open);
    }

    #[test]
    fn auth_open_browser_returns_an_open_browser_effect() {
        let mut app = app_with(oauth_connected());
        let effect = app.update(AppMessage::Auth(AuthMessage::OpenBrowser {
            url: "https://auth.openai.com/codex/device".into(),
        }));
        let Some(AppEffect::OpenBrowser(url)) = &effect else {
            panic!("expected an OpenBrowser effect, got {effect:?}");
        };
        assert_eq!(url, "https://auth.openai.com/codex/device");
    }

    #[tokio::test]
    async fn submitting_a_keyless_oauth_provider_kicks_off_sign_in() {
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.open_add_provider();
        app.update(AppMessage::AddProvider(AddProviderMessage::OpenCustom));
        let form = app.add_provider_form.as_mut().unwrap();
        form.name.set("ChatGPT");
        form.kind.set("chatgpt");
        form.api_key.clear();
        app.update(AppMessage::AddProvider(AddProviderMessage::Submit));
        let mut saw_login = false;
        while let Ok(cmd) = rx.try_recv() {
            if matches!(cmd, shuvarie_core::Command::AuthProviderLogin { .. }) {
                saw_login = true;
            }
        }
        assert!(
            saw_login,
            "keyless chatgpt submit must send AuthProviderLogin"
        );
    }

    #[tokio::test]
    async fn submitting_an_oauth_provider_always_kicks_off_sign_in() {
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.open_add_provider();
        app.update(AppMessage::AddProvider(AddProviderMessage::OpenCustom));
        let form = app.add_provider_form.as_mut().unwrap();
        form.name.set("ChatGPT");
        form.kind.set("chatgpt");
        form.api_key.set("tok");
        app.update(AppMessage::AddProvider(AddProviderMessage::Submit));
        let mut saw_login = false;
        while let Ok(cmd) = rx.try_recv() {
            if matches!(cmd, shuvarie_core::Command::AuthProviderLogin { .. }) {
                saw_login = true;
            }
        }
        assert!(
            saw_login,
            "OAuth2 device-flow kinds always kick off sign-in"
        );
    }

    #[tokio::test]
    async fn submitting_a_non_oauth_provider_sends_no_sign_in() {
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.open_add_provider();
        app.update(AppMessage::AddProvider(AddProviderMessage::OpenCustom));
        let form = app.add_provider_form.as_mut().unwrap();
        form.name.set("Acme");
        form.kind.set("openai");
        form.api_key.set("tok");
        app.update(AppMessage::AddProvider(AddProviderMessage::Submit));
        while let Ok(cmd) = rx.try_recv() {
            assert!(
                !matches!(cmd, shuvarie_core::Command::AuthProviderLogin { .. }),
                "key-based kinds must not trigger the device flow: {cmd:?}"
            );
        }
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

    #[test]
    fn ctrl_c_clears_the_draft_then_quits_even_while_streaming() {
        let mut app = app_with(connected());
        app.welcome.close();
        app.overlay = Overlay::None;
        app.session.update(SessionMessage::TurnStarted {
            content: "hi".into(),
            steered: false,
        });
        app.session
            .update(SessionMessage::Chat(ChatMessage::TokenReceived {
                content: "answer".into(),
            }));
        app.session.input.buffer.set("draft");
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('c'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::Session(SessionMessage::Text(
                TextAreaMessage::Clear
            )))
        ));
        app.update(AppMessage::Session(SessionMessage::Text(
            TextAreaMessage::Clear,
        )));
        assert!(app.session.input.is_empty(), "draft is cleared");
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::RequestQuit)
        ));
        app.update(AppMessage::RequestQuit);
        assert!(app.confirm_quit.open, "quit prompt opens");
        assert!(matches!(app.overlay, Overlay::ConfirmQuit));
    }

    #[tokio::test]
    async fn ctrl_t_maps_to_cycle_variant_and_sends_the_command() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('t'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::CycleModelVariant)
        ));
        app.update(AppMessage::CycleModelVariant);
        assert!(matches!(
            rx.try_recv().unwrap(),
            shuvarie_core::Command::CycleVariant
        ));
    }

    #[test]
    fn resized_tracks_sidebar_width_for_toggle() {
        let mut app = app_with(connected());
        assert!(!app.session.sidebar.collapsed_at(200), "wide default");
        app.update(AppMessage::Resized { rows: 24, cols: 50 });
        assert!(app.session.sidebar.collapsed_at(50), "narrow auto-collapse");
        app.update(AppMessage::Session(SessionMessage::Sidebar(
            SidebarMessage::Toggle,
        )));
        assert!(
            !app.session.sidebar.collapsed_at(50),
            "toggle expands on the narrow screen"
        );
        assert!(
            !app.session.sidebar.collapsed_at(200),
            "manual override sticks across widths"
        );
    }

    #[tokio::test]
    async fn title_command_with_args_sends_set_title() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        app.run_command(CommandAction::EditTitle, Some("New name".into()));
        let cmd = rx.recv().await.unwrap();
        assert!(matches!(
            cmd,
            shuvarie_core::Command::SetTitle { ref title } if title == "New name"
        ));
    }

    #[test]
    fn title_command_without_args_opens_popup_prefilled() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        app.session.session_title = Some("Old title".into());
        app.run_command(CommandAction::EditTitle, None);
        assert!(matches!(app.overlay, Overlay::TitleEdit));
        assert!(app.title_popup.open);
        assert_eq!(app.title_popup.buffer.value, "Old title");
        assert!(rx.try_recv().is_err(), "no command sent until submit");
    }

    #[tokio::test]
    async fn title_popup_submit_sends_set_title_and_closes() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        app.session.session_title = Some("Old title".into());
        app.run_command(CommandAction::EditTitle, None);
        app.update(AppMessage::TitlePopup(TitleMessage::Input('!')));
        app.update(AppMessage::TitlePopup(TitleMessage::Submit));
        assert!(!app.title_popup.open);
        assert!(matches!(app.overlay, Overlay::None));
        let cmd = rx.recv().await.unwrap();
        assert!(matches!(
            cmd,
            shuvarie_core::Command::SetTitle { ref title } if title == "Old title!"
        ));
    }

    #[test]
    fn title_command_without_session_shows_error() {
        let (mut app, mut rx) = app_with_rx(connected());
        app.welcome.close();
        app.overlay = Overlay::None;
        app.run_command(CommandAction::EditTitle, None);
        assert!(matches!(app.overlay, Overlay::None), "popup stays closed");
        assert!(!app.title_popup.open);
        assert!(app.session.error.is_some(), "error is shown");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn title_edit_overlay_captures_keys() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        app.session.session_title = Some("Old".into());
        app.run_command(CommandAction::EditTitle, None);
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('z'), termina::event::Modifiers::NONE);
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::TitlePopup(TitleMessage::Input('z')))
        ));
        app.update(AppMessage::TitlePopup(TitleMessage::Input('z')));
        app.update(AppMessage::TitlePopup(TitleMessage::Submit));
        assert!(!app.title_popup.open);
        assert!(matches!(
            rx.try_recv().unwrap(),
            shuvarie_core::Command::SetTitle { ref title } if title == "Oldz"
        ));
        // The header only moves when the core echoes the change back.
        assert_eq!(app.session.session_title.as_deref(), Some("Old"));
        app.update(AppMessage::SessionTitleChanged {
            id: app.session.session_id.unwrap(),
            title: "Oldz".into(),
        });
        assert_eq!(app.session.session_title.as_deref(), Some("Oldz"));
    }

    #[test]
    fn session_title_changed_event_updates_header() {
        let mut app = app_with(connected());
        active_session(&mut app);
        app.session.session_title = Some("Before".into());
        app.update(AppMessage::SessionTitleChanged {
            id: app.session.session_id.unwrap(),
            title: "After".into(),
        });
        assert_eq!(app.session.session_title.as_deref(), Some("After"));
    }

    #[test]
    fn scenes_loaded_opens_the_warning_popup_on_conflicts() {
        let mut app = app_with(connected());
        app.update(AppMessage::ScenesLoaded {
            scenes: vec![shuvarie_core::scenes::SceneListEntry {
                id: Some("Plan".into()),
                name: "Plan".into(),
                description: None,
                switchable: true,
            }],
            default: None,
            warnings: vec!["scene `Plan` is defined multiple times".into()],
        });
        assert_eq!(app.scene_entries.len(), 1);
        assert!(app.warning.open, "the conflict warning surfaces as a popup");
    }

    #[test]
    fn scenes_loaded_without_warnings_keeps_the_popup_closed() {
        let mut app = app_with(connected());
        app.update(AppMessage::ScenesLoaded {
            scenes: Vec::new(),
            default: None,
            warnings: Vec::new(),
        });
        assert!(!app.warning.open);
        assert!(app.scene_entries.is_empty());
    }

    fn connected_variant() -> Connections {
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
        connections
    }

    #[test]
    fn catalog_variants_resolves_the_tagged_ollama_cloud_model() {
        assert_eq!(
            catalog_variants(&connected_variant()),
            Some(vec!["low".into(), "high".into(), "max".into()])
        );
    }

    #[test]
    fn catalog_variants_is_none_for_a_model_without_variants() {
        assert_eq!(catalog_variants(&connected()), None);
        assert_eq!(catalog_variants(&Connections::default()), None);
    }

    #[tokio::test]
    async fn variant_command_with_a_valid_argument_sends_select_variant() {
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, Some("HIGH".into()));
        assert!(matches!(app.overlay, Overlay::None), "no popup opens");
        let cmd = rx.recv().await.unwrap();
        assert!(
            matches!(
                cmd,
                shuvarie_core::Command::SelectVariant {
                    variant: Some(ref v)
                } if v == "high"
            ),
            "the canonical casing is sent"
        );
    }

    #[tokio::test]
    async fn variant_command_default_argument_clears_the_variant() {
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, Some("Default".into()));
        let cmd = rx.recv().await.unwrap();
        assert!(matches!(
            cmd,
            shuvarie_core::Command::SelectVariant { variant: None }
        ));
    }

    #[test]
    fn variant_command_with_an_invalid_argument_shows_an_error() {
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, Some("bogus".into()));
        assert!(app.session.error.is_some(), "an error is shown");
        let error = app.session.error.unwrap();
        assert!(
            error.contains("unknown variant `bogus`")
                && error.contains("available: default, low, high, max"),
            "the error names the accepted values: {error}"
        );
        assert!(
            rx.try_recv().is_err(),
            "nothing is sent for an invalid pick"
        );
    }

    #[test]
    fn variant_command_without_argument_opens_the_selector() {
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.ctx.connections.active.as_mut().unwrap().variant = Some("max".into());
        app.run_command(CommandAction::OpenVariantPicker, None);
        assert!(matches!(app.overlay, Overlay::Variant));
        assert!(app.variant_picker.open);
        assert_eq!(
            app.variant_picker.selected, 3,
            "the current pick is preselected"
        );
        assert!(rx.try_recv().is_err(), "no command until Enter");
    }

    #[test]
    fn variant_command_on_a_model_without_variants_shows_an_error() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, None);
        assert!(matches!(app.overlay, Overlay::None), "no popup opens");
        assert_eq!(
            app.session.error.as_deref(),
            Some("model claude-sonnet-4-5 has no variants")
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn variant_command_without_a_model_shows_an_error() {
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.welcome.close();
        app.overlay = Overlay::None;
        app.run_command(CommandAction::OpenVariantPicker, Some("high".into()));
        assert_eq!(app.session.error.as_deref(), Some("no active model"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_variant_overlay_captures_keys_and_select_sends_the_pick() {
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, None);
        let down = termina::event::KeyEvent::new(
            termina::event::KeyCode::Down,
            termina::event::Modifiers::empty(),
        );
        let enter = termina::event::KeyEvent::new(
            termina::event::KeyCode::Enter,
            termina::event::Modifiers::empty(),
        );
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(down))),
            Some(AppMessage::Variant(variant::VariantMessage::Next))
        ));
        app.update(AppMessage::Variant(variant::VariantMessage::Next));
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(enter))),
            Some(AppMessage::Variant(variant::VariantMessage::Select))
        ));
        app.update(AppMessage::Variant(variant::VariantMessage::Select));
        assert!(matches!(app.overlay, Overlay::None), "the picker closes");
        let cmd = rx.try_recv().unwrap();
        assert!(matches!(
            cmd,
            shuvarie_core::Command::SelectVariant {
                variant: Some(ref v)
            } if v == "low"
        ));
    }
}
