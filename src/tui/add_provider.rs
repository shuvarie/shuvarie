use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};
use selune::Provider;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::components::InputBuffer;
use super::list::{render_list_item, render_list_item_line, scroll_offset_for};
use super::registry::{FetchState, RegistrySource};
use super::search::{Search, SearchMessage};
use super::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddProviderStage {
    /// Searchable registry list plus the pinned custom-provider row.
    Select,
    /// Transport picker opened from the `kind` field.
    KindList,
    /// The provider configuration form.
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormField {
    Name,
    Kind,
    Catalog,
    BaseUrl,
    ApiKey,
}

#[derive(Debug, PartialEq)]
pub enum AddProviderMessage {
    Next,
    Prev,
    Select,
    ToggleSource,
    OpenCustom,
    RegistryLoaded { providers: Vec<Provider> },
    RegistryError { error: String },
    Search(SearchMessage),
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    KillToEnd,
    NextField,
    PrevField,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Submit,
    Cancel,
    Resize { viewport_height: u16 },
}

#[derive(Debug, PartialEq)]
pub enum AddProviderOutcome {
    None,
    Cancel,
    FetchRegistry,
    Submit {
        kind: String,
        catalog: Option<String>,
        name: String,
        api_key: Option<String>,
        base_url: Option<String>,
    },
}

/// The rig transports a connection can use, in picker order.
const TRANSPORTS: [(selune::ProviderType, &str); 30] = [
    (selune::ProviderType::Openai, "OpenAI"),
    (
        selune::ProviderType::OpenaiCompat,
        "OpenAI-compatible endpoint",
    ),
    (selune::ProviderType::Openrouter, "OpenRouter"),
    (selune::ProviderType::Vercel, "Vercel AI Gateway"),
    (selune::ProviderType::Anthropic, "Anthropic"),
    (selune::ProviderType::Google, "Google Gemini"),
    (selune::ProviderType::Azure, "Azure OpenAI"),
    (selune::ProviderType::Bedrock, "AWS Bedrock"),
    (selune::ProviderType::GoogleVertex, "Google Vertex AI"),
    (selune::ProviderType::Ollama, "Ollama (local)"),
    (selune::ProviderType::Llamafile, "Llamafile (local)"),
    (selune::ProviderType::Chatgpt, "ChatGPT (subscription)"),
    (selune::ProviderType::Copilot, "GitHub Copilot"),
    (selune::ProviderType::Cohere, "Cohere"),
    (selune::ProviderType::Deepseek, "DeepSeek"),
    (selune::ProviderType::Doubleword, "Doubleword"),
    (selune::ProviderType::Groq, "Groq"),
    (selune::ProviderType::Huggingface, "Hugging Face Router"),
    (selune::ProviderType::Hyperbolic, "Hyperbolic"),
    (selune::ProviderType::Minimax, "MiniMax (OpenAI surface)"),
    (selune::ProviderType::Mira, "Mira"),
    (selune::ProviderType::Mistral, "Mistral AI"),
    (selune::ProviderType::Moonshot, "Moonshot AI"),
    (selune::ProviderType::Perplexity, "Perplexity"),
    (selune::ProviderType::Together, "Together AI"),
    (selune::ProviderType::Venice, "Venice AI"),
    (selune::ProviderType::Voyageai, "Voyage AI (embeddings)"),
    (selune::ProviderType::Xai, "xAI"),
    (selune::ProviderType::Xiaomimimo, "Xiaomi MiMo"),
    (selune::ProviderType::Zai, "Z.ai"),
];

pub struct AddProviderForm {
    pub stage: AddProviderStage,
    pub source: RegistrySource,
    providers: Vec<Provider>,
    pub search: Search,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub kind_selected: usize,
    kind_offset: usize,
    kind_viewport_height: u16,
    /// The registry entry the form was opened for; `None` is a custom
    /// provider.
    origin: Option<Provider>,
    pub name: InputBuffer,
    pub kind: InputBuffer,
    pub catalog: InputBuffer,
    pub base_url: InputBuffer,
    pub api_key: InputBuffer,
    pub field: FormField,
    pub error: Option<String>,
    pub existing_names: Vec<String>,
    pub existing_catalog_ids: Vec<String>,
}

impl AddProviderForm {
    pub fn new(
        existing_names: &[String],
        existing_catalog_ids: &[String],
        registry: shuvarie_core::RegistryEntry,
    ) -> Self {
        let source = RegistrySource::new(registry);
        Self::with_providers(
            source.snapshot(),
            source,
            existing_names,
            existing_catalog_ids,
        )
    }

    fn with_providers(
        providers: Vec<Provider>,
        source: RegistrySource,
        existing_names: &[String],
        existing_catalog_ids: &[String],
    ) -> Self {
        let mut form = Self {
            stage: AddProviderStage::Select,
            source,
            providers,
            search: Search::new(),
            filtered: Vec::new(),
            selected: 0,
            offset: 0,
            viewport_height: 0,
            kind_selected: 0,
            kind_offset: 0,
            kind_viewport_height: 0,
            origin: None,
            name: InputBuffer::new(),
            kind: InputBuffer::new(),
            catalog: InputBuffer::new(),
            base_url: InputBuffer::new(),
            api_key: InputBuffer::new(),
            field: FormField::Name,
            error: None,
            existing_names: existing_names.to_vec(),
            existing_catalog_ids: existing_catalog_ids.to_vec(),
        };
        form.refilter();
        form
    }

    /// Whether opening the form should be followed by a registry fetch.
    pub fn needs_fetch(&self) -> bool {
        self.source.needs_fetch()
    }

    /// The selected select-stage row: a registry provider for indices below
    /// `filtered.len()`, the pinned custom row for the last index.
    fn selected_provider(&self) -> Option<&Provider> {
        self.filtered
            .get(self.selected)
            .map(|&i| &self.providers[i])
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.providers.len(), |i| self.providers[i].name.clone());
        self.selected = 0;
        self.offset = 0;
        self.recompute_offset();
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.visible_len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    /// Select-stage row count: the matched providers plus the pinned custom
    /// row.
    fn visible_len(&self) -> usize {
        self.filtered.len() + 1
    }

    fn recompute_kind_offset(&mut self) {
        let vh = self.kind_viewport_height as usize;
        self.kind_offset =
            scroll_offset_for(self.kind_selected, self.kind_offset, vh, TRANSPORTS.len());
    }

    fn compute_default_name(&self, base: &str) -> String {
        if !self.existing_names.contains(&base.to_string()) {
            return base.to_string();
        }
        let mut i = 1;
        loop {
            let candidate = format!("{base} {i}");
            if !self.existing_names.contains(&candidate) {
                return candidate;
            }
            i += 1;
        }
    }

    fn open_details(&mut self, origin: Option<&Provider>) {
        self.origin = origin.cloned();
        match origin {
            Some(provider) => {
                self.name.set(&self.compute_default_name(&provider.name));
                let transport = provider
                    .r#type
                    .map(|t| shuvarie_core::catalog::provider_type_name(t).to_string())
                    .unwrap_or_else(|| provider.id.0.clone());
                self.kind.set(&transport);
                self.catalog.set(&provider.id.0);
                self.base_url.clear();
                if let Some(url) =
                    shuvarie_core::catalog::api_endpoint(provider).filter(|u| !u.contains('$'))
                {
                    self.base_url.set(&url);
                }
                self.api_key.clear();
                self.field = FormField::ApiKey;
            }
            None => {
                self.name.clear();
                self.kind.set("openai-compat");
                self.catalog.clear();
                self.base_url.clear();
                self.api_key.clear();
                self.field = FormField::Name;
            }
        }
        self.stage = AddProviderStage::Details;
        self.error = None;
    }

    fn next_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::Kind,
            FormField::Kind => FormField::Catalog,
            FormField::Catalog => FormField::BaseUrl,
            FormField::BaseUrl => FormField::ApiKey,
            FormField::ApiKey => FormField::Name,
        };
    }

    fn prev_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::ApiKey,
            FormField::Kind => FormField::Name,
            FormField::Catalog => FormField::Kind,
            FormField::BaseUrl => FormField::Catalog,
            FormField::ApiKey => FormField::BaseUrl,
        };
    }

    fn field_buf(&mut self, field: FormField) -> &mut InputBuffer {
        match field {
            FormField::Name => &mut self.name,
            FormField::Kind => &mut self.kind,
            FormField::Catalog => &mut self.catalog,
            FormField::BaseUrl => &mut self.base_url,
            FormField::ApiKey => &mut self.api_key,
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AddProviderMessage> {
        match self.stage {
            AddProviderStage::Select => {
                if ctrl(key) {
                    return match key.code {
                        KeyCode::Char('n') => Some(AddProviderMessage::Next),
                        KeyCode::Char('p') => Some(AddProviderMessage::Prev),
                        KeyCode::Char('o') if self.source.can_toggle() => {
                            Some(AddProviderMessage::ToggleSource)
                        }
                        KeyCode::Char('i') => Some(AddProviderMessage::OpenCustom),
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Escape => Some(AddProviderMessage::Cancel),
                    KeyCode::Down | KeyCode::Char('j') => Some(AddProviderMessage::Next),
                    KeyCode::Up | KeyCode::Char('k') => Some(AddProviderMessage::Prev),
                    KeyCode::Enter => Some(AddProviderMessage::Select),
                    KeyCode::Backspace => {
                        Some(AddProviderMessage::Search(SearchMessage::Backspace))
                    }
                    KeyCode::Char(c) => Some(AddProviderMessage::Search(SearchMessage::Input(c))),
                    _ => None,
                }
            }
            AddProviderStage::KindList => {
                if ctrl(key) {
                    return match key.code {
                        KeyCode::Char('n') => Some(AddProviderMessage::Next),
                        KeyCode::Char('p') => Some(AddProviderMessage::Prev),
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Escape => Some(AddProviderMessage::Cancel),
                    KeyCode::Down | KeyCode::Char('j') => Some(AddProviderMessage::Next),
                    KeyCode::Up | KeyCode::Char('k') => Some(AddProviderMessage::Prev),
                    KeyCode::Enter => Some(AddProviderMessage::Select),
                    _ => None,
                }
            }
            AddProviderStage::Details => {
                if ctrl(key) {
                    return match key.code {
                        KeyCode::Char('b') => Some(AddProviderMessage::Left),
                        KeyCode::Char('f') => Some(AddProviderMessage::Right),
                        KeyCode::Char('a') => Some(AddProviderMessage::Home),
                        KeyCode::Char('e') => Some(AddProviderMessage::End),
                        KeyCode::Char('d') => Some(AddProviderMessage::Delete),
                        KeyCode::Char('h') => Some(AddProviderMessage::Backspace),
                        KeyCode::Char('k') => Some(AddProviderMessage::KillToEnd),
                        _ => None,
                    };
                }
                if alt(key) {
                    return match key.code {
                        KeyCode::Char('b') => Some(AddProviderMessage::LeftWord),
                        KeyCode::Char('f') => Some(AddProviderMessage::RightWord),
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Escape => Some(AddProviderMessage::Cancel),
                    KeyCode::Tab => Some(AddProviderMessage::NextField),
                    KeyCode::BackTab => Some(AddProviderMessage::PrevField),
                    KeyCode::Enter => Some(AddProviderMessage::Submit),
                    KeyCode::Backspace => Some(AddProviderMessage::Backspace),
                    KeyCode::Left => Some(AddProviderMessage::Left),
                    KeyCode::Right => Some(AddProviderMessage::Right),
                    KeyCode::Home => Some(AddProviderMessage::Home),
                    KeyCode::End => Some(AddProviderMessage::End),
                    KeyCode::Char(c) => Some(AddProviderMessage::Input(c)),
                    _ => None,
                }
            }
        }
    }

    pub fn update(&mut self, msg: AddProviderMessage) -> AddProviderOutcome {
        match msg {
            AddProviderMessage::Cancel => match self.stage {
                AddProviderStage::KindList => {
                    self.stage = AddProviderStage::Details;
                    AddProviderOutcome::None
                }
                AddProviderStage::Details => {
                    self.stage = AddProviderStage::Select;
                    self.error = None;
                    AddProviderOutcome::None
                }
                AddProviderStage::Select => AddProviderOutcome::Cancel,
            },
            AddProviderMessage::ToggleSource => {
                if self.source.toggle() {
                    AddProviderOutcome::FetchRegistry
                } else {
                    self.refresh_registry_snapshot();
                    AddProviderOutcome::None
                }
            }
            AddProviderMessage::RegistryLoaded { providers } => {
                self.source.on_loaded();
                if self.source.remote {
                    self.providers = providers;
                    self.refilter();
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::RegistryError { error } => {
                self.source.on_error(error);
                AddProviderOutcome::None
            }
            AddProviderMessage::Next => match self.stage {
                AddProviderStage::KindList => {
                    self.kind_selected = (self.kind_selected + 1).min(TRANSPORTS.len() - 1);
                    self.recompute_kind_offset();
                    AddProviderOutcome::None
                }
                _ => {
                    let len = self.visible_len();
                    if len > 0 {
                        self.selected = (self.selected + 1).min(len - 1);
                        self.recompute_offset();
                    }
                    AddProviderOutcome::None
                }
            },
            AddProviderMessage::Prev => match self.stage {
                AddProviderStage::KindList => {
                    self.kind_selected = self.kind_selected.saturating_sub(1);
                    self.recompute_kind_offset();
                    AddProviderOutcome::None
                }
                _ => {
                    self.selected = self.selected.saturating_sub(1);
                    self.recompute_offset();
                    AddProviderOutcome::None
                }
            },
            AddProviderMessage::Select => match self.stage {
                AddProviderStage::KindList => {
                    let (ptype, _) = TRANSPORTS[self.kind_selected];
                    self.kind
                        .set(shuvarie_core::catalog::provider_type_name(ptype));
                    self.stage = AddProviderStage::Details;
                    AddProviderOutcome::None
                }
                _ => {
                    let origin = self.selected_provider().cloned();
                    self.open_details(origin.as_ref());
                    AddProviderOutcome::None
                }
            },
            AddProviderMessage::OpenCustom => {
                self.open_details(None);
                AddProviderOutcome::None
            }
            AddProviderMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
                AddProviderOutcome::None
            }
            AddProviderMessage::NextField => {
                self.next_field();
                AddProviderOutcome::None
            }
            AddProviderMessage::PrevField => {
                self.prev_field();
                AddProviderOutcome::None
            }
            AddProviderMessage::Input(c) => {
                self.field_buf(self.field).push(c);
                AddProviderOutcome::None
            }
            AddProviderMessage::Paste(text) => {
                let flat = super::components::flatten_newlines(&text);
                match self.stage {
                    AddProviderStage::Select => {
                        for c in flat.chars() {
                            self.search.update(SearchMessage::Input(c));
                        }
                        self.refilter();
                    }
                    _ => {
                        self.field_buf(self.field).insert_str(&flat);
                    }
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Backspace => {
                self.field_buf(self.field).backspace();
                AddProviderOutcome::None
            }
            AddProviderMessage::Delete => {
                self.field_buf(self.field).delete();
                AddProviderOutcome::None
            }
            AddProviderMessage::KillToEnd => {
                self.field_buf(self.field).kill_to_end();
                AddProviderOutcome::None
            }
            AddProviderMessage::Left => {
                self.field_buf(self.field).left();
                AddProviderOutcome::None
            }
            AddProviderMessage::Right => {
                self.field_buf(self.field).right();
                AddProviderOutcome::None
            }
            AddProviderMessage::LeftWord => {
                self.field_buf(self.field).left_word();
                AddProviderOutcome::None
            }
            AddProviderMessage::RightWord => {
                self.field_buf(self.field).right_word();
                AddProviderOutcome::None
            }
            AddProviderMessage::Home => {
                self.field_buf(self.field).home();
                AddProviderOutcome::None
            }
            AddProviderMessage::End => {
                self.field_buf(self.field).end();
                AddProviderOutcome::None
            }
            AddProviderMessage::Submit => {
                if self.stage == AddProviderStage::Details && self.field == FormField::Kind {
                    self.kind_selected = 0;
                    self.kind_offset = 0;
                    self.stage = AddProviderStage::KindList;
                    return AddProviderOutcome::None;
                }
                self.submit()
            }
            AddProviderMessage::Resize { viewport_height } => match self.stage {
                AddProviderStage::KindList => {
                    if self.kind_viewport_height != viewport_height {
                        self.kind_viewport_height = viewport_height;
                        self.recompute_kind_offset();
                    }
                    AddProviderOutcome::None
                }
                AddProviderStage::Select => {
                    if self.viewport_height != viewport_height {
                        self.viewport_height = viewport_height;
                        self.recompute_offset();
                    }
                    AddProviderOutcome::None
                }
                AddProviderStage::Details => AddProviderOutcome::None,
            },
        }
    }

    fn refresh_registry_snapshot(&mut self) {
        self.providers = self.source.snapshot();
        self.refilter();
    }

    fn submit(&mut self) -> AddProviderOutcome {
        let name = self.name.value.trim().to_string();
        if name.is_empty() {
            self.error = Some("Name is required".into());
            return AddProviderOutcome::None;
        }
        if self.existing_names.contains(&name) {
            self.error = Some(format!("Provider '{name}' already exists"));
            return AddProviderOutcome::None;
        }
        let kind = self.kind.value.trim().to_string();
        if shuvarie_core::catalog::parse_provider_type(&kind).is_none() {
            self.error = Some(format!(
                "Unknown provider type '{kind}' — press Enter to pick one"
            ));
            return AddProviderOutcome::None;
        }
        let api_key = if self.key_required() {
            let k = self.api_key.value.trim().to_string();
            if k.is_empty() {
                self.error = Some("This provider requires an API key".into());
                return AddProviderOutcome::None;
            }
            Some(k)
        } else {
            None
        };
        let base_url = if self.base_url.value.trim().is_empty() {
            None
        } else {
            Some(self.base_url.value.trim().to_string())
        };
        let catalog = self.catalog.value.trim().to_string();
        let catalog = (!catalog.is_empty()).then_some(catalog);
        AddProviderOutcome::Submit {
            kind,
            catalog,
            name,
            api_key,
            base_url,
        }
    }

    /// Whether the provider being configured demands a non-empty API key: the
    /// catalog entry's requirement for a registry origin, else the typed
    /// catalog id's entry when it resolves, else the transport (everything
    /// but the local runtimes and the OAuth-backed subscriptions).
    fn key_required(&self) -> bool {
        match &self.origin {
            Some(provider) => shuvarie_core::catalog::requires_api_key(provider),
            None => {
                let kind = self.kind.value.trim();
                let catalog = self.catalog.value.trim();
                if !catalog.is_empty() {
                    let providers = shuvarie_core::catalog::providers();
                    if let Some(entry) = shuvarie_core::catalog::find_provider(&providers, catalog)
                    {
                        return shuvarie_core::catalog::requires_api_key(entry);
                    }
                }
                !matches!(
                    shuvarie_core::catalog::parse_provider_type(kind),
                    Some(
                        selune::ProviderType::Ollama
                            | selune::ProviderType::Llamafile
                            | selune::ProviderType::Chatgpt
                            | selune::ProviderType::Copilot
                    )
                )
            }
        }
    }

    fn field_line(
        &self,
        label: &str,
        buffer: &InputBuffer,
        field: FormField,
        suffix: &str,
    ) -> Line<'static> {
        let label_style = Style::new().fg(theme::text_dim());
        let hint_style = Style::new().fg(theme::text_muted());
        let active_style = Style::new().fg(theme::accent());
        let inactive_style = Style::new().fg(theme::text_dim());
        let cursor_style = Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::REVERSED);

        let is_active = self.field == field;

        let mut spans = vec![Span::styled(format!("{label:<10}"), label_style)];

        if is_active {
            let chars: Vec<char> = buffer.value.chars().collect();
            let cursor_idx = buffer.cursor_char_index();
            for (i, c) in chars.iter().enumerate() {
                let style = if i == cursor_idx {
                    cursor_style
                } else {
                    active_style
                };
                spans.push(Span::styled(c.to_string(), style));
            }
            if cursor_idx >= chars.len() {
                spans.push(Span::styled(" ".to_string(), cursor_style));
            }
        } else {
            spans.push(Span::styled(buffer.value.clone(), inactive_style));
        }

        if !suffix.is_empty() {
            spans.push(Span::styled(suffix.to_string(), hint_style));
        }

        Line::from(spans)
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        match self.stage {
            AddProviderStage::Select => self.view_select(frame, area),
            AddProviderStage::KindList => self.view_kind_list(frame, area),
            AddProviderStage::Details => self.view_details(frame, area),
        }
    }

    fn source_line(&self) -> Line<'static> {
        let mut spans = vec![
            Span::raw("Registry: ").fg(theme::text_dim()),
            Span::raw(self.source.label()).fg(theme::accent()),
        ];
        match &self.source.state {
            FetchState::Fetching => spans.push(Span::raw(" — fetching…").fg(theme::text_muted())),
            FetchState::Failed(error) => {
                spans.push(Span::raw(" — ").fg(theme::text_muted()));
                spans.push(Span::raw(error.clone()).fg(theme::error()));
            }
            FetchState::Idle => {}
        }
        Line::from(spans)
    }

    fn view_select(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(50, 55, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Add Provider");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [source_area, search_area, list_area, hint_area] =
            Layout::vertical([Length(1), Length(1), Min(0), Length(1)]).areas(inner);

        frame.render_widget(self.source_line(), source_area);
        self.search
            .view(frame, search_area, "Type to filter providers");

        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            list_area.height as usize,
            self.visible_len(),
        );
        let visible_len = list_area.height as usize;
        let mut items: Vec<ListItem> = Vec::new();
        for row in offset..self.visible_len().min(offset + visible_len) {
            if let Some(&orig) = self.filtered.get(row) {
                let provider = &self.providers[orig];
                let mut line = vec![Span::raw(provider.name.clone()).fg(theme::text())];
                if self.existing_catalog_ids.contains(&provider.id.0) {
                    line.push(Span::raw("  ✓ configured").fg(theme::text_muted()));
                }
                items.push(render_list_item_line(
                    Line::from(line),
                    row == self.selected,
                ));
            } else {
                items.push(render_list_item(
                    "Add custom provider…".to_string(),
                    row == self.selected,
                ));
            }
        }
        frame.render_widget(List::new(items), list_area);

        let mut hints = vec![
            ("Type", "to filter"),
            ("↑↓", "navigate"),
            ("Enter", "continue"),
        ];
        if self.source.can_toggle() {
            hints.push(("Ctrl+O", "online registry"));
        }
        hints.push(("Ctrl+I", "custom"));
        hints.push(("Esc", "cancel"));
        frame.render_widget(
            Paragraph::new(theme::help_line(&hints)).fg(theme::text_muted()),
            hint_area,
        );
    }

    fn view_kind_list(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(50, 55, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Provider Type");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);

        let offset = scroll_offset_for(
            self.kind_selected,
            self.kind_offset,
            list_area.height as usize,
            TRANSPORTS.len(),
        );
        let visible_len = list_area.height as usize;
        let items: Vec<ListItem> = (offset..TRANSPORTS.len().min(offset + visible_len))
            .map(|i| {
                let (ptype, description) = TRANSPORTS[i];
                let line = Line::from(vec![
                    Span::raw(shuvarie_core::catalog::provider_type_name(ptype).to_string())
                        .fg(theme::text()),
                    Span::raw(format!("  — {description}")).fg(theme::text_muted()),
                ]);
                render_list_item_line(line, i == self.kind_selected)
            })
            .collect();
        frame.render_widget(List::new(items), list_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("↑↓", "navigate"),
                ("Enter", "pick"),
                ("Esc", "back"),
            ]))
            .fg(theme::text_muted()),
            hint_area,
        );
    }

    fn view_details(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(60, 55, area);
        frame.render_widget(Clear, popup);
        let title = match &self.origin {
            Some(provider) => format!("Configure Provider — {}", provider.name),
            None => "Add Custom Provider".to_string(),
        };
        let block = theme::overlay_block(&title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let name_line = self.field_line("Name", &self.name, FormField::Name, "");
        let kind_line = self.field_line("Kind", &self.kind, FormField::Kind, "  (Enter to pick)");
        let catalog_suffix = if self.origin.is_none() {
            "  (optional Selune id)"
        } else {
            ""
        };
        let catalog_line =
            self.field_line("Catalog", &self.catalog, FormField::Catalog, catalog_suffix);
        let url_line = self.field_line("Base URL", &self.base_url, FormField::BaseUrl, "");
        let key_suffix = if self.key_required() {
            ""
        } else {
            "  (not required)"
        };
        let key_line = self.field_line("API key", &self.api_key, FormField::ApiKey, key_suffix);

        let lines = vec![name_line, kind_line, catalog_line, url_line, key_line];
        let body = Paragraph::new(lines).wrap(Wrap { trim: false });
        let [body_area, error_area, hint_area] =
            Layout::vertical([Min(0), Length(2), Length(1)]).areas(inner);
        frame.render_widget(body, body_area);
        if let Some(e) = &self.error {
            frame.render_widget(Paragraph::new(e.as_str()).fg(theme::error()), error_area);
        }
        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Tab", "next"),
                ("Enter", "submit"),
                ("Esc", "back"),
            ]))
            .fg(theme::text_muted()),
            hint_area,
        );
    }
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{InferenceProvider, Model as SeluneModel, ModelLimit};
    use termina::event::Modifiers;

    fn catalog_provider(id: &str, name: &str, ptype: selune::ProviderType) -> Provider {
        Provider {
            name: name.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: Some("$KEY".into()),
            api_endpoint: Some("https://api.example.com/v1".into()),
            doc: None,
            r#type: Some(ptype),
            default_large_model_id: None,
            default_small_model_id: None,
            models: vec![SeluneModel {
                id: "test-model".into(),
                name: "Test Model".into(),
                reasoning: false,
                reasoning_options: Vec::new(),
                attachment: false,
                limit: ModelLimit::default(),
                cost: Default::default(),
                options: None,
            }],
            default_headers: None,
        }
    }

    fn providers() -> Vec<Provider> {
        vec![
            catalog_provider("openai", "OpenAI", selune::ProviderType::Openai),
            catalog_provider("ollama", "Ollama", selune::ProviderType::Ollama),
        ]
    }

    fn form() -> AddProviderForm {
        AddProviderForm::with_providers(
            providers(),
            RegistrySource::new(shuvarie_core::RegistryEntry::default()),
            &["Existing".to_string()],
            &[],
        )
    }

    fn custom_form() -> AddProviderForm {
        let mut form = form();
        form.open_details(None);
        form
    }

    #[test]
    fn transports_cover_every_provider_type_and_parse() {
        let seen: std::collections::HashSet<selune::ProviderType> =
            TRANSPORTS.iter().map(|(t, _)| *t).collect();
        for (ptype, _) in TRANSPORTS {
            let name = shuvarie_core::catalog::provider_type_name(ptype);
            assert_eq!(
                shuvarie_core::catalog::parse_provider_type(name),
                Some(ptype),
                "{name} should parse"
            );
        }
        // Every variant must be pickable in the form.
        let all = [
            selune::ProviderType::Openai,
            selune::ProviderType::OpenaiCompat,
            selune::ProviderType::Openrouter,
            selune::ProviderType::Vercel,
            selune::ProviderType::Anthropic,
            selune::ProviderType::Google,
            selune::ProviderType::Azure,
            selune::ProviderType::Bedrock,
            selune::ProviderType::GoogleVertex,
            selune::ProviderType::Ollama,
            selune::ProviderType::Chatgpt,
            selune::ProviderType::Copilot,
            selune::ProviderType::Cohere,
            selune::ProviderType::Deepseek,
            selune::ProviderType::Doubleword,
            selune::ProviderType::Groq,
            selune::ProviderType::Huggingface,
            selune::ProviderType::Hyperbolic,
            selune::ProviderType::Llamafile,
            selune::ProviderType::Minimax,
            selune::ProviderType::Mira,
            selune::ProviderType::Mistral,
            selune::ProviderType::Moonshot,
            selune::ProviderType::Perplexity,
            selune::ProviderType::Together,
            selune::ProviderType::Venice,
            selune::ProviderType::Voyageai,
            selune::ProviderType::Xai,
            selune::ProviderType::Xiaomimimo,
            selune::ProviderType::Zai,
        ];
        for ptype in all {
            assert!(seen.contains(&ptype), "{ptype:?} missing from TRANSPORTS");
        }
        assert_eq!(seen.len(), TRANSPORTS.len(), "duplicates in TRANSPORTS");
    }

    #[test]
    fn empty_query_lists_all_providers() {
        let form = form();
        assert_eq!(form.filtered, vec![0, 1]);
        assert_eq!(form.visible_len(), 3, "providers plus the custom row");
    }

    #[test]
    fn query_filters_providers_and_custom_row_stays_last() {
        let mut form = form();
        form.update(AddProviderMessage::Search(SearchMessage::Input('p')));
        assert_eq!(form.filtered.len(), 1, "only OpenAI fuzzy-matches 'p'");
        assert_eq!(form.visible_len(), 2);
        assert_eq!(form.selected, 0);
        form.update(AddProviderMessage::Next);
        assert_eq!(
            form.selected,
            form.filtered.len(),
            "second row after refilter"
        );
    }

    #[test]
    fn selecting_provider_prefills_and_focuses_api_key() {
        let mut form = form();
        form.update(AddProviderMessage::Search(SearchMessage::Input('o')));
        form.update(AddProviderMessage::Select);
        assert_eq!(form.stage, AddProviderStage::Details);
        assert_eq!(form.field, FormField::ApiKey, "registry origin autofocus");
        assert_eq!(form.catalog.value, "openai");
        assert_eq!(form.kind.value, "openai");
        assert_eq!(form.base_url.value, "https://api.example.com/v1");
        assert!(form.name.value.starts_with("OpenAI"));
    }

    #[test]
    fn custom_row_focuses_name_with_empty_fields() {
        let mut form = form();
        form.selected = form.filtered.len();
        form.update(AddProviderMessage::Select);
        assert_eq!(form.stage, AddProviderStage::Details);
        assert_eq!(form.field, FormField::Name, "custom origin starts on Name");
        assert!(form.name.value.is_empty());
        assert_eq!(form.kind.value, "openai-compat");
        assert!(form.catalog.value.is_empty());
        assert!(form.origin.is_none());
    }

    #[test]
    fn ctrl_i_opens_custom_directly() {
        let mut form = form();
        assert!(matches!(
            form.map_event(&key(KeyCode::Char('i'), Modifiers::CONTROL)),
            Some(AddProviderMessage::OpenCustom)
        ));
        form.update(AddProviderMessage::OpenCustom);
        assert_eq!(form.stage, AddProviderStage::Details);
        assert_eq!(form.field, FormField::Name);
    }

    #[test]
    fn ctrl_o_is_hidden_when_registry_disabled() {
        let form = AddProviderForm::with_providers(
            vec![],
            RegistrySource::new(shuvarie_core::RegistryEntry {
                disabled: true,
                remote_first: false,
            }),
            &[],
            &[],
        );
        assert_eq!(form.filtered.len(), 0, "disabled registry lists nothing");
        assert_eq!(form.visible_len(), 1, "only the custom row");
        assert!(!form.source.can_toggle());
        assert_eq!(
            form.map_event(&key(KeyCode::Char('o'), Modifiers::CONTROL)),
            None,
            "no toggle mapping when disabled"
        );
    }

    #[test]
    fn enter_on_kind_field_opens_transport_list() {
        let mut form = custom_form();
        form.field = FormField::Kind;
        form.update(AddProviderMessage::Submit);
        assert_eq!(form.stage, AddProviderStage::KindList);
        form.kind_selected = 4;
        form.update(AddProviderMessage::Select);
        assert_eq!(form.stage, AddProviderStage::Details);
        assert_eq!(form.kind.value, "anthropic");
    }

    #[test]
    fn escape_walks_back_through_stages() {
        let mut form = custom_form();
        form.field = FormField::Kind;
        form.update(AddProviderMessage::Submit);
        assert_eq!(form.stage, AddProviderStage::KindList);
        form.update(AddProviderMessage::Cancel);
        assert_eq!(form.stage, AddProviderStage::Details);
        form.update(AddProviderMessage::Cancel);
        assert_eq!(form.stage, AddProviderStage::Select);
        assert_eq!(
            form.update(AddProviderMessage::Cancel),
            AddProviderOutcome::Cancel
        );
    }

    #[test]
    fn submit_custom_requires_parseable_kind_and_key() {
        let mut form = custom_form();
        form.name.set("Acme");
        form.kind.set("not-a-kind");
        assert!(matches!(
            form.update(AddProviderMessage::Submit),
            AddProviderOutcome::None
        ));
        assert!(
            form.error
                .as_deref()
                .unwrap()
                .contains("Unknown provider type")
        );

        form.kind.set("openai-compat");
        form.update(AddProviderMessage::Submit);
        assert!(
            form.error.as_deref().unwrap().contains("API key"),
            "openai-compat without a catalog id needs a key"
        );

        form.kind.set("ollama");
        form.api_key.clear();
        assert!(
            matches!(
                form.update(AddProviderMessage::Submit),
                AddProviderOutcome::Submit { .. }
            ),
            "ollama needs no key"
        );
    }

    #[test]
    fn submit_custom_catalog_entry_governs_key_requirement() {
        let mut form = custom_form();
        form.name.set("Acme");
        form.kind.set("openai-compat");
        form.catalog.set("bedrock");
        assert!(!form.key_required(), "catalog entry without api_key");

        form.catalog.set("openai");
        assert!(form.key_required(), "catalog entry with api_key");
    }

    #[test]
    fn submit_registry_origin_requires_catalog_key() {
        let mut form = form();
        form.open_details(Some(&catalog_provider(
            "openai",
            "OpenAI",
            selune::ProviderType::Openai,
        )));
        form.update(AddProviderMessage::Submit);
        assert!(form.error.as_deref().unwrap().contains("API key"));

        form.api_key.set("sk-test");
        let outcome = form.update(AddProviderMessage::Submit);
        assert!(matches!(
            outcome,
            AddProviderOutcome::Submit {
                kind,
                catalog: Some(catalog),
                ..
            } if kind == "openai" && catalog == "openai"
        ));
    }

    #[test]
    fn duplicate_name_is_rejected() {
        let mut form = custom_form();
        form.name.set("Existing");
        form.update(AddProviderMessage::Submit);
        assert!(form.error.as_deref().unwrap().contains("already exists"));
    }

    #[test]
    fn registry_loaded_refreshes_remote_snapshot() {
        let mut form = form();
        form.source.remote = true;
        let remote = vec![catalog_provider(
            "remote-only",
            "RemoteOnly",
            selune::ProviderType::Openai,
        )];
        form.update(AddProviderMessage::RegistryLoaded { providers: remote });
        assert_eq!(form.source.state, FetchState::Idle);
        assert_eq!(
            form.providers.len(),
            1,
            "snapshot shows the remote registry"
        );
        assert_eq!(form.filtered, vec![0]);
    }

    #[test]
    fn registry_loaded_is_ignored_while_offline() {
        let mut form = form();
        form.update(AddProviderMessage::RegistryLoaded { providers: vec![] });
        assert_eq!(form.source.state, FetchState::Idle);
        assert_eq!(form.providers.len(), 2, "offline snapshot untouched");
    }

    #[test]
    fn registry_error_is_surfaced_and_keeps_list() {
        let mut form = form();
        form.update(AddProviderMessage::RegistryError {
            error: "offline".into(),
        });
        assert_eq!(form.source.state, FetchState::Failed("offline".into()));
        assert_eq!(form.providers.len(), 2, "list keeps the offline snapshot");
    }

    fn key(code: KeyCode, mods: Modifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
}
