use std::collections::HashMap;

use ratatui::prelude::*;
use shuvarie_core::{Config, Event as CoreEvent, ModelInfo};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEventKind};
use tokio::sync::mpsc::Sender;

use crate::tui::event::Event;
use crate::tui::utils::ctrl;

use super::add_provider::{AddProviderForm, AddProviderMessage, AddProviderOutcome};
use super::command_menu::{CommandMenu, CommandMenuEffect, CommandMenuMessage};
use super::confirm_quit::{ConfirmQuit, ConfirmQuitEffect, ConfirmQuitMessage};
use super::context::UpdateCtx;
use super::home::{HomeEffect, HomeMessage, HomeScreen};
use super::model_picker::{ModelPicker, ModelPickerEffect, ModelPickerMessage};
use super::session::{SessionEffect, SessionMessage, SessionScreen};
use super::session_picker::{SessionPicker, SessionPickerEffect, SessionPickerMessage};
use super::sidebar::SidebarMessage;
use super::welcome::{Welcome, WelcomeEffect, WelcomeMessage};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Home,
    Session,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    None,
    Welcome,
    AddProvider,
    ModelPicker,
    CommandMenu,
    ConfirmQuit,
    SessionPicker,
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
    Home(HomeMessage),
    Session(SessionMessage),
    AddProvider(AddProviderMessage),
    ModelPicker(ModelPickerMessage),
    CommandMenu(CommandMenuMessage),
    Welcome(WelcomeMessage),
    SessionPicker(SessionPickerMessage),
    ConfigSaved,
    ConfigError {
        error: String,
    },
    ModelsLoaded {
        provider_name: String,
        models: Vec<ModelInfo>,
    },
    ModelsError {
        provider_name: String,
        error: String,
    },
    SessionsLoaded {
        sessions: Vec<shuvarie_core::SessionSummary>,
    },
    SessionLoaded {
        id: u64,
        title: String,
        session: shuvarie_core::Session,
    },
    SessionCreated {
        id: u64,
        title: String,
    },
    SessionDeleted {
        id: u64,
    },
    SessionError {
        error: String,
    },
}

#[derive(Debug)]
pub enum AppEffect {
    Quit,
}

pub struct App {
    pub ctx: UpdateCtx,
    pub route: Route,
    pub overlay: Overlay,
    pub home: HomeScreen,
    pub session: SessionScreen,
    pub command_menu: CommandMenu,
    pub welcome: Welcome,
    pub confirm_quit: ConfirmQuit,
    pub add_provider_form: Option<AddProviderForm>,
    pub model_picker: ModelPicker,
    pub session_picker: SessionPicker,
    pub models: HashMap<String, Vec<ModelInfo>>,
    pending_model_pick: Option<String>,
}

impl App {
    pub fn new(config: Config, cmd_tx: Sender<shuvarie_core::Command>) -> Self {
        let route = Route::Home;
        let mut welcome = Welcome::new();
        if !config.has_connected_providers() {
            welcome.open();
        }
        let initial_provider = config.active_provider.clone();
        let initial_model = config.active_model.clone();
        Self {
            ctx: UpdateCtx::new(config, cmd_tx),
            route,
            overlay: if welcome.open {
                Overlay::Welcome
            } else {
                Overlay::None
            },
            home: HomeScreen::new(),
            session: {
                let mut s = SessionScreen::new();
                s.sidebar.update(SidebarMessage::UpdateConfig {
                    provider: initial_provider,
                    model: initial_model,
                    context_length: None,
                });
                s
            },
            command_menu: CommandMenu::new(),
            welcome,
            confirm_quit: ConfirmQuit::new(),
            add_provider_form: None,
            model_picker: ModelPicker::new(),
            session_picker: SessionPicker::new(),
            models: HashMap::new(),
            pending_model_pick: None,
        }
    }

