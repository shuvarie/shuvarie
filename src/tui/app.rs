use std::collections::HashMap;

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use shuvarie_core::{Config, Event as CoreEvent, ModelInfo};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEventKind};
use tokio::sync::mpsc::Sender;

use crate::tui::event::Event;
use crate::tui::utils::ctrl;

use super::add_provider::{
    AddProviderForm, AddProviderMessage, AddProviderOutcome, AddProviderStage,
};
use super::command_menu::{CommandMenu, CommandMenuEffect, CommandMenuMessage};
use super::confirm_quit::{ConfirmQuit, ConfirmQuitEffect, ConfirmQuitMessage};
use super::context::UpdateCtx;
use super::home::{HomeEffect, HomeMessage, HomeScreen};
use super::model_picker::{ModelPicker, ModelPickerEffect, ModelPickerMessage};
use super::session::{SessionEffect, SessionMessage, SessionScreen};
use super::sidebar::SidebarMessage;
use super::theme;
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
}

#[derive(Debug)]
pub enum AppReturn {
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
    pub models: HashMap<String, Vec<ModelInfo>>,
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
            models: HashMap::new(),
        }
    }

    pub fn map_event(ev: Event, app: &App) -> Option<AppMessage> {
        match ev {
            Event::Terminal(term_ev) => match term_ev {
                TermEvent::WindowResized(size) => Some(AppMessage::Resized {
                    rows: size.rows,
                    cols: size.cols,
                }),
                TermEvent::Key(key) => {
                    // Overlay events
                    match app.overlay {
                        Overlay::CommandMenu => {
                            return CommandMenu::map_event(&key).map(AppMessage::CommandMenu);
                        }
                        Overlay::AddProvider => {
                            let stage = app
                                .add_provider_form
                                .as_ref()
                                .map(|f| f.stage)
                                .unwrap_or(AddProviderStage::SelectKind);
                            return AddProviderForm::map_event(&key, stage).map(AppMessage::AddProvider);
                        }
                        Overlay::ModelPicker => {
                            return ModelPicker::map_event(&key).map(AppMessage::ModelPicker);
                        }
                        Overlay::Welcome => {
                            return Welcome::map_event(&key).map(AppMessage::Welcome);
                        }
                        Overlay::ConfirmQuit => {
                            return ConfirmQuit::map_event(&key).map(|m| match m {
                                ConfirmQuitMessage::Confirm => AppMessage::ConfirmQuit,
                                ConfirmQuitMessage::Cancel => AppMessage::CancelQuit,
                            });
                        }
                        Overlay::None => {}
                    }

                    // App key event
                    #[allow(clippy::single_match)]
                    match key.kind {
                        KeyEventKind::Press => match key.code {
                            KeyCode::Char('c') if ctrl(&key) => {
                                if app.route == Route::Session && app.session.streaming {
                                    return Some(AppMessage::Session(SessionMessage::CancelRequested));
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

                    match app.route {
                        Route::Home => app.home.map_event(&key).map(AppMessage::Home),
                        Route::Session => app.session.map_event(&key).map(AppMessage::Session),
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
                CoreEvent::SessionStarted => Some(AppMessage::Session(SessionMessage::Reset)),
                CoreEvent::TokenReceived { content } => {
                    Some(AppMessage::Session(SessionMessage::TokenReceived { content }))
                }
                CoreEvent::StreamDone { .. } => Some(AppMessage::Session(SessionMessage::StreamDone)),
                CoreEvent::StreamError { error } => {
                    Some(AppMessage::Session(SessionMessage::StreamError { error }))
                }
                CoreEvent::StreamCancelled => Some(AppMessage::Session(SessionMessage::StreamCancelled)),
                CoreEvent::UsageUpdate { usage, cost } => {
                    Some(AppMessage::Session(SessionMessage::UsageUpdate { usage, cost }))
                }
            },
        }
    }

    pub fn update(&mut self, msg: AppMessage) -> Option<AppReturn> {
        match msg {
            AppMessage::OpenCommandMenu => {
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
                    return Some(AppReturn::Quit);
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
                        ModelPickerEffect::Close => {}
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
                        }
                        CommandMenuEffect::AddProvider => {
                            let names: Vec<String> =
                                self.ctx.config.providers.keys().cloned().collect();
                            self.add_provider_form = Some(AddProviderForm::new(&names));
                            self.overlay = Overlay::AddProvider;
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
                if let Some(form) = &mut self.add_provider_form {
                    form.error = Some(format!("{provider_name}: {error}"));
                }
            }
        }
        None
    }

    fn start_session(&mut self, content: String) {
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

    fn close_overlay(&mut self) {
        self.overlay = Overlay::None;
        self.command_menu.close();
        self.add_provider_form = None;
        self.model_picker.close();
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
        let [content_area, footer_area] = Layout::vertical([Min(0), Length(1)]).areas(area);

        let padded = Rect::new(
            content_area.x + 1,
            content_area.y + 1,
            content_area.width.saturating_sub(2),
            content_area.height.saturating_sub(2),
        );
        match self.route {
            Route::Home => self.home.view(frame, padded),
            Route::Session => self.session.view(frame, padded),
        }

        let footer = self.build_footer();
        frame.render_widget(Paragraph::new(footer), footer_area);

        self.welcome.view(frame, area);
        if let Some(form) = &self.add_provider_form {
            form.view(frame, area);
        }
        self.model_picker.view(frame, area);
        self.command_menu.view(frame, area);
        self.confirm_quit.view(frame, area);
    }

    fn build_footer(&self) -> Line<'static> {
        if self.confirm_quit.open {
            return theme::help_line(&[
                ("Enter", "confirm"),
                ("Ctrl+C", "confirm"),
                ("Esc", "cancel"),
            ]);
        }
        if self.command_menu.open {
            return theme::help_line(&[("Enter", "run"), ("Esc", "close"), ("↑↓", "navigate")]);
        }
        if self.overlay == Overlay::AddProvider {
            let stage = self
                .add_provider_form
                .as_ref()
                .map(|f| f.stage)
                .unwrap_or(AddProviderStage::SelectKind);
            return match stage {
                AddProviderStage::SelectKind => theme::help_line(&[
                    ("↑↓", "navigate"),
                    ("Enter", "continue"),
                    ("Esc", "cancel"),
                ]),
                AddProviderStage::Details => {
                    theme::help_line(&[("Tab", "next field"), ("Enter", "submit"), ("Esc", "back")])
                }
            };
        }
        if self.overlay == Overlay::ModelPicker {
            return theme::help_line(&[
                ("Type", "to filter"),
                ("Esc", "close"),
                ("Enter", "select"),
            ]);
        }
        if self.overlay == Overlay::Welcome {
            return theme::help_line(&[("Enter", "add provider"), ("Ctrl+C", "quit")]);
        }

        if self.route == Route::Session && self.session.streaming {
            return theme::help_line(&[("Ctrl+C", "stop"), ("Ctrl+M", "commands")]);
        }

        if let Some(status) = &self.session.status {
            return Line::from(vec![
                Span::raw(" ").fg(theme::TEXT_MUTED),
                Span::raw(status.clone()).fg(theme::TEXT_MUTED),
            ]);
        }

        match self.route {
            Route::Home => theme::help_line(&[
                ("Enter", "send"),
                ("Ctrl+M", "commands"),
                ("Ctrl+C", "quit"),
            ]),
            Route::Session => theme::help_line(&[
                ("Enter", "send"),
                ("Ctrl+M", "commands"),
                ("Ctrl+C", "quit"),
            ]),
        }
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
    overlay_inner_list_height(area, 50, 55, 2)
}
