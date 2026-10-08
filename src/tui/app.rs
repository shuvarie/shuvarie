use std::collections::HashMap;
use std::io::{self, Write};

use ratatui::prelude::*;
use shuvarie_core::{Connections, Event as CoreEvent, Model, RegistriesConfig, UiPrefs};
use termina::event::{KeyCode, KeyEventKind, MouseButton, MouseEventKind};
use termina::{Event as TermEvent, PlatformTerminal};
use tokio::sync::mpsc::Sender;

use crate::tui::event::Event;
use crate::tui::utils::ctrl;

use super::add_decision_provider::{
    AddDecisionProviderForm, AddDecisionProviderMessage, AddDecisionProviderOutcome,
};
use super::add_provider::{
    AddProviderForm, AddProviderMessage, AddProviderOutcome, AddProviderStage,
};
use super::assisted_by::{AssistedByEffect, AssistedByMessage, AssistedByPopup};
use super::auth::{AuthMessage, AuthPopup};
use super::command_menu::{CommandMenu, CommandMenuMessage};
use super::commands::{CommandAction, CommandRef};
use super::components::TextAreaMessage;
use super::confirm_quit::{ConfirmQuit, ConfirmQuitEffect, ConfirmQuitMessage};
use super::context::UpdateCtx;
use super::history_search::{HistorySearch, HistorySearchEffect, HistorySearchMessage};
use super::model_picker::{ModelPicker, ModelPickerEffect, ModelPickerMessage};
use super::scene;
use super::search::SearchMessage;
use super::session::media::MediaBytes;
use super::session::tree::{TreeEffect, TreeMessage, TreePopup};
use super::session::{
    BashMessage, ChatMessage, MouseKind, SessionEffect, SessionMessage, SessionScreen,
};
use super::session_picker::{SessionPicker, SessionPickerEffect, SessionPickerMessage};
use super::sidebar::SidebarMessage;
use super::spinner::SpinnerKind;
use super::theme;
use super::theme_picker::{ThemePicker, ThemePickerEffect, ThemePickerMessage};
use super::title::{TitleEffect, TitleMessage, TitlePopup};
use super::variant;
use super::viewer::{MediaViewer, MediaViewerMessage, ViewerItem};
use super::warning::{WarningMessage, WarningPopup};
use super::welcome::{Welcome, WelcomeEffect, WelcomeMessage};
use super::workspace::WorkspaceInfo;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Welcome,
    AddProvider,
    /// Register or edit a decision-model connection. A decision model is not a
    /// generative LLM, so it has no Selune catalog entry, no transport list,
    /// and no OAuth — just one System One endpoint and a free-text model name.
    AddDecisionProvider,
    ModelPicker,
    CommandMenu,
    ConfirmQuit,
    SessionPicker,
    HistorySearch,
    TitleEdit,
    Tree,
    Scene,
    Variant,
    ThemePicker,
    AssistedBy,
    MediaViewer,
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
    AddDecisionProvider(AddDecisionProviderMessage),
    ModelPicker(ModelPickerMessage),
    CommandMenu(CommandMenuMessage),
    Welcome(WelcomeMessage),
    SessionPicker(SessionPickerMessage),
    Tree(TreeMessage),
    Scene(scene::SceneMessage),
    Variant(variant::VariantMessage),
    Theme(ThemePickerMessage),
    AssistedBy(AssistedByMessage),
    /// The media viewer overlay: navigation and dismiss.
    MediaViewer(MediaViewerMessage),
    /// Attachment blob bytes arrived from core (the chat pane's media and
    /// the viewer's display cache share this channel). Mapped from
    /// `CoreEvent::AttachmentMedia`.
    MediaArrived(Vec<(String, Option<MediaBytes>)>),
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
    /// A registry's online fetch (from either selector popup) succeeded.
    RegistryLoaded {
        registry: String,
        providers: Vec<selune::Provider>,
    },
    /// A registry's online fetch failed.
    RegistryError {
        registry: String,
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
    CustomCommandsLoaded {
        commands: Vec<shuvarie_core::CustomCommand>,
        warnings: Vec<shuvarie_core::CustomCommandWarning>,
    },
    ShellWarning {
        message: String,
    },
    Auth(AuthMessage),
    Warning(WarningMessage),
    /// The render loop's spinner wake: refresh the animated spinner renders.
    SpinnerUpdate,
    /// The workspace's watched `.git/HEAD` file changed (a checkout or
    /// branch switch made outside the TUI): the sidebar re-reads the branch
    /// label.
    BranchChanged,
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
    /// The decision-provider dialog. Its fields are plain strings, so the form
    /// exists only while the overlay is open.
    pub add_decision_provider_form: Option<AddDecisionProviderForm>,
    pub model_picker: ModelPicker,
    pub session_picker: SessionPicker,
    pub tree_popup: TreePopup,
    pub scene_picker: scene::ScenePicker,
    pub variant_picker: variant::VariantPicker,
    pub history_search: HistorySearch,
    pub title_popup: TitlePopup,
    pub assisted_by: AssistedByPopup,
    pub theme_picker: ThemePicker,
    pub warning: WarningPopup,
    pub auth: AuthPopup,
    pub models: HashMap<String, Vec<Model>>,
    registries: RegistriesConfig,
    pending_model_pick: Option<String>,
    /// The configured scene set (built-in Default first) for the switcher.
    scene_entries: Vec<shuvarie_core::scenes::SceneListEntry>,
    /// The scene new sessions start under (`None` = built-in Default).
    scene_default: Option<String>,
    /// The session's current scene (`None` = built-in Default).
    current_scene: Option<String>,
    /// Every selectable theme for the picker (the unset default first).
    theme_choices: Vec<shuvarie_core::ThemeChoice>,
    /// The current `ui.theme` pref (`None` = the unset default).
    current_theme_pref: Option<String>,
    /// The palette painted before the picker opened, restored on cancel.
    theme_backup: Option<shuvarie_core::ThemeColors>,
    /// The fullscreen media viewer overlay (the `/images` slash command, or
    /// a click on an image region).
    pub media_view: MediaViewer,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ui: UiPrefs,
        theme: shuvarie_core::ResolvedTheme,
        theme_choices: Vec<shuvarie_core::ThemeChoice>,
        registries: RegistriesConfig,
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
        let current_theme_pref = ui.theme.clone();
        // One resolution of the image settings for both the chat pane and
        // the fullscreen viewer. tui.rs resolved auto-detection into
        // `protocol` before constructing the App; a raw-config construction
        // (tests) defaults to halfblocks.
        let image_protocol = ui
            .image
            .protocol
            .unwrap_or(shuvarie_core::ImageProtocol::Halfblocks);
        let image_cell = ui
            .image
            .cell_size
            .map(|(width, height)| ratatui_image::FontSize::new(width, height))
            .unwrap_or_else(|| ratatui_image::FontSize::new(8, 16));
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
                s.update(SessionMessage::Chat(ChatMessage::ImageConfig {
                    cell_size: ui.image.cell_size,
                    protocol: Some(image_protocol),
                }));
                s
            },
            command_menu: CommandMenu::new(),
            welcome,
            confirm_quit: ConfirmQuit::new(),
            add_provider_form: None,
            add_decision_provider_form: None,
            model_picker: ModelPicker::new(),
            session_picker: SessionPicker::new(),
            tree_popup: TreePopup::new(),
            scene_picker: scene::ScenePicker::new(),
            variant_picker: variant::VariantPicker::new(),
            history_search: HistorySearch::new(),
            title_popup: TitlePopup::new(),
            assisted_by: AssistedByPopup::new(),
            theme_picker: ThemePicker::new(),
            media_view: MediaViewer::new(image_cell, image_protocol),
            theme_choices,
            current_theme_pref,
            theme_backup: None,
            warning,
            auth: AuthPopup::new(),
            models: HashMap::new(),
            registries,
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

    /// Execute the frame's media renders and deliver the escape payloads
    /// (kitty transmissions, placed sixel/iTerm2 graphics). Paint is pure:
    /// this runs between frames, where `&mut` is legitimately held, and
    /// fills every render entry the pane and viewer paint read. The queued
    /// payloads reach the terminal outside the cell diff — a megabyte
    /// payload inside a cell symbol gets wrap-sliced into text by the pane's
    /// paint — and a draw that delivered one is immediately followed by one
    /// empty-diff draw (the loop re-draws on `Ok(true)`), letting the
    /// terminal re-render its grid with the now-transmitted graphic in
    /// place.
    pub fn after_frame(
        &mut self,
        rat: &mut ratatui::Terminal<TerminaBackend<PlatformTerminal>>,
    ) -> io::Result<bool> {
        self.session.chat.ensure_media_renders();
        if self.media_view.is_open() {
            let frame_area = rat.get_frame().area();
            self.media_view.after_frame(frame_area);
        }
        let mut writes = self.session.chat.take_media_writes();
        writes.extend(self.media_view.take_media_writes());
        if writes.is_empty() {
            return Ok(false);
        }

        let terminal = rat.backend_mut().terminal_mut();
        for write in writes {
            super::session::media::write_media_write(terminal, write)?;
        }
        terminal.flush()?;
        Ok(true)
    }

    /// Delete every kitty image the chat pane and viewer transmitted — the
    /// shutdown hop empties the terminal's image cache.
    pub fn write_kitty_shutdown_deletes<W: io::Write>(
        &mut self,
        terminal: &mut W,
    ) -> io::Result<()> {
        let mut writes = self.session.chat.take_kitty_delete_writes();
        writes.extend(self.media_view.take_kitty_delete_writes());
        if writes.is_empty() {
            return Ok(());
        }
        for bytes in writes {
            terminal.write_all(&bytes)?;
        }
        terminal.flush()
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
                        Overlay::AddDecisionProvider => {
                            return self
                                .add_decision_provider_form
                                .as_ref()
                                .and_then(|f| f.map_event(&key))
                                .map(AppMessage::AddDecisionProvider);
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
                        Overlay::ThemePicker => {
                            return self.theme_picker.map_event(&key).map(AppMessage::Theme);
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
                        Overlay::AssistedBy => {
                            return self.assisted_by.map_event(&key).map(AppMessage::AssistedBy);
                        }
                        Overlay::MediaViewer => {
                            return self.media_view.map_event(&key).map(AppMessage::MediaViewer);
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
                CoreEvent::RegistryLoaded {
                    registry,
                    providers,
                } => Some(AppMessage::RegistryLoaded {
                    registry,
                    providers,
                }),
                CoreEvent::RegistryError { registry, error } => {
                    Some(AppMessage::RegistryError { registry, error })
                }
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
                    spawn,
                    call_id,
                } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::ToolStarted {
                        name,
                        args,
                        worker,
                        spawn,
                        call_id: Some(call_id),
                    },
                ))),
                CoreEvent::ToolFinished {
                    name,
                    ok,
                    output,
                    worker,
                    spawn,
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
                        spawn,
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
                CoreEvent::TurnMeta { meta } => Some(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::TurnMeta { meta },
                ))),
                CoreEvent::PromptSteered { content } => Some(AppMessage::Session(
                    SessionMessage::Chat(ChatMessage::SteeredQueued { content }),
                )),
                CoreEvent::TurnStarted {
                    content,
                    attachments,
                    steered,
                } => Some(AppMessage::Session(SessionMessage::TurnStarted {
                    content,
                    attachments,
                    steered,
                })),
                CoreEvent::AttachmentMedia { items } => Some(AppMessage::MediaArrived(
                    items
                        .into_iter()
                        .map(|item| {
                            (
                                item.sha256,
                                item.bytes.map(|bytes| MediaBytes(bytes.into())),
                            )
                        })
                        .collect(),
                )),
                CoreEvent::DirectivesProbed { token, items } => {
                    Some(AppMessage::Session(SessionMessage::DirectivesValidated {
                        token,
                        items,
                    }))
                }
                CoreEvent::PathCompletions { token, candidates } => {
                    let menu = SessionMessage::PathCompletions { token, candidates };
                    Some(AppMessage::Session(menu))
                }
                CoreEvent::AttachmentNotice { text } => {
                    Some(AppMessage::Session(SessionMessage::ShowStatus {
                        status: text,
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
                CoreEvent::SessionCompacted { session } => {
                    Some(AppMessage::Session(SessionMessage::SessionCompacted {
                        session,
                    }))
                }
                CoreEvent::ModelUsed { session_id, models } => {
                    Some(AppMessage::Session(SessionMessage::ModelsUsed {
                        session_id,
                        models,
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
                    allow_dir,
                } => Some(AppMessage::Session(SessionMessage::PermissionRequested {
                    id,
                    description,
                    allow_session,
                    allow_dir,
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
                CoreEvent::CustomCommandsLoaded { commands, warnings } => {
                    Some(AppMessage::CustomCommandsLoaded { commands, warnings })
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
            Overlay::AddDecisionProvider => self.add_decision_provider_form.as_ref().map(|_| {
                AppMessage::AddDecisionProvider(AddDecisionProviderMessage::Paste(text.to_string()))
            }),
            Overlay::Welcome
            | Overlay::CommandMenu
            | Overlay::ConfirmQuit
            | Overlay::SessionPicker
            | Overlay::Tree
            | Overlay::Scene
            | Overlay::Variant
            | Overlay::ThemePicker
            | Overlay::AssistedBy
            | Overlay::MediaViewer => None,
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
                self.open_confirm_quit();
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
                // The quit prompt can also open over the welcome screen, so
                // canceling must return there instead of the bare session.
                self.overlay = if self.welcome.open {
                    Overlay::Welcome
                } else {
                    Overlay::None
                };
            }
            AppMessage::Session(m) => {
                let effect = self.session.update(m);
                if self.tree_popup.open {
                    self.tree_popup.set_busy(self.session.is_busy());
                }
                // The message's own effect first (it may hop an app-effect —
                // a clipboard write or a builtin command); the compose-side
                // sync's queued core requests (attachment probe, mention
                // completions) drain after — a session update on the very
                // next message flushes any that collide with an app hop.
                if let Some(app_effect) =
                    effect.and_then(|effect| self.handle_session_effect(effect))
                {
                    return Some(app_effect);
                }
                for deferred in self.session.take_deferred_effects() {
                    if let Some(app_effect) = self.handle_session_effect(deferred) {
                        return Some(app_effect);
                    }
                }
                return None;
            }
            AppMessage::MediaArrived(items) => {
                // Feed an open viewer from the same content channel the chat
                // pane consumes, then delegate to the session hop.
                if self.media_view.is_open() {
                    self.media_view.receive(&items);
                    let missing = self.media_view.missing();
                    if !missing.is_empty() {
                        self.ctx
                            .send(shuvarie_core::Command::LoadAttachmentMedia { hashes: missing });
                    }
                }
                return self.update(AppMessage::Session(SessionMessage::Chat(
                    ChatMessage::MediaArrived { items },
                )));
            }
            AppMessage::MediaViewer(m) => {
                self.media_view.update(m);
                if self.media_view.is_open() {
                    let missing = self.media_view.missing();
                    if !missing.is_empty() {
                        self.ctx
                            .send(shuvarie_core::Command::LoadAttachmentMedia { hashes: missing });
                    }
                } else if self.overlay == Overlay::MediaViewer {
                    self.overlay = Overlay::None;
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
            AppMessage::Theme(m) => {
                if let Some(effect) = self.theme_picker.update(m) {
                    match effect {
                        ThemePickerEffect::Preview { colors } => {
                            // Live preview while walking: repaint with the
                            // highlighted palette immediately.
                            theme::set(colors);
                            self.session.update(SessionMessage::ThemeChanged);
                        }
                        ThemePickerEffect::Select { pref } => {
                            // Keep the previewed palette; record the choice
                            // in the config file.
                            self.theme_backup = None;
                            self.current_theme_pref = pref.clone();
                            self.close_overlay();
                            self.ctx.send(shuvarie_core::Command::SetUiTheme { pref });
                        }
                        ThemePickerEffect::Close => {
                            // Walked away without picking: repaint with the
                            // palette active at open.
                            if let Some(colors) = self.theme_backup.take() {
                                theme::set(colors);
                                self.session.update(SessionMessage::ThemeChanged);
                            }
                            self.close_overlay();
                        }
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
                        TreeEffect::Fork {
                            node,
                            summarize,
                            after,
                        } => {
                            self.close_overlay();
                            self.ctx.send(shuvarie_core::Command::ForkSession {
                                node: Some(node),
                                summarize,
                                after,
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
            AppMessage::AssistedBy(m) => {
                if let Some(effect) = self.assisted_by.update(m) {
                    match effect {
                        AssistedByEffect::Copy { content } => {
                            return Some(AppEffect::CopyToClipboard(content));
                        }
                        AssistedByEffect::Close => self.close_overlay(),
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
                        AddProviderOutcome::FetchRegistries(ids) => {
                            for registry in ids {
                                self.ctx
                                    .send(shuvarie_core::Command::FetchRegistry { registry });
                            }
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
            AppMessage::AddDecisionProvider(m) => {
                if let Some(form) = &mut self.add_decision_provider_form {
                    match form.update(m) {
                        AddDecisionProviderOutcome::Cancel => self.close_overlay(),
                        AddDecisionProviderOutcome::Submit {
                            name,
                            base_url,
                            api_key,
                            model,
                        } => {
                            // The connection and the active selection are
                            // written together: a decision connection is
                            // unusable without a model, so a half-saved
                            // selection would leave the shell checks with
                            // nothing to ask.
                            self.ctx.send(shuvarie_core::Command::SetDecisionProvider {
                                name,
                                api_key,
                                base_url: Some(base_url),
                                model,
                            });
                            self.close_overlay();
                        }
                        AddDecisionProviderOutcome::None => {}
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
                        ModelPickerEffect::FetchRegistries(ids) => {
                            for registry in ids {
                                self.ctx
                                    .send(shuvarie_core::Command::FetchRegistry { registry });
                            }
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
                if let Some(action) = self.command_menu.update(m) {
                    match action {
                        CommandRef::Builtin(action) => {
                            if let Some(effect) = self.run_command(action, None) {
                                return Some(effect);
                            }
                        }
                        CommandRef::Custom { name, .. } => {
                            // The session owns the command roster and the
                            // expansion.
                            self.session
                                .update(SessionMessage::RunCustomCommand { name });
                        }
                    }
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
                        WelcomeEffect::RequestQuit => self.open_confirm_quit(),
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
                    // The decision-provider dialog is a fixed-size form with
                    // no list, so a resize needs no viewport recalculation.
                    Overlay::AddDecisionProvider => {}
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
                    | Overlay::Variant
                    | Overlay::ThemePicker
                    | Overlay::AssistedBy
                    | Overlay::MediaViewer => {}
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
            AppMessage::RegistryLoaded {
                registry,
                providers,
            } => match self.overlay {
                Overlay::AddProvider => {
                    if let Some(form) = &mut self.add_provider_form {
                        form.update(AddProviderMessage::RegistryLoaded {
                            registry,
                            providers,
                        });
                    }
                }
                Overlay::ModelPicker => {
                    self.model_picker
                        .update(ModelPickerMessage::RegistryLoaded {
                            registry,
                            providers,
                        });
                }
                _ => {}
            },
            AppMessage::RegistryError { registry, error } => match self.overlay {
                Overlay::AddProvider => {
                    if let Some(form) = &mut self.add_provider_form {
                        form.update(AddProviderMessage::RegistryError {
                            registry,
                            error: error.clone(),
                        });
                    }
                }
                Overlay::ModelPicker => {
                    self.model_picker
                        .update(ModelPickerMessage::RegistryError { registry, error });
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
            AppMessage::CustomCommandsLoaded { commands, warnings } => {
                self.session.update(SessionMessage::SetCustomCommands {
                    commands: commands.clone(),
                });
                self.command_menu.set_custom_commands(&commands);
                if !warnings.is_empty() {
                    let messages: Vec<String> = warnings
                        .into_iter()
                        .map(|warning| format!("{}: {}", warning.path.display(), warning.message))
                        .collect();
                    self.warning.open(messages.join("\n"));
                }
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
            AppMessage::BranchChanged => {
                // The watched `.git/HEAD` file changed: the sidebar (and the
                // collapsed footer) re-read the branch label.
                self.session
                    .update(SessionMessage::Sidebar(SidebarMessage::BranchChanged));
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
        let form = AddProviderForm::new(&names, &catalog_ids, &self.registries);
        for registry in form.ids_needing_fetch() {
            self.ctx
                .send(shuvarie_core::Command::FetchRegistry { registry });
        }
        self.add_provider_form = Some(form);
        self.overlay = Overlay::AddProvider;
    }

    /// Opens the decision-provider dialog. When a decision connection is
    /// already active the form is seeded from it, so the dialog edits that
    /// connection in place — with no registry to pick from, re-running the form
    /// is the only way to change a model or endpoint.
    fn open_add_decision_provider(&mut self) {
        let names: Vec<String> = self
            .ctx
            .connections
            .decision_providers
            .keys()
            .cloned()
            .collect();
        let mut form = AddDecisionProviderForm::new(&names);
        if let Some(active) = &self.ctx.connections.decision {
            let provider = self
                .ctx
                .connections
                .decision_providers
                .get(&active.provider);
            form = form.prefill(
                &active.provider,
                provider.and_then(|p| p.base_url.as_deref()),
                provider.and_then(|p| p.api_key.as_deref()),
                &active.model,
            );
        }
        self.add_decision_provider_form = Some(form);
        self.overlay = Overlay::AddDecisionProvider;
    }

    /// Opens the model selector popup over every configured provider.
    /// Catalog-less providers whose models are not cached yet get a live
    /// `ListModels` fetch.
    fn open_model_picker(&mut self) {
        self.model_picker
            .open(&self.ctx.connections, &self.models, &self.registries);
        for registry in self.model_picker.ids_needing_fetch() {
            self.ctx
                .send(shuvarie_core::Command::FetchRegistry { registry });
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

    /// Opens the theme picker, or with an argument (`/theme <name[:variant]>`)
    /// applies that palette directly: `default` clears to the unset default,
    /// a selectable `name` or `name:variant` (exactly as one of the picker's
    /// choices) switches in place, anything else shows an error listing the
    /// selectable themes.
    fn open_theme_picker(&mut self, args: Option<String>) {
        match args {
            None => {
                self.theme_backup = Some(theme::current());
                self.theme_picker
                    .open(&self.theme_choices, self.current_theme_pref.as_deref());
                self.overlay = Overlay::ThemePicker;
            }
            Some(arg) => {
                if arg.eq_ignore_ascii_case("default") {
                    let colors = self
                        .theme_choices
                        .first()
                        .map(|choice| choice.colors)
                        .unwrap_or_else(theme::current);
                    self.apply_theme(None, colors);
                    return;
                }
                match self
                    .theme_choices
                    .iter()
                    .find(|choice| choice.pref.as_deref() == Some(arg.as_str()))
                {
                    Some(choice) => {
                        let colors = choice.colors;
                        self.apply_theme(choice.pref.clone(), colors);
                    }
                    None => {
                        let available: Vec<String> = self
                            .theme_choices
                            .iter()
                            .filter_map(|choice| choice.pref.clone())
                            .collect();
                        self.session.update(SessionMessage::ShowError {
                            error: format!(
                                "unknown theme `{arg}` — available: default, {}",
                                available.join(", ")
                            ),
                        });
                    }
                }
            }
        }
    }

    /// Applies a theme palette: swaps the active palette, records the pref,
    /// and persists it to the config file. `pref` = `None` selects the unset
    /// default (Faerun by the terminal's mode).
    fn apply_theme(&mut self, pref: Option<String>, colors: shuvarie_core::ThemeColors) {
        theme::set(colors);
        self.session.update(SessionMessage::ThemeChanged);
        self.current_theme_pref = pref.clone();
        self.ctx.send(shuvarie_core::Command::SetUiTheme { pref });
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
        self.command_menu.set_availability(
            CommandAction::GenTitle,
            self.session.session_id.is_some() && self.session.has_user_turn(),
        );
        self.command_menu
            .set_availability(CommandAction::Export, self.session.session_id.is_some());
        self.command_menu
            .set_availability(CommandAction::Images, self.session.chat.has_images());
    }

    /// Runs a built-in command action (from the Ctrl+M menu or the inline
    /// slash menu). Returns `Some(AppEffect)` when the action needs to
    /// escalate to the parent (quit).
    fn run_command(&mut self, action: CommandAction, args: Option<String>) -> Option<AppEffect> {
        match action {
            CommandAction::OpenModelSelect => {
                self.open_model_picker();
            }
            CommandAction::AddProvider => {
                self.open_add_provider();
            }
            CommandAction::AddDecisionProvider => {
                self.open_add_decision_provider();
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
            CommandAction::OpenThemePicker => {
                self.open_theme_picker(args);
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
            CommandAction::GenTitle => {
                if self.session.session_id.is_none() {
                    self.session.update(SessionMessage::ShowError {
                        error: "no active session".into(),
                    });
                } else {
                    self.ctx.send(shuvarie_core::Command::GenTitle);
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
                    after: false,
                });
            }
            CommandAction::Replay => {
                self.ctx.send(shuvarie_core::Command::Replay);
            }
            CommandAction::Compact => {
                if self.session.session_id.is_none() {
                    self.session.update(SessionMessage::ShowError {
                        error: "no active session".into(),
                    });
                } else {
                    let instruction = args.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
                    self.ctx
                        .send(shuvarie_core::Command::CompactSession { instruction });
                }
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
            CommandAction::Search => {
                self.session.open_search(args.as_deref());
            }
            CommandAction::AssistedBy => {
                self.assisted_by.open(self.session.models_used.clone());
                self.overlay = Overlay::AssistedBy;
            }
            CommandAction::Images => {
                let effect = self
                    .session
                    .update(SessionMessage::OpenMediaViewer { sha: None });
                return effect.and_then(|effect| self.handle_session_effect(effect));
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

    /// Opens the quit confirmation prompt over whatever overlay is active.
    fn open_confirm_quit(&mut self) {
        self.confirm_quit.open();
        self.overlay = Overlay::ConfirmQuit;
    }

    /// Route a session effect into the core (`ctx.send`) or the app
    /// (an `AppEffect`). Extracted from the `AppMessage::Session` arm so the
    /// deferred compose-request effects (attachment probe, mention
    /// completions) drain through the same routing.
    fn handle_session_effect(&mut self, effect: SessionEffect) -> Option<AppEffect> {
        match effect {
            SessionEffect::SendMessage {
                content,
                attachments,
                model,
            } => {
                self.ctx.send(shuvarie_core::Command::SendMessage {
                    content,
                    attachments,
                    model,
                });
                None
            }
            SessionEffect::LoadMedia { hashes } => {
                self.ctx
                    .send(shuvarie_core::Command::LoadAttachmentMedia { hashes });
                None
            }
            SessionEffect::ProbeDirectives { token, paths } => {
                self.ctx
                    .send(shuvarie_core::Command::ProbeDirectives { token, paths });
                None
            }
            SessionEffect::PathCompletions { token, query } => {
                self.ctx
                    .send(shuvarie_core::Command::RequestPathCompletions { token, query });
                None
            }
            SessionEffect::OpenMediaViewer { items, selected } => {
                let items = items
                    .into_iter()
                    .map(|(sha256, name)| ViewerItem { sha256, name })
                    .collect();
                let missing = self.media_view.open(items, selected.as_deref());
                self.overlay = Overlay::MediaViewer;
                if !missing.is_empty() {
                    self.ctx
                        .send(shuvarie_core::Command::LoadAttachmentMedia { hashes: missing });
                }
                None
            }
            SessionEffect::RunBash { command } => {
                self.ctx.send(shuvarie_core::Command::RunBash { command });
                None
            }
            SessionEffect::CancelStream => {
                self.ctx.send(shuvarie_core::Command::CancelStream);
                None
            }
            SessionEffect::RecallSteered { stacked } => {
                self.ctx
                    .send(shuvarie_core::Command::RecallSteered { stacked });
                None
            }
            SessionEffect::AnswerQuestion { id, answers } => {
                self.ctx
                    .send(shuvarie_core::Command::AnswerQuestion { id, answers });
                None
            }
            SessionEffect::PermissionDecide { id, decision } => {
                self.ctx
                    .send(shuvarie_core::Command::PermissionDecide { id, decision });
                None
            }
            SessionEffect::RunCommand { action, args } => {
                // Custom refs never reach the app (the session expands
                // them); a defensive no-op otherwise.
                if let CommandRef::Builtin(action) = action
                    && let Some(effect) = self.run_command(action, args)
                {
                    return Some(effect);
                }
                None
            }
            SessionEffect::CopyToClipboard { text } => Some(AppEffect::CopyToClipboard(text)),
        }
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.command_menu.close();
        self.add_provider_form = None;
        self.add_decision_provider_form = None;
        self.model_picker.close();
        self.session_picker.close();
        self.tree_popup.close();
        self.history_search.close();
        self.confirm_quit.close();
        self.title_popup.close();
        self.assisted_by.close();
        self.variant_picker.close();
        self.theme_picker.close();
        // Walking away from the theme picker repaints with the palette
        // active at open (already cleared to `None` by a selection).
        if let Some(colors) = self.theme_backup.take() {
            theme::set(colors);
            self.session.update(SessionMessage::ThemeChanged);
        }
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

        // `auth` and `warning` are transient popups that paint above every
        // overlay; the dialogs underneath them take the dim border so the
        // stack reads top-down.
        let stacked = self.auth.open || self.warning.open;
        self.welcome.view(frame, area, stacked);
        if let Some(form) = &self.add_provider_form {
            form.view(frame, area, stacked);
        }
        if let Some(form) = &self.add_decision_provider_form {
            form.view(frame, area, stacked);
        }
        self.model_picker.view(frame, area, stacked);
        self.session_picker.view(frame, area, stacked);
        self.tree_popup.view(frame, area, stacked);
        self.scene_picker.view(frame, area, stacked);
        self.variant_picker.view(frame, area, stacked);
        self.theme_picker.view(frame, area, stacked);
        self.media_view.view(frame, area);
        self.history_search.view(frame, area, stacked);
        self.command_menu.view(frame, area, stacked);
        self.title_popup.view(frame, area, stacked);
        self.assisted_by.view(frame, area, stacked);
        self.confirm_quit.view(frame, area, stacked);
        self.auth.view(frame, area, self.warning.open);
        self.warning.view(frame, area, false);
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
            shuvarie_core::ThemeSet::default().choices(shuvarie_core::ThemeVariant::Dark),
            RegistriesConfig::default(),
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
            shuvarie_core::ThemeSet::default().choices(shuvarie_core::ThemeVariant::Dark),
            RegistriesConfig::default(),
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

    /// A connection's active-model context window, read from the live catalog
    /// instead of a pinned literal, so a registry bump (a renamed model, a new
    /// context window) does not fail the test.
    fn catalog_context_window(catalog_id: &str, model: &str) -> u64 {
        let providers = shuvarie_core::catalog::providers();
        shuvarie_core::catalog::find_provider(&providers, catalog_id)
            .and_then(|provider| shuvarie_core::catalog::context_length(provider, model))
            .unwrap_or_else(|| panic!("no catalog `{catalog_id}` model `{model}`"))
            .max(0) as u64
    }

    /// The reasoning-effort variants the catalog declares for a model, in
    /// declared order — the expectation side of the same lookup the app makes.
    fn catalog_model_variants(catalog_id: &str, model: &str) -> Vec<String> {
        let providers = shuvarie_core::catalog::providers();
        let provider = shuvarie_core::catalog::find_provider(&providers, catalog_id)
            .unwrap_or_else(|| panic!("no catalog provider `{catalog_id}`"));
        shuvarie_core::catalog::find_model(provider, model)
            .map(shuvarie_core::catalog::model_variants)
            .unwrap_or_else(|| panic!("no catalog model `{model}` in `{catalog_id}`"))
            .to_vec()
    }

    /// The first model the catalog declares for `catalog_id` with no
    /// reasoning-effort variants, so the no-variant cases exercise real catalog
    /// data instead of pinning a model that may gain variants later.
    fn catalog_model_without_variants(catalog_id: &str) -> String {
        let providers = shuvarie_core::catalog::providers();
        let provider = shuvarie_core::catalog::find_provider(&providers, catalog_id)
            .unwrap_or_else(|| panic!("no catalog provider `{catalog_id}`"));
        provider
            .models
            .iter()
            .find(|model| shuvarie_core::catalog::model_variants(model).is_empty())
            .unwrap_or_else(|| panic!("catalog `{catalog_id}` has no variant-free model"))
            .id
            .clone()
    }

    #[test]
    fn catalog_context_length_maps_catalog_and_alias_model_id() {
        let app = app_with(connected());
        assert_eq!(
            catalog_context_length(&app.ctx.connections),
            Some(catalog_context_window("anthropic", "claude-sonnet-4-5")),
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
            Some(catalog_context_window("anthropic", "claude-sonnet-4-5")),
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
            Some(catalog_context_window("ollama-cloud", "glm-5.3-flash")),
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
            attachments: Vec::new(),
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

    #[test]
    fn welcome_ctrl_c_opens_the_quit_prompt_and_confirming_quits() {
        let mut app = app_with(Connections::default());
        assert!(app.welcome.open, "welcome starts open without providers");
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('c'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::Welcome(WelcomeMessage::RequestQuit))
        ));
        app.update(AppMessage::Welcome(WelcomeMessage::RequestQuit));
        assert!(app.confirm_quit.open, "quit prompt opens over welcome");
        assert!(matches!(app.overlay, Overlay::ConfirmQuit));

        // The second Ctrl+C is routed by the confirm prompt itself.
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::ConfirmQuit)
        ));
        assert!(matches!(
            app.update(AppMessage::ConfirmQuit),
            Some(AppEffect::Quit)
        ));
    }

    #[test]
    fn canceling_the_quit_prompt_restores_the_previous_screen() {
        let mut app = app_with(Connections::default());
        app.update(AppMessage::Welcome(WelcomeMessage::RequestQuit));
        assert!(matches!(app.overlay, Overlay::ConfirmQuit));

        app.update(AppMessage::CancelQuit);
        assert!(!app.confirm_quit.open, "cancel closes the prompt");
        assert!(app.welcome.open);
        assert!(matches!(app.overlay, Overlay::Welcome));

        // Outside the welcome screen, cancel still returns to the session.
        let mut app = app_with(connected());
        active_session(&mut app);
        app.update(AppMessage::RequestQuit);
        app.update(AppMessage::CancelQuit);
        assert!(matches!(app.overlay, Overlay::None));
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
    fn ctrl_m_opens_the_command_menu() {
        let mut app = app_with(connected());
        active_session(&mut app);
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('m'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            app.map_event(Event::Terminal(TermEvent::Key(key))),
            Some(AppMessage::OpenCommandMenu)
        ));
        app.update(AppMessage::OpenCommandMenu);
        assert!(app.command_menu.open, "the menu opens");
        assert!(matches!(app.overlay, Overlay::CommandMenu));
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

    #[test]
    fn images_command_opens_the_media_viewer() {
        let (mut app, mut rx) = app_with_rx(connected());
        active_session(&mut app);
        // No images: the command is a no-op (the session shows a status
        // notice — covered at the session level).
        app.run_command(CommandAction::Images, None);
        assert!(matches!(app.overlay, Overlay::None));
        assert!(rx.try_recv().is_err(), "nothing sent without images");
        // TurnStarted delivers a turn with an image; the command now opens
        // the viewer on the session's most recent image.
        app.session.update(SessionMessage::TurnStarted {
            content: "hi".into(),
            attachments: vec![shuvarie_llm::Attachment {
                kind: shuvarie_llm::AttachmentKind::Image,
                name: "shot.png".into(),
                media_type: "image/png".into(),
                size: 2048,
                sha256: "sha_x".into(),
            }],
            steered: false,
        });
        app.run_command(CommandAction::Images, None);
        assert!(matches!(app.overlay, Overlay::MediaViewer));
        assert!(app.media_view.is_open());
        assert!(
            matches!(
                rx.try_recv().unwrap(),
                shuvarie_core::Command::LoadAttachmentMedia { .. }
            ),
            "the viewer fetches the image's bytes"
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
        let declared = catalog_model_variants("ollama-cloud", "glm-5.3-flash");
        assert!(
            !declared.is_empty(),
            "the fixture model must declare variants"
        );
        assert_eq!(catalog_variants(&connected_variant()), Some(declared));
    }

    #[test]
    fn catalog_variants_is_none_for_a_model_without_variants() {
        let mut connections = connected();
        connections.active.as_mut().unwrap().model =
            Some(catalog_model_without_variants("anthropic"));
        assert_eq!(catalog_variants(&connections), None);
        assert_eq!(catalog_variants(&Connections::default()), None);
    }

    #[tokio::test]
    async fn variant_command_with_a_valid_argument_sends_select_variant() {
        let variants = catalog_model_variants("ollama-cloud", "glm-5.3-flash");
        let pick = variants
            .last()
            .expect("the fixture model must declare variants")
            .clone();
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, Some(pick.to_uppercase()));
        assert!(matches!(app.overlay, Overlay::None), "no popup opens");
        let cmd = rx.recv().await.unwrap();
        assert!(
            matches!(
                cmd,
                shuvarie_core::Command::SelectVariant {
                    variant: Some(ref v)
                } if v == &pick
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
        let variants = catalog_model_variants("ollama-cloud", "glm-5.3-flash");
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, Some("bogus".into()));
        assert!(app.session.error.is_some(), "an error is shown");
        let error = app.session.error.unwrap();
        let accepted = format!(
            "available: {}, {}",
            variant::DEFAULT_VARIANT,
            variants.join(", ")
        );
        assert!(
            error.contains("unknown variant `bogus`") && error.contains(&accepted),
            "the error names the accepted values: {error}"
        );
        assert!(
            rx.try_recv().is_err(),
            "nothing is sent for an invalid pick"
        );
    }

    #[test]
    fn variant_command_without_argument_opens_the_selector() {
        let variants = catalog_model_variants("ollama-cloud", "glm-5.3-flash");
        let current = variants
            .last()
            .expect("the fixture model must declare variants")
            .clone();
        let (mut app, mut rx) = app_with_rx(connected_variant());
        active_session(&mut app);
        app.ctx.connections.active.as_mut().unwrap().variant = Some(current.clone());
        app.run_command(CommandAction::OpenVariantPicker, None);
        assert!(matches!(app.overlay, Overlay::Variant));
        assert!(app.variant_picker.open);
        // Row 0 is the unset default, then the declared variants in order.
        let preselected = 1 + variants
            .iter()
            .position(|v| v == &current)
            .expect("the current pick must be a declared variant");
        assert_eq!(
            app.variant_picker.selected, preselected,
            "the current pick is preselected"
        );
        assert!(rx.try_recv().is_err(), "no command until Enter");
    }

    #[test]
    fn variant_command_on_a_model_without_variants_shows_an_error() {
        let mut connections = connected();
        let model = catalog_model_without_variants("anthropic");
        connections.active.as_mut().unwrap().model = Some(model.clone());
        let (mut app, mut rx) = app_with_rx(connections);
        active_session(&mut app);
        app.run_command(CommandAction::OpenVariantPicker, None);
        assert!(matches!(app.overlay, Overlay::None), "no popup opens");
        assert_eq!(
            app.session.error,
            Some(format!("model {model} has no variants"))
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

    #[test]
    fn theme_picker_opens_previews_and_restores_on_cancel() {
        let _guard = theme::lock_for_tests();
        let mut app = app_with(Connections::default());
        let original = theme::current();
        let _restore = PaletteRestore(original);
        // Seed cached sidebar lines so the repaint below is observable.
        app.update(AppMessage::Session(SessionMessage::Sidebar(
            SidebarMessage::SetTodos { done: 1, total: 2 },
        )));
        app.run_command(CommandAction::OpenThemePicker, None);
        assert!(matches!(app.overlay, Overlay::ThemePicker));
        assert!(app.theme_picker.open, "preselected the current theme");
        assert!(app.theme_backup.is_some(), "the palette snapshot is kept");
        let before = fg_bgs(&app);

        // Walking previews the highlighted palette live — both the global
        // palette and the sidebar's cached lines (which only rebuild on
        // messages) flip.
        app.update(AppMessage::Theme(ThemePickerMessage::Next));
        assert_ne!(app.theme_backup, Some(theme::current()), "preview repaints");
        assert_ne!(fg_bgs(&app), before, "the sidebar repaints during preview");

        // …and closing without a selection restores the palette at open.
        app.update(AppMessage::Theme(ThemePickerMessage::Close));
        assert!(matches!(app.overlay, Overlay::None), "the overlay closes");
        assert_eq!(theme::current(), original);
        assert_eq!(fg_bgs(&app), before, "the sidebar cache is restored");
    }

    #[tokio::test]
    async fn theme_picker_select_persists_the_choice() {
        let _guard = theme::lock_for_tests();
        let original = theme::current();
        let _restore = PaletteRestore(original);
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.run_command(CommandAction::OpenThemePicker, None);
        assert!(app.theme_picker.open);
        app.update(AppMessage::Theme(ThemePickerMessage::Next));
        let previewed = theme::current();
        app.update(AppMessage::Theme(ThemePickerMessage::Select));
        assert!(matches!(app.overlay, Overlay::None), "the picker closes");
        assert!(
            app.theme_backup.is_none(),
            "the snapshot is dropped: keep the preview"
        );
        assert_eq!(theme::current(), previewed, "the previewed palette stays");
        assert_eq!(app.current_theme_pref.as_deref(), Some("Faerun:light"));
        assert!(matches!(
            rx.try_recv().unwrap(),
            shuvarie_core::Command::SetUiTheme {
                pref: Some(ref pref)
            } if pref == "Faerun:light"
        ));
    }

    #[tokio::test]
    async fn theme_command_with_an_argument_applies_directly() {
        let _guard = theme::lock_for_tests();
        let original = theme::current();
        let _restore = PaletteRestore(original);
        let (mut app, mut rx) = app_with_rx(Connections::default());
        let expected = app
            .theme_choices
            .iter()
            .find(|choice| choice.pref.as_deref() == Some("Kanagawa:lotus"))
            .expect("the built-in choice")
            .colors;
        app.welcome.close();
        app.overlay = Overlay::None;
        app.run_command(
            CommandAction::OpenThemePicker,
            Some("Kanagawa:lotus".into()),
        );
        assert!(matches!(app.overlay, Overlay::None), "no overlay opens");
        assert_eq!(theme::current(), expected);
        assert_eq!(app.current_theme_pref.as_deref(), Some("Kanagawa:lotus"));
        assert!(matches!(
            rx.try_recv().unwrap(),
            shuvarie_core::Command::SetUiTheme {
                pref: Some(ref pref)
            } if pref == "Kanagawa:lotus"
        ));
    }

    #[tokio::test]
    async fn theme_command_default_clears_the_pref() {
        let _guard = theme::lock_for_tests();
        let original = theme::current();
        let _restore = PaletteRestore(original);
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.welcome.close();
        app.overlay = Overlay::None;
        app.run_command(CommandAction::OpenThemePicker, Some("default".into()));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.current_theme_pref, None);
        assert!(matches!(
            rx.try_recv().unwrap(),
            shuvarie_core::Command::SetUiTheme { pref: None }
        ));
    }

    #[test]
    fn theme_command_with_an_unknown_argument_shows_an_error() {
        let _guard = theme::lock_for_tests();
        let original = theme::current();
        let _restore = PaletteRestore(original);
        let (mut app, mut rx) = app_with_rx(Connections::default());
        app.welcome.close();
        app.overlay = Overlay::None;
        app.run_command(CommandAction::OpenThemePicker, Some("Nord".into()));
        assert_eq!(
            app.session.error.as_deref().map(|e| e.contains("Nord")),
            Some(true),
            "the error names the unknown theme"
        );
        assert_eq!(theme::current(), original, "the palette is untouched");
        assert!(rx.try_recv().is_err(), "no command is sent");
    }

    struct PaletteRestore(shuvarie_core::ThemeColors);

    impl Drop for PaletteRestore {
        fn drop(&mut self) {
            theme::set(self.0);
        }
    }

    /// (fg, bg) of every cached sidebar span, for repaint assertions.
    fn fg_bgs(app: &App) -> Vec<(Option<Color>, Option<Color>)> {
        app.session
            .sidebar
            .rendered_lines()
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| (s.style.fg, s.style.bg)))
            .collect()
    }
}
