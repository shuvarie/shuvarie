use std::collections::HashMap;

use ratatui::layout::Constraint::{Fill, Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};
use shuvarie_core::{Config, ModelInfo, ProviderConfig};
use shuvarie_llm::Provider;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::context::UpdateCtx;
use super::search::Search;
use super::theme;
use super::widgets::InputBuffer;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Providers,
    Models,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    Kind,
    ApiKey,
    BaseUrl,
}

pub enum ModelSelectMessage {
    AddProvider,
    RemoveProvider,
    SelectProvider,
    SelectModel,
    NextProvider,
    PrevProvider,
    NextModel,
    PrevModel,
    FirstItem,
    LastItem,
    FocusSearch,
    BlurSearch,
    SearchInput(char),
    SearchBackspace,
    TabPane,
    AddProviderInput(char),
    AddProviderBackspace,
    AddProviderNextField,
    AddProviderPrevField,
    AddProviderCycleKind(i32),
    AddProviderSubmit,
    AddProviderCancel,
    ModelsLoaded {
        provider_name: String,
        models: Vec<ModelInfo>,
    },
    ModelsError {
        provider_name: String,
        error: String,
    },
    ConfigError {
        error: String,
    },
}

pub struct AddProviderForm {
    pub name: InputBuffer,
    pub kind_idx: usize,
    pub api_key: InputBuffer,
    pub base_url: InputBuffer,
    pub field: FormField,
    pub error: Option<String>,
}

impl AddProviderForm {
    pub fn new() -> Self {
        Self {
            name: InputBuffer::new(),
            kind_idx: 0,
            api_key: InputBuffer::new(),
            base_url: InputBuffer::new(),
            field: FormField::Name,
            error: None,
        }
    }

    pub fn next_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::Kind,
            FormField::Kind => FormField::ApiKey,
            FormField::ApiKey => FormField::BaseUrl,
            FormField::BaseUrl => FormField::Name,
        };
    }

    pub fn prev_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::BaseUrl,
            FormField::Kind => FormField::Name,
            FormField::ApiKey => FormField::Kind,
            FormField::BaseUrl => FormField::ApiKey,
        };
    }

    fn field_indicator(&self, field: FormField, value: &str) -> String {
        if self.field == field {
            format!("[{value}]")
        } else {
            format!(" {value} ")
        }
    }
}

impl Default for AddProviderForm {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ModelSelectScreen {
    pub providers_state: ListState,
    pub models_state: ListState,
    pub focused: Pane,
    pub search: Search,
    pub search_active: bool,
    pub add_form: Option<AddProviderForm>,
    pub status: Option<String>,
    pub loading: Option<String>,
    pub filtered_models: Vec<usize>,
    pub models: HashMap<String, Vec<ModelInfo>>,
}

impl ModelSelectScreen {
    pub fn new() -> Self {
        Self {
            providers_state: ListState::default(),
            models_state: ListState::default(),
            focused: Pane::Providers,
            search: Search::new(),
            search_active: false,
            add_form: None,
            status: None,
            loading: None,
            filtered_models: Vec::new(),
            models: HashMap::new(),
        }
    }

    pub fn init_from_config(&mut self, config: &Config) {
        if !config.providers.is_empty() {
            self.providers_state.select(Some(0));
            if let Some(active) = &config.active_provider {
                let names = Self::provider_names(config);
                if let Some(idx) = names.iter().position(|n| n == active) {
                    self.providers_state.select(Some(idx));
                }
            }
        }
    }

    pub fn provider_names(config: &Config) -> Vec<String> {
        let mut names: Vec<String> = config.providers.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn selected_provider_name(&self, config: &Config) -> Option<String> {
        let names = Self::provider_names(config);
        let idx = self.providers_state.selected()?;
        names.get(idx).cloned()
    }

    pub fn selected_provider<'a>(
        &self,
        config: &'a Config,
    ) -> Option<(String, &'a ProviderConfig)> {
        let name = self.selected_provider_name(config)?;
        config.providers.get(&name).map(|p| (name, p))
    }

    fn set_models(&mut self, models: &[ModelInfo]) {
        self.refilter_models(models);
    }

    fn refilter_models(&mut self, models: &[ModelInfo]) {
        self.filtered_models = self
            .search
            .filter_indices(models.len(), |i| models[i].id.clone());
        if !self.filtered_models.is_empty() {
            self.models_state.select(Some(0));
        } else {
            self.models_state.select(None);
        }
    }

