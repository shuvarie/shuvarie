use ratatui::prelude::*;
use shuvarie_core::{Config, Event};
use termina::Event as TermEvent;
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};

use super::chat::ChatScreen;
use super::command_menu::{CommandMenu, CommandMenuEffect, CommandMenuMessage};
use super::context::UpdateCtx;
use super::model_select::{AddProviderForm, ModelSelectMessage, ModelSelectScreen};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Route {
    ModelSelect,
    Chat,
}

pub enum AppMessage {
    Quit,
    OpenModelSelect,
    OpenCommandMenu,
    ModelSelect(ModelSelectMessage),
    CommandMenu(CommandMenuMessage),
    Pong,
    ConfigSaved,
    ConfigError { error: String },
}

#[derive(Debug)]
pub enum AppReturn {
    Quit,
}

pub struct App {
    pub ctx: UpdateCtx,
    pub route: Route,
    pub model_select: ModelSelectScreen,
    pub command_menu: CommandMenu,
    pub chat: ChatScreen,
}

impl App {
    pub fn new(config: Config, cmd_tx: tokio::sync::mpsc::Sender<shuvarie_core::Command>) -> Self {
        let route = if config.has_connected_providers() {
            Route::Chat
        } else {
            Route::ModelSelect
        };
        let mut model_select = ModelSelectScreen::new();
        model_select.init_from_config(&config);
        Self {
            ctx: UpdateCtx::new(config, cmd_tx),
            route,
            model_select,
            command_menu: CommandMenu::new(),
            chat: ChatScreen::new(),
        }
    }

    pub fn map_core_event(ev: Event) -> AppMessage {
        match ev {
            Event::Pong => AppMessage::Pong,
            Event::ModelsLoaded {
                provider_name,
                models,
            } => AppMessage::ModelSelect(ModelSelectMessage::ModelsLoaded {
                provider_name,
                models,
            }),
            Event::ModelsError {
                provider_name,
                error,
            } => AppMessage::ModelSelect(ModelSelectMessage::ModelsError {
                provider_name,
                error,
            }),
            Event::ConfigSaved => AppMessage::ConfigSaved,
            Event::ConfigError { error } => AppMessage::ConfigError { error },
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

        if app.command_menu.open {
            return CommandMenu::handle_event(key).map(AppMessage::CommandMenu);
        }

        if app.route == Route::ModelSelect && app.model_select.add_form.is_some() {
            return app
                .model_select
                .handle_add_form_event(key)
                .map(AppMessage::ModelSelect);
        }

        if app.route == Route::ModelSelect && app.model_select.search_active {
            return app
                .model_select
                .handle_search_event(key)
                .map(AppMessage::ModelSelect);
        }

        if ctrl(&key) && key.code == KeyCode::Char('p') {
            return Some(AppMessage::OpenCommandMenu);
        }

        match app.route {
            Route::ModelSelect => {
                if key.code == KeyCode::Char('q') {
                    return Some(AppMessage::Quit);
                }
                app.model_select
                    .handle_event(key)
                    .map(AppMessage::ModelSelect)
            }
            Route::Chat => {
                if key.code == KeyCode::Char('q') {
                    return Some(AppMessage::Quit);
                }
                if key.code == KeyCode::Tab {
                    return Some(AppMessage::OpenModelSelect);
                }
                None
            }
        }
    }

    pub fn update(&mut self, msg: AppMessage) -> Option<AppReturn> {
        match msg {
            AppMessage::Quit => return Some(AppReturn::Quit),
            AppMessage::OpenModelSelect => {
                self.command_menu.close();
                self.route = Route::ModelSelect;
            }
            AppMessage::OpenCommandMenu => {
                self.command_menu.open();
            }
            AppMessage::ModelSelect(m) => {
                self.model_select.update(m, &self.ctx);
            }
            AppMessage::CommandMenu(m) => {
                if let Some(effect) = self.command_menu.update(m) {
                    match effect {
                        CommandMenuEffect::OpenModelSelect => {
                            self.route = Route::ModelSelect;
                        }
                        CommandMenuEffect::AddProvider => {
                            self.route = Route::ModelSelect;
                            self.model_select.add_form = Some(AddProviderForm::new());
                        }
                        CommandMenuEffect::Quit => return Some(AppReturn::Quit),
                    }
                }
            }
            AppMessage::Pong => {}
            AppMessage::ConfigSaved => {
                self.reload_config();
            }
            AppMessage::ConfigError { error } => {
                self.model_select
                    .update(ModelSelectMessage::ConfigError { error }, &self.ctx);
            }
        }
        None
    }

    fn reload_config(&mut self) {
        if let Ok(fresh) = Config::load() {
            self.ctx.config = fresh;
        }
    }

    pub fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        match self.route {
            Route::ModelSelect => {
                self.model_select.view(frame, area, &self.ctx.config);
            }
            Route::Chat => {
                self.chat.view(frame, area, &self.ctx.config);
            }
        }
        self.command_menu.view(frame, area);
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}