    pub fn map_event(&self, ev: Event) -> Option<AppMessage> {
        match ev {
            Event::Terminal(term_ev) => match term_ev {
                TermEvent::WindowResized(size) => Some(AppMessage::Resized {
                    rows: size.rows,
                    cols: size.cols,
                }),
                TermEvent::Key(key) => {
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
                        Overlay::None => {}
                    }

                    // App key event
                    #[allow(clippy::single_match)]
                    match key.kind {
                        KeyEventKind::Press => match key.code {
                            KeyCode::Char('c') if ctrl(&key) => {
                                if self.route == Route::Session && self.session.streaming {
                                    return Some(AppMessage::Session(
                                        SessionMessage::CancelRequested,
                                    ));
                                }
                                return Some(AppMessage::RequestQuit);
                            }
                            KeyCode::Char('m') if ctrl(&key) => {
                                return Some(AppMessage::OpenCommandMenu);
                            }
                            _ => {}
                        },
                        _ => {}
                    }

                    match self.route {
                        Route::Home => self.home.map_event(&key).map(AppMessage::Home),
                        Route::Session => self.session.map_event(&key).map(AppMessage::Session),
                    }
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
                CoreEvent::TokenReceived { content } => {
                    Some(AppMessage::Session(SessionMessage::TokenReceived {
                        content,
                    }))
                }
                CoreEvent::ContextLoaded { paths } => {
                    Some(AppMessage::Session(SessionMessage::ContextLoaded { paths }))
                }
                CoreEvent::ToolStarted { name, args, worker } => {
                    Some(AppMessage::Session(SessionMessage::ToolStarted {
                        name,
                        args,
                        worker,
                    }))
                }
                CoreEvent::ToolFinished {
                    name,
                    ok,
                    output,
                    worker,
                } => Some(AppMessage::Session(SessionMessage::ToolFinished {
                    name,
                    ok,
                    output,
                    worker,
                })),
                CoreEvent::WorkerStarted { name, args } => {
                    Some(AppMessage::Session(SessionMessage::WorkerStarted {
                        name,
                        args,
                    }))
                }
                CoreEvent::WorkerFinished { name, ok, output } => {
                    Some(AppMessage::Session(SessionMessage::WorkerFinished {
                        name,
                        ok,
                        output,
                    }))
                }
                CoreEvent::StreamDone { .. } => {
                    Some(AppMessage::Session(SessionMessage::StreamDone))
                }
                CoreEvent::StreamError { error } => {
                    Some(AppMessage::Session(SessionMessage::StreamError { error }))
                }
                CoreEvent::StreamCancelled => {
                    Some(AppMessage::Session(SessionMessage::StreamCancelled))
                }
                CoreEvent::UsageUpdate { usage, cost } => {
                    Some(AppMessage::Session(SessionMessage::UsageUpdate {
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
            },
        }
    }

    pub fn update(&mut self, msg: AppMessage) -> Option<AppEffect> {
        match msg {
            AppMessage::OpenCommandMenu => {
                self.session.update(SessionMessage::ClearError);
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
            AppMessage::Home(m) => {
                if let Some(effect) = self.home.update(m) {
                    match effect {
                        HomeEffect::Submit { content } => {
                            self.start_session(content);
                        }
                    }
                }
            }
            AppMessage::Session(m) => {
                if let Some(effect) = self.session.update(m) {
                    match effect {
                        SessionEffect::SendMessage { content } => {
                            self.ctx
                                .send(shuvarie_core::Command::SendMessage { content });
                        }
                        SessionEffect::CancelStream => {
                            self.ctx.send(shuvarie_core::Command::CancelStream);
                        }
                    }
                }
            }
            AppMessage::SessionPicker(m) => {
                if let Some(effect) = self.session_picker.update(m) {
                    match effect {
                        SessionPickerEffect::LoadSession { id } => {
                            self.route = Route::Session;
                            self.ctx.send(shuvarie_core::Command::LoadSession { id });
                            self.close_overlay();
                        }
                        SessionPickerEffect::DeleteSession { id } => {
                            self.ctx.send(shuvarie_core::Command::DeleteSession { id });
                        }
                        SessionPickerEffect::NewSession => {
                            self.route = Route::Session;
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
                            name,
                            api_key,
                            base_url,
                        } => {
                            let pc = shuvarie_core::ProviderConfig::new(kind, api_key, base_url);
                            self.pending_model_pick = Some(name.clone());
                            self.ctx.send(shuvarie_core::Command::AddProvider {
                                name: name.clone(),
                                config: pc,
                            });
                            self.ctx.send(shuvarie_core::Command::SetActiveProvider {
                                name: name.clone(),
                            });
                            self.ctx.send(shuvarie_core::Command::ListModels {
                                provider_name: name,
                            });
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
                            if self.ctx.config.active_model.is_none()
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
                if let Some(effect) = self.command_menu.update(m) {
                    match effect {
                        CommandMenuEffect::OpenModelSelect => {
                            let models = self
                                .models
                                .get(self.ctx.config.active_provider.as_deref().unwrap_or(""))
                                .cloned()
                                .unwrap_or_default();
                            self.model_picker.open(&models);
                            self.overlay = Overlay::ModelPicker;
                            return None;
                        }
                        CommandMenuEffect::AddProvider => {
                            let names: Vec<String> =
                                self.ctx.config.providers.keys().cloned().collect();
                            self.add_provider_form = Some(AddProviderForm::new(&names));
                            self.overlay = Overlay::AddProvider;
                            return None;
                        }
                        CommandMenuEffect::OpenSessionPicker => {
                            self.session_picker.open(self.session.session_id);
                            self.refresh_sessions();
                            self.overlay = Overlay::SessionPicker;
                            return None;
                        }
                        CommandMenuEffect::NewSession => {
                            self.route = Route::Session;
                            self.session.update(SessionMessage::Reset);
                            self.ctx.send(shuvarie_core::Command::NewSession);
                        }
                    }
                }
                if !self.command_menu.open {
                    self.overlay = Overlay::None;
                }
            }
            AppMessage::Welcome(m) => {
                if let Some(effect) = self.welcome.update(m) {
                    match effect {
                        WelcomeEffect::AddProvider => {
                            let names: Vec<String> =
                                self.ctx.config.providers.keys().cloned().collect();
                            self.add_provider_form = Some(AddProviderForm::new(&names));
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
                if Some(provider_name.as_str()) == self.ctx.config.active_provider.as_deref() {
                    let models = self.models.get(&provider_name).unwrap();
                    let current = self.ctx.config.active_model.as_deref();
                    let chosen = current
                        .filter(|c| models.iter().any(|m| m.id == *c))
                        .map(|c| c.to_string())
                        .or_else(|| models.first().map(|m| m.id.clone()));
                    if let Some(model) = chosen
                        && self.ctx.config.active_model.as_deref() != Some(model.as_str())
                    {
                        self.ctx
                            .send(shuvarie_core::Command::SetActiveModel { model });
                    } else {
                        self.session.update(SessionMessage::UpdateConfig {
                            provider: Some(provider_name.clone()),
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
                self.route = Route::Session;
                self.session
                    .update(SessionMessage::Loaded { id, title, session });
                self.session_picker.active_id = Some(id);
            }
            AppMessage::SessionDeleted { id } => {
                if self.session.session_id == Some(id) {
                    self.session.update(SessionMessage::Reset);
                }
                self.refresh_sessions();
            }
            AppMessage::SessionError { error } => {
                self.session.update(SessionMessage::ShowError { error });
            }
        }
        None
    }

    fn start_session(&mut self, content: String) {
        self.session.update(SessionMessage::ClearError);
        self.route = Route::Session;
        self.session.messages.clear();
        self.session
            .messages
            .push((shuvarie_core::Role::User, content.clone()));
        self.session.status = Some("thinking…".to_string());
        self.ctx.send(shuvarie_core::Command::StartSession);
        self.ctx
            .send(shuvarie_core::Command::SendMessage { content });
    }

    fn refresh_sessions(&mut self) {
        self.ctx.send(shuvarie_core::Command::ListSessions);
    }

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.command_menu.close();
        self.add_provider_form = None;
        self.model_picker.close();
        self.session_picker.close();
        self.confirm_quit.close();
        if self.welcome.open {
            self.welcome.close();
        }
    }

    fn active_context_length(&self) -> Option<u64> {
        let provider = self.ctx.config.active_provider.as_deref()?;
        let model = self.ctx.config.active_model.as_deref()?;
        self.models
            .get(provider)?
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.context_length)
    }

    fn reload_config(&mut self) {
        if let Ok(fresh) = Config::load() {
            self.ctx.config = fresh;
            if self.ctx.config.has_connected_providers() && self.welcome.open {
                self.welcome.close();
                self.overlay = Overlay::None;
            }
            self.session.update(SessionMessage::UpdateConfig {
                provider: self.ctx.config.active_provider.clone(),
                model: self.ctx.config.active_model.clone(),
                context_length: self.active_context_length(),
            });
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        match self.route {
            Route::Home => self.home.view(frame, area),
            Route::Session => self.session.view(frame, area),
        }

        self.welcome.view(frame, area);
        if let Some(form) = &self.add_provider_form {
            form.view(frame, area);
        }
        self.model_picker.view(frame, area);
        self.session_picker.view(frame, area);
        self.command_menu.view(frame, area);
        self.confirm_quit.view(frame, area);
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