    fn next_provider(&mut self, config: &Config) {
        let count = config.providers.len();
        if count == 0 {
            return;
        }
        let i = self.providers_state.selected().unwrap_or(0);
        let next = (i + 1).min(count - 1);
        self.providers_state.select(Some(next));
    }

    fn prev_provider(&mut self, config: &Config) {
        let count = config.providers.len();
        if count == 0 {
            return;
        }
        let i = self.providers_state.selected().unwrap_or(0);
        let prev = i.saturating_sub(1);
        self.providers_state.select(Some(prev));
    }

    fn first_provider(&mut self, config: &Config) {
        if config.providers.is_empty() {
            return;
        }
        self.providers_state.select(Some(0));
    }

    fn last_provider(&mut self, config: &Config) {
        let count = config.providers.len();
        if count == 0 {
            return;
        }
        self.providers_state.select(Some(count - 1));
    }

    fn next_model(&mut self) {
        if self.filtered_models.is_empty() {
            return;
        }
        let i = self.models_state.selected().unwrap_or(0);
        let next = (i + 1).min(self.filtered_models.len() - 1);
        self.models_state.select(Some(next));
    }

    fn prev_model(&mut self) {
        if self.filtered_models.is_empty() {
            return;
        }
        let i = self.models_state.selected().unwrap_or(0);
        let prev = i.saturating_sub(1);
        self.models_state.select(Some(prev));
    }

    fn first_model(&mut self) {
        if !self.filtered_models.is_empty() {
            self.models_state.select(Some(0));
        }
    }

    fn last_model(&mut self) {
        if !self.filtered_models.is_empty() {
            self.models_state
                .select(Some(self.filtered_models.len() - 1));
        }
    }

