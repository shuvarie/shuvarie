use std::collections::HashMap;

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use shuvarie_core::{Config, Event, ModelInfo};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

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
    pub fn new(config: Config, cmd_tx: tokio::sync::mpsc::Sender<shuvarie_core::Command>) -> Self {
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

    pub fn map_core_event(ev: Event) -> AppMessage {
        match ev {
            Event::Pong => AppMessage::CommandMenu(CommandMenuMessage::Close),
            Event::ModelsLoaded {
                provider_name,
                models,
            } => AppMessage::ModelsLoaded {
                provider_name,
                models,
            },
            Event::ModelsError {
                provider_name,
                error,
            } => AppMessage::ModelsError {
                provider_name,
                error,
            },
            Event::ConfigSaved => AppMessage::ConfigSaved,
            Event::ConfigError { error } => AppMessage::ConfigError { error },
            Event::SessionStarted => AppMessage::Session(SessionMessage::Reset),
            Event::MessageReceived { content } => {
                AppMessage::Session(SessionMessage::MessageReceived { content })
            }
            Event::ReplyError { error } => {
                AppMessage::Session(SessionMessage::ReplyError { error })
            }
        }
    }

    pub fn handle_event(event: &TermEvent, app: &App) -> Option<AppMessage> {
        let TermEvent::Key(key) = event else {
            return None;
        };
        if key.kind != KeyEventKind::Press {
            return None;
        }
        let key = *key;

        match app.overlay {
            Overlay::CommandMenu => {
                return CommandMenu::handle_event(key).map(AppMessage::CommandMenu);
            }
            Overlay::AddProvider => {
                let stage = app
                    .add_provider_form
                    .as_ref()
                    .map(|f| f.stage)
                    .unwrap_or(AddProviderStage::SelectKind);
                return AddProviderForm::handle_event(key, stage).map(AppMessage::AddProvider);
            }
            Overlay::ModelPicker => {
                return ModelPicker::handle_event(key).map(AppMessage::ModelPicker);
            }
            Overlay::Welcome => {
                return Welcome::handle_event(key).map(AppMessage::Welcome);
            }
            Overlay::ConfirmQuit => {
                return ConfirmQuit::handle_event(key).map(|m| match m {
                    ConfirmQuitMessage::Confirm => AppMessage::ConfirmQuit,
                    ConfirmQuitMessage::Cancel => AppMessage::CancelQuit,
                });
            }
            Overlay::None => {}
        }

        if ctrl(&key) && key.code == KeyCode::Char('c') {
            return Some(AppMessage::RequestQuit);
        }

        if ctrl(&key) && key.code == KeyCode::Char('m') {
            return Some(AppMessage::OpenCommandMenu);
        }

        match app.route {
            Route::Home => app.home.handle_event(key).map(AppMessage::Home),
            Route::Session => app.session.handle_event(key).map(AppMessage::Session),
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
            });
        }
    }

    pub fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
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
        frame.render_widget(Paragraph::new(footer).bg(theme::SURFACE), footer_area);

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

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}