    fn selected_model_id<'a>(&self, models: &'a [ModelInfo]) -> Option<&'a str> {
        let idx = self.models_state.selected()?;
        let orig = *self.filtered_models.get(idx)?;
        Some(&models[orig].id)
    }

    pub fn handle_event(&self, key: KeyEvent) -> Option<ModelSelectMessage> {
        if ctrl(&key) && key.code == KeyCode::Char('p') {
            return None;
        }
        match key.code {
            KeyCode::Tab => Some(ModelSelectMessage::TabPane),
            KeyCode::Char('a') => Some(ModelSelectMessage::AddProvider),
            KeyCode::Char('d') => Some(ModelSelectMessage::RemoveProvider),
            KeyCode::Char('/') => Some(ModelSelectMessage::FocusSearch),
            KeyCode::Enter => match self.focused {
                Pane::Providers => Some(ModelSelectMessage::SelectProvider),
                Pane::Models => Some(ModelSelectMessage::SelectModel),
            },
            KeyCode::Down | KeyCode::Char('j') => match self.focused {
                Pane::Providers => Some(ModelSelectMessage::NextProvider),
                Pane::Models => Some(ModelSelectMessage::NextModel),
            },
            KeyCode::Up | KeyCode::Char('k') => match self.focused {
                Pane::Providers => Some(ModelSelectMessage::PrevProvider),
                Pane::Models => Some(ModelSelectMessage::PrevModel),
            },
            KeyCode::Char('g') => Some(ModelSelectMessage::FirstItem),
            KeyCode::Char('G') => Some(ModelSelectMessage::LastItem),
            _ => None,
        }
    }

    pub fn handle_search_event(&self, key: KeyEvent) -> Option<ModelSelectMessage> {
        match key.code {
            KeyCode::Escape => Some(ModelSelectMessage::BlurSearch),
            KeyCode::Backspace => Some(ModelSelectMessage::SearchBackspace),
            KeyCode::Char(c) if !ctrl(&key) => Some(ModelSelectMessage::SearchInput(c)),
            KeyCode::Enter => Some(ModelSelectMessage::BlurSearch),
            _ => None,
        }
    }

    pub fn handle_add_form_event(&self, key: KeyEvent) -> Option<ModelSelectMessage> {
        let form_field = self.add_form.as_ref().map(|f| f.field);
        match key.code {
            KeyCode::Escape => Some(ModelSelectMessage::AddProviderCancel),
            KeyCode::Tab => Some(ModelSelectMessage::AddProviderNextField),
            KeyCode::BackTab => Some(ModelSelectMessage::AddProviderPrevField),
            KeyCode::Enter => Some(ModelSelectMessage::AddProviderSubmit),
            KeyCode::Left if form_field == Some(FormField::Kind) => {
                Some(ModelSelectMessage::AddProviderCycleKind(-1))
            }
            KeyCode::Right if form_field == Some(FormField::Kind) => {
                Some(ModelSelectMessage::AddProviderCycleKind(1))
            }
            KeyCode::Backspace => Some(ModelSelectMessage::AddProviderBackspace),
            KeyCode::Char(c) if !ctrl(&key) => Some(ModelSelectMessage::AddProviderInput(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ModelSelectMessage, ctx: &UpdateCtx) {
        match msg {
            ModelSelectMessage::AddProvider => {
                self.add_form = Some(AddProviderForm::new());
            }
            ModelSelectMessage::RemoveProvider => {
                if let Some(name) = self.selected_provider_name(&ctx.config) {
                    ctx.send(shuvarie_core::Command::RemoveProvider { name });
                }
            }
            ModelSelectMessage::SelectProvider => {
                if let Some(name) = self.selected_provider_name(&ctx.config) {
                    ctx.send(shuvarie_core::Command::SetActiveProvider { name: name.clone() });
                    self.loading = Some(format!("Fetching models for {name}…"));
                    ctx.send(shuvarie_core::Command::ListModels {
                        provider_name: name,
                    });
                }
            }
            ModelSelectMessage::SelectModel => {
                let active_provider = self.selected_provider_name(&ctx.config);
                if let Some(pname) = active_provider
                    && let Some(models) = self.models.get(&pname)
                    && let Some(mid) = self.selected_model_id(models)
                {
                    ctx.send(shuvarie_core::Command::SetActiveModel {
                        model: mid.to_string(),
                    });
                }
            }
            ModelSelectMessage::NextProvider => {
                self.next_provider(&ctx.config);
                self.auto_request_models(ctx);
            }
            ModelSelectMessage::PrevProvider => {
                self.prev_provider(&ctx.config);
                self.auto_request_models(ctx);
            }
            ModelSelectMessage::NextModel => self.next_model(),
            ModelSelectMessage::PrevModel => self.prev_model(),
            ModelSelectMessage::FirstItem => match self.focused {
                Pane::Providers => self.first_provider(&ctx.config),
                Pane::Models => self.first_model(),
            },
            ModelSelectMessage::LastItem => match self.focused {
                Pane::Providers => self.last_provider(&ctx.config),
                Pane::Models => self.last_model(),
            },
            ModelSelectMessage::FocusSearch => {
                self.search_active = true;
                self.search.clear();
                self.focused = Pane::Models;
            }
            ModelSelectMessage::BlurSearch => {
                self.search_active = false;
            }
            ModelSelectMessage::SearchInput(c) => {
                self.search.push(c);
                self.refilter_for_selected(&ctx.config);
            }
            ModelSelectMessage::SearchBackspace => {
                self.search.backspace();
                self.refilter_for_selected(&ctx.config);
            }
            ModelSelectMessage::TabPane => {
                self.focused = match self.focused {
                    Pane::Providers => Pane::Models,
                    Pane::Models => Pane::Providers,
                };
            }
            ModelSelectMessage::AddProviderInput(c) => {
                if let Some(form) = &mut self.add_form {
                    match form.field {
                        FormField::Name => form.name.push(c),
                        FormField::ApiKey => form.api_key.push(c),
                        FormField::BaseUrl => form.base_url.push(c),
                        FormField::Kind => {}
                    }
                }
            }
            ModelSelectMessage::AddProviderBackspace => {
                if let Some(form) = &mut self.add_form {
                    match form.field {
                        FormField::Name => form.name.backspace(),
                        FormField::ApiKey => form.api_key.backspace(),
                        FormField::BaseUrl => form.base_url.backspace(),
                        FormField::Kind => {}
                    }
                }
            }
            ModelSelectMessage::AddProviderNextField => {
                if let Some(form) = &mut self.add_form {
                    form.next_field();
                }
            }
            ModelSelectMessage::AddProviderPrevField => {
                if let Some(form) = &mut self.add_form {
                    form.prev_field();
                }
            }
            ModelSelectMessage::AddProviderCycleKind(delta) => {
                if let Some(form) = &mut self.add_form {
                    let len = Provider::ALL.len() as i32;
                    let mut idx = form.kind_idx as i32 + delta;
                    idx = ((idx % len) + len) % len;
                    form.kind_idx = idx as usize;
                    let kind = Provider::ALL[form.kind_idx];
                    if form.base_url.is_empty()
                        && let Some(url) = kind.default_base_url()
                    {
                        form.base_url.set(url);
                    }
                    form.error = None;
                }
            }
            ModelSelectMessage::AddProviderSubmit => {
                if let Some(form) = self.add_form.take() {
                    let name = form.name.value.trim().to_string();
                    if name.is_empty() {
                        let mut f = AddProviderForm::new();
                        f.error = Some("Name is required".into());
                        self.add_form = Some(f);
                        return;
                    }
                    if ctx.config.providers.contains_key(&name) {
                        let mut f = form;
                        f.error = Some(format!("Provider '{name}' already exists"));
                        self.add_form = Some(f);
                        return;
                    }
                    let kind = Provider::ALL[form.kind_idx];
                    let api_key = if kind.requires_api_key() {
                        let k = form.api_key.value.trim().to_string();
                        if k.is_empty() {
                            let mut f = form;
                            f.error = Some(format!("{} requires an API key", kind.display_name()));
                            self.add_form = Some(f);
                            return;
                        }
                        Some(k)
                    } else {
                        None
                    };
                    let base_url = if form.base_url.value.trim().is_empty() {
                        None
                    } else {
                        Some(form.base_url.value.trim().to_string())
                    };
                    let pc = ProviderConfig::new(kind, api_key, base_url);
                    ctx.send(shuvarie_core::Command::AddProvider {
                        name: name.clone(),
                        config: pc,
                    });
                    ctx.send(shuvarie_core::Command::ListModels {
                        provider_name: name,
                    });
                }
            }
            ModelSelectMessage::AddProviderCancel => {
                self.add_form = None;
            }
            ModelSelectMessage::ModelsLoaded {
                provider_name,
                models,
            } => {
                self.loading = None;
                self.status = None;
                let empty = models.is_empty();
                self.refilter_models(&models);
                self.models.insert(provider_name.clone(), models);
                if empty {
                    self.status = Some(format!("No models returned for '{provider_name}'"));
                }
            }
            ModelSelectMessage::ModelsError {
                provider_name,
                error,
            } => {
                self.loading = None;
                self.status = Some(format!("{provider_name}: {error}"));
            }
            ModelSelectMessage::ConfigError { error } => {
                self.status = Some(format!("Config error: {error}"));
            }
        }
    }

    fn auto_request_models(&mut self, ctx: &UpdateCtx) {
        if let Some(name) = self.selected_provider_name(&ctx.config) {
            if !self.models.contains_key(&name) {
                self.loading = Some(format!("Fetching models for {name}…"));
                ctx.send(shuvarie_core::Command::ListModels {
                    provider_name: name,
                });
                return;
            }
            self.loading = None;
            let models = self.models.get(&name).cloned();
            if let Some(ms) = models {
                self.set_models(&ms);
            }
        }
    }

    fn refilter_for_selected(&mut self, config: &Config) {
        let name = self.selected_provider_name(config);
        let models = name.and_then(|n| self.models.get(&n).cloned());
        if let Some(ms) = models {
            self.refilter_models(&ms);
        }
    }

    pub fn view(&mut self, frame: &mut Frame<'_>, area: Rect, config: &Config) {
        let [left, right] = Layout::horizontal([Fill(1), Fill(2)])
            .spacing(1)
            .areas(area);

        let names = Self::provider_names(config);
        let item_fg = if self.focused == Pane::Providers {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };
        let provider_items: Vec<ListItem> = names
            .iter()
            .map(|n| {
                let is_active = config.active_provider.as_deref() == Some(n.as_str());
                let marker = theme::active_marker(is_active);
                let pc = &config.providers[n];
                ListItem::new(format!("{marker}{n}  [{}]", pc.kind.display_name())).fg(item_fg)
            })
            .collect();
        let provider_block = theme::section_block("Providers", self.focused == Pane::Providers);
        frame.render_stateful_widget(
            List::new(provider_items)
                .block(provider_block)
                .highlight_style(Style::new().bg(theme::ACCENT_BG).fg(theme::TEXT))
                .highlight_symbol("▶ "),
            left,
            &mut self.providers_state,
        );

        let active_provider = self.selected_provider(config).map(|(n, _)| n);
        let models_list = active_provider
            .as_ref()
            .and_then(|n| self.models.get(n))
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        let [search_area, models_area] = Layout::vertical([Length(1), Min(0)]).areas(right);

        let model_title = format!(
            "Models{}",
            active_provider
                .as_ref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default()
        );
        let model_item_fg = if self.focused == Pane::Models {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        };

        let search_widget = if self.search_active {
            Paragraph::new(format!("/ {}", self.search.query))
                .bg(theme::SURFACE_FOCUSED)
                .fg(theme::ACCENT)
        } else if self.search.is_empty() {
            Paragraph::new("/ to search models").fg(theme::TEXT_MUTED)
        } else {
            Paragraph::new(format!("filter: {}", self.search.query)).fg(theme::TEXT_MUTED)
        };
        frame.render_widget(search_widget, search_area);

        let model_items: Vec<ListItem> = self
            .filtered_models
            .iter()
            .map(|&orig| {
                let m = &models_list[orig];
                let is_active = config.active_model.as_deref() == Some(m.id.as_str());
                let marker = theme::active_marker(is_active);
                ListItem::new(format!("{marker}{}", m.display_name())).fg(model_item_fg)
            })
            .collect();
        let model_block = theme::section_block(&model_title, self.focused == Pane::Models);
        frame.render_stateful_widget(
            List::new(model_items)
                .block(model_block)
                .highlight_style(Style::new().bg(theme::ACCENT_BG).fg(theme::TEXT))
                .highlight_symbol("▶ "),
            models_area,
            &mut self.models_state,
        );

        if let Some(form) = &mut self.add_form {
            view_add_form(frame, area, form);
        }
    }
}

fn view_add_form(frame: &mut Frame<'_>, area: Rect, form: &mut AddProviderForm) {
    let popup = centered_rect(70, 60, area);
    frame.render_widget(Clear, popup);
    let block = theme::overlay_block("Add Provider");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let kind = Provider::ALL[form.kind_idx];
    let needs_key = kind.requires_api_key();
    let default_url = kind.default_base_url().unwrap_or("");

    let label_style = Style::new().fg(theme::TEXT_DIM);
    let value_style = Style::new().fg(theme::TEXT);
    let active_value_style = Style::new().fg(theme::ACCENT).bold();
    let hint_style = Style::new().fg(theme::TEXT_MUTED);

    let make_line = |label: &str, indicator: &str, value: &str, suffix: &str| -> Line {
        let val_style = if indicator.starts_with('[') {
            active_value_style
        } else {
            value_style
        };
        Line::from(vec![
            Span::styled(format!("{label:<10}"), label_style),
            Span::styled(value.to_string(), val_style),
            Span::styled(suffix.to_string(), hint_style),
        ])
    };

    let name_ind = form.field_indicator(FormField::Name, &form.name.value);
    let kind_ind = form.field_indicator(FormField::Kind, kind.display_name());
    let key_ind = form.field_indicator(FormField::ApiKey, &form.api_key.value);
    let url_ind = form.field_indicator(FormField::BaseUrl, &form.base_url.value);

    let url_suffix = if form.base_url.is_empty() && !default_url.is_empty() {
        format!("  (default: {default_url})")
    } else {
        String::new()
    };

    let lines = vec![
        make_line("Name", &name_ind, &form.name.value, ""),
        make_line("Kind", &kind_ind, kind.display_name(), "  (← → to cycle)"),
        make_line(
            "API key",
            &key_ind,
            &form.api_key.value,
            if needs_key { "" } else { "  (not required)" },
        ),
        make_line("Base URL", &url_ind, &form.base_url.value, &url_suffix),
    ];
    let body = Paragraph::new(lines).wrap(Wrap { trim: false });
    let [body_area, error_area, hint_area] =
        Layout::vertical([Min(0), Length(2), Length(1)]).areas(inner);
    frame.render_widget(body, body_area);
    if let Some(e) = &form.error {
        frame.render_widget(Paragraph::new(e.as_str()).fg(theme::ERROR), error_area);
    }
    frame.render_widget(
        Paragraph::new(theme::help_line(&[
            ("Tab", "next"),
            ("Enter", "submit"),
            ("Esc", "cancel"),
        ]))
        .fg(theme::TEXT_MUTED),
        hint_area,
    );
}

impl Default for ModelSelectScreen {
    fn default() -> Self {
        Self::new()
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}
