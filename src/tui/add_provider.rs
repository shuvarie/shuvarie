use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{List, ListItem, Paragraph, Wrap};
use selune::Provider;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::components::InputBuffer;
use super::list::{render_list_item, render_list_item_line, scroll_offset_for};
use super::registry::RegistryManager;
use super::search::{Search, SearchMessage, filter_indices};
use super::{popup, theme};

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

/// Where the API key comes from: typed (and written to `connections.kdl`) or
/// read from an environment variable at connect time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Inline,
    Env,
}

impl KeySource {
    fn flipped(self) -> Self {
        match self {
            KeySource::Inline => KeySource::Env,
            KeySource::Env => KeySource::Inline,
        }
    }
}

/// Whether `name` could be an environment variable name: a leading letter or
/// underscore, then letters, digits and underscores. Catches the common
/// mistake of pasting the key itself into the env field.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether `name` holds a non-empty value in this process's environment.
fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

/// One row of the details form: a label, optional dimmed text before and after
/// the value, and the ghost text an empty value shows in its place.
struct FieldRow<'a> {
    label: &'a str,
    /// Dimmed marker before the value, e.g. `[env] `.
    prefix: &'a str,
    /// Dimmed text shown while the value is empty.
    placeholder: &'a str,
    buffer: &'a InputBuffer,
    field: FormField,
    /// Dimmed note after the value, e.g. `(Enter to pick)`.
    suffix: &'a str,
}

impl<'a> FieldRow<'a> {
    fn new(label: &'a str, buffer: &'a InputBuffer, field: FormField) -> Self {
        Self {
            label,
            prefix: "",
            placeholder: "",
            buffer,
            field,
            suffix: "",
        }
    }

    fn prefix(mut self, prefix: &'a str) -> Self {
        self.prefix = prefix;
        self
    }

    fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    fn suffix(mut self, suffix: &'a str) -> Self {
        self.suffix = suffix;
        self
    }
}

#[derive(Debug, PartialEq)]
pub enum AddProviderMessage {
    Next,
    Prev,
    Select,
    ToggleSource,
    OpenCustom,
    RegistryLoaded {
        registry: String,
        providers: Vec<Provider>,
    },
    RegistryError {
        registry: String,
        error: String,
    },
    Search(SearchMessage),
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    KillToEnd,
    NextField,
    PrevField,
    ToggleKeySource,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    Submit,
    Cancel,
    Resize {
        viewport_height: u16,
    },
}

#[derive(Debug, PartialEq)]
pub enum AddProviderOutcome {
    None,
    Cancel,
    FetchRegistries(Vec<String>),
    Submit {
        kind: String,
        catalog: Option<String>,
        name: String,
        api_key: Option<String>,
        api_key_env: Option<String>,
        base_url: Option<String>,
    },
}

/// A transport kind: the protocol type plus its optional vendor dialect.
const fn k(
    r#type: selune::ProviderType,
    dialect: Option<selune::Dialect>,
) -> shuvarie_llm::ProviderKind {
    shuvarie_llm::ProviderKind::new(r#type, dialect)
}

/// A vendor dialect of the openai-compat protocol.
const fn v(dialect: selune::Dialect) -> shuvarie_llm::ProviderKind {
    k(selune::ProviderType::OpenaiCompat, Some(dialect))
}

/// The rig transports a connection can use, in picker order.
const TRANSPORTS: [(shuvarie_llm::ProviderKind, &str); 30] = [
    (k(selune::ProviderType::Openai, None), "OpenAI"),
    (
        k(selune::ProviderType::OpenaiCompat, None),
        "OpenAI-compatible endpoint",
    ),
    (k(selune::ProviderType::Openrouter, None), "OpenRouter"),
    (k(selune::ProviderType::Vercel, None), "Vercel AI Gateway"),
    (k(selune::ProviderType::Anthropic, None), "Anthropic"),
    (k(selune::ProviderType::Google, None), "Google Gemini"),
    (k(selune::ProviderType::Azure, None), "Azure OpenAI"),
    (k(selune::ProviderType::Bedrock, None), "AWS Bedrock"),
    (
        k(selune::ProviderType::GoogleVertex, None),
        "Google Vertex AI",
    ),
    (k(selune::ProviderType::Ollama, None), "Ollama (local)"),
    (
        k(selune::ProviderType::Llamafile, None),
        "Llamafile (local)",
    ),
    (
        k(selune::ProviderType::Chatgpt, None),
        "ChatGPT (subscription)",
    ),
    (k(selune::ProviderType::Copilot, None), "GitHub Copilot"),
    (k(selune::ProviderType::Cohere, None), "Cohere"),
    (v(selune::Dialect::Deepseek), "DeepSeek"),
    (v(selune::Dialect::Doubleword), "Doubleword"),
    (v(selune::Dialect::Groq), "Groq"),
    (v(selune::Dialect::Huggingface), "Hugging Face Router"),
    (v(selune::Dialect::Hyperbolic), "Hyperbolic"),
    (v(selune::Dialect::Minimax), "MiniMax (OpenAI surface)"),
    (v(selune::Dialect::Mira), "Mira"),
    (v(selune::Dialect::Mistral), "Mistral AI"),
    (v(selune::Dialect::Moonshot), "Moonshot AI"),
    (v(selune::Dialect::Perplexity), "Perplexity"),
    (v(selune::Dialect::Together), "Together AI"),
    (v(selune::Dialect::Venice), "Venice AI"),
    (
        k(selune::ProviderType::Voyageai, None),
        "Voyage AI (embeddings)",
    ),
    (v(selune::Dialect::Xai), "xAI"),
    (v(selune::Dialect::Xiaomimimo), "Xiaomi MiMo"),
    (v(selune::Dialect::Zai), "Z.ai"),
];

/// One select-stage row: a registry's group header (rendered live from the
/// manager so badges track fetch state), one of its providers, or the pinned
/// custom-provider row that always ends the list.
#[derive(Clone, Debug, PartialEq, Eq)]
enum SelectRow {
    Header { registry: String },
    Provider { registry: String, index: usize },
    Custom,
}

impl SelectRow {
    fn selectable(&self) -> bool {
        !matches!(self, SelectRow::Header { .. })
    }
}

pub struct AddProviderForm {
    pub stage: AddProviderStage,
    pub registries: RegistryManager,
    rows: Vec<SelectRow>,
    pub search: Search,
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
    /// The literal key, used when [`Self::key_source`] is
    /// [`KeySource::Inline`].
    pub api_key: InputBuffer,
    /// The environment variable name, used when [`Self::key_source`] is
    /// [`KeySource::Env`].
    pub api_key_env: InputBuffer,
    pub key_source: KeySource,
    pub field: FormField,
    pub error: Option<String>,
    pub existing_names: Vec<String>,
    pub existing_catalog_ids: Vec<String>,
}

impl AddProviderForm {
    pub fn new(
        existing_names: &[String],
        existing_catalog_ids: &[String],
        registries: &shuvarie_core::RegistriesConfig,
    ) -> Self {
        Self::with_registries(
            RegistryManager::from_config(registries),
            existing_names,
            existing_catalog_ids,
        )
    }

    fn with_registries(
        registries: RegistryManager,
        existing_names: &[String],
        existing_catalog_ids: &[String],
    ) -> Self {
        let mut form = Self {
            stage: AddProviderStage::Select,
            registries,
            rows: Vec::new(),
            search: Search::new(),
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
            api_key_env: InputBuffer::new(),
            key_source: KeySource::Inline,
            field: FormField::Name,
            error: None,
            existing_names: existing_names.to_vec(),
            existing_catalog_ids: existing_catalog_ids.to_vec(),
        };
        form.rebuild_rows();
        form
    }

    /// The registry ids to fetch when the form opens.
    pub fn ids_needing_fetch(&self) -> Vec<String> {
        self.registries.ids_needing_fetch()
    }

    /// The selected select-stage row's provider, `None` for the pinned custom
    /// row.
    fn selected_provider(&self) -> Option<&Provider> {
        match self.rows.get(self.selected)? {
            SelectRow::Provider { registry, index } => {
                self.registries.state(registry)?.providers().get(*index)
            }
            _ => None,
        }
    }

    /// Rebuilds the grouped rows: one header per registry (kept even for an
    /// idle empty group so its badge shows), its providers in filtered order,
    /// and the pinned custom row last. While searching, groups with no match
    /// disappear — headers included.
    fn rebuild_rows(&mut self) {
        let query = self.search.query.trim().to_string();
        let filtering = !query.is_empty();
        let mut rows: Vec<SelectRow> = Vec::new();
        for registry in self.registries.registries() {
            let providers = registry.providers();
            let matched: Vec<usize> = if filtering {
                filter_indices(&query, providers.len(), |i| providers[i].name.clone())
            } else {
                (0..providers.len()).collect()
            };
            if filtering && matched.is_empty() {
                continue;
            }
            rows.push(SelectRow::Header {
                registry: registry.id().to_string(),
            });
            rows.extend(matched.into_iter().map(|index| SelectRow::Provider {
                registry: registry.id().to_string(),
                index,
            }));
        }
        rows.push(SelectRow::Custom);
        self.rows = rows;
        self.selected = self.first_selectable();
        self.offset = 0;
        self.recompute_offset();
    }

    fn first_selectable(&self) -> usize {
        self.rows
            .iter()
            .position(SelectRow::selectable)
            .unwrap_or(self.rows.len().saturating_sub(1))
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        self.offset = scroll_offset_for(self.selected, self.offset, vh, self.rows.len());
    }

    /// Steps the selection by `dir`, skipping group headers; the movement is
    /// a no-op when nothing selectable lies in that direction.
    fn move_selection(&mut self, dir: isize) {
        let len = self.rows.len() as isize;
        if len == 0 {
            return;
        }
        let mut next = self.selected as isize + dir;
        while next >= 0 && next < len && !self.rows[next as usize].selectable() {
            next += dir;
        }
        if next < 0 {
            return;
        }
        self.selected = (next as usize).min(len as usize - 1);
        self.recompute_offset();
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
                    .map(|t| {
                        shuvarie_core::catalog::provider_kind_name(shuvarie_llm::ProviderKind::new(
                            t,
                            provider.dialect,
                        ))
                    })
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
                // Selune spells the key it wants as `$OPENAI_API_KEY`, which is
                // exactly the variable this pact can read instead of storing a
                // secret. It only becomes the source when it is set: a source
                // that cannot resolve helps nobody.
                self.api_key_env.clear();
                self.key_source = match provider
                    .api_key
                    .as_deref()
                    .and_then(shuvarie_core::normalize_env_name)
                {
                    Some(env) if env_is_set(&env) => {
                        self.api_key_env.set(&env);
                        KeySource::Env
                    }
                    _ => KeySource::Inline,
                };
                self.field = if provider.oauth_device_login() {
                    FormField::Name
                } else {
                    FormField::ApiKey
                };
            }
            None => {
                self.name.clear();
                self.kind.set("openai-compat");
                self.catalog.clear();
                self.base_url.clear();
                self.api_key.clear();
                self.api_key_env.clear();
                self.key_source = KeySource::Inline;
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
            FormField::BaseUrl => match self.oauth_login() {
                true => FormField::Name,
                false => FormField::ApiKey,
            },
            FormField::ApiKey => FormField::Name,
        };
    }

    fn prev_field(&mut self) {
        self.field = match self.field {
            FormField::Name => match self.oauth_login() {
                true => FormField::BaseUrl,
                false => FormField::ApiKey,
            },
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
            FormField::ApiKey => self.key_buf(),
        }
    }

    /// The buffer the API key field edits: the literal key, or the name of the
    /// environment variable holding it.
    fn key_buf(&mut self) -> &mut InputBuffer {
        match self.key_source {
            KeySource::Inline => &mut self.api_key,
            KeySource::Env => &mut self.api_key_env,
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AddProviderMessage> {
        match self.stage {
            AddProviderStage::Select => {
                if ctrl(key) {
                    return match key.code {
                        KeyCode::Char('n') => Some(AddProviderMessage::Next),
                        KeyCode::Char('p') => Some(AddProviderMessage::Prev),
                        KeyCode::Char('o') if self.registries.can_toggle() => {
                            Some(AddProviderMessage::ToggleSource)
                        }
                        KeyCode::Char('i') => Some(AddProviderMessage::OpenCustom),
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Escape => Some(AddProviderMessage::Cancel),
                    KeyCode::Down => Some(AddProviderMessage::Next),
                    KeyCode::Up => Some(AddProviderMessage::Prev),
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
                        KeyCode::Char('e') if self.env_option_available() => {
                            Some(AddProviderMessage::ToggleKeySource)
                        }
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
                let ids = self.registries.initiate_online();
                // Going online may have switched sources (an already-loaded
                // snapshot appears, a pending fetch empties one); the rows
                // re-balance either way.
                self.rebuild_rows();
                if ids.is_empty() {
                    AddProviderOutcome::None
                } else {
                    AddProviderOutcome::FetchRegistries(ids)
                }
            }
            AddProviderMessage::RegistryLoaded {
                registry,
                providers,
            } => {
                // Only registries this popup tracks resolve; other registries'
                // fetches (e.g. `remote-first` ones at startup) refresh
                // nothing here.
                if self.registries.on_loaded(&registry, providers) {
                    self.rebuild_rows();
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::RegistryError { registry, error } => {
                self.registries.on_error(&registry, error);
                AddProviderOutcome::None
            }
            AddProviderMessage::Next => match self.stage {
                AddProviderStage::KindList => {
                    self.kind_selected = (self.kind_selected + 1).min(TRANSPORTS.len() - 1);
                    self.recompute_kind_offset();
                    AddProviderOutcome::None
                }
                _ => {
                    self.move_selection(1);
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
                    self.move_selection(-1);
                    AddProviderOutcome::None
                }
            },
            AddProviderMessage::Select => match self.stage {
                AddProviderStage::KindList => {
                    let (transport, _) = TRANSPORTS[self.kind_selected];
                    self.kind
                        .set(&shuvarie_core::catalog::provider_kind_name(transport));
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
                self.rebuild_rows();
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
            AddProviderMessage::ToggleKeySource => {
                // Only an environment variable that is actually set can become
                // the key source; otherwise the option is not offered at all.
                if !self.env_option_available() {
                    return AddProviderOutcome::None;
                }
                self.key_source = self.key_source.flipped();
                // A freshly opened env field is seeded from Selune, whose
                // `api_key` is the `$NAME` reference the provider expects.
                if self.key_source == KeySource::Env
                    && self.api_key_env.value.trim().is_empty()
                    && let Some(env) = self.catalog_env_name()
                {
                    self.api_key_env.set(&env);
                }
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
                        self.rebuild_rows();
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
        if shuvarie_core::catalog::parse_provider_kind(&kind).is_none() {
            self.error = Some(format!(
                "Unknown provider type '{kind}' — press Enter to pick one"
            ));
            return AddProviderOutcome::None;
        }
        let (api_key, api_key_env) = match self.key_payload() {
            Ok(payload) => payload,
            Err(error) => {
                self.error = Some(error);
                return AddProviderOutcome::None;
            }
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
            api_key_env,
            base_url,
        }
    }

    /// The key fields to save, or the error to show: the literal key for
    /// [`KeySource::Inline`], the environment variable name for
    /// [`KeySource::Env`]. A provider that demands a key demands one from
    /// whichever source is selected.
    fn key_payload(&self) -> Result<(Option<String>, Option<String>), String> {
        let required = self.key_required();
        match self.key_source {
            KeySource::Inline => {
                let key = self.api_key.value.trim().to_string();
                if required && key.is_empty() {
                    return Err("This provider requires an API key".into());
                }
                Ok(((!key.is_empty()).then_some(key), None))
            }
            KeySource::Env => {
                // An empty field falls back to the catalog's own reference, so
                // switching to the env source is enough for a known provider.
                let typed = self.api_key_env.value.trim();
                let raw = if typed.is_empty() {
                    self.catalog_env_name().unwrap_or_default()
                } else {
                    typed.to_string()
                };
                match shuvarie_core::normalize_env_name(&raw) {
                    Some(name) if !is_env_name(&name) => Err(format!(
                        "'{raw}' is not an environment variable name — use letters, digits and \
                         underscores"
                    )),
                    // The option is only offered for a variable that is set, so
                    // this catches a name edited to one that is not.
                    Some(name) if !env_is_set(&name) => Err(format!(
                        "{name} is not set in this environment — export it, or store the key \
                         instead"
                    )),
                    Some(name) => Ok((None, Some(name))),
                    None if required => Err(
                        "This provider requires an API key — name the environment variable that \
                         holds it"
                            .into(),
                    ),
                    None => Ok((None, None)),
                }
            }
        }
    }

    /// Whether the provider being configured signs in through the OAuth2
    /// device flow: a registry origin consults its catalog entry's `auth`
    /// field, a custom provider resolves the typed kind + catalog id.
    fn oauth_login(&self) -> bool {
        match &self.origin {
            Some(provider) => provider.oauth_device_login(),
            None => {
                let kind = self.kind.value.trim();
                let catalog = self.catalog.value.trim();
                shuvarie_core::catalog::oauth_device_login_kind(
                    kind,
                    (!catalog.is_empty()).then_some(catalog),
                )
            }
        }
    }

    /// Whether the provider being configured demands a non-empty API key:
    /// OAuth2 device-flow providers never do; otherwise the catalog entry's
    /// requirement for a registry origin, else the typed catalog id's entry
    /// when it resolves, else the transport (everything but the local
    /// runtimes).
    fn key_required(&self) -> bool {
        if self.oauth_login() {
            return false;
        }
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
                    shuvarie_core::catalog::parse_provider_kind(kind).map(|k| k.r#type),
                    Some(selune::ProviderType::Ollama | selune::ProviderType::Llamafile)
                )
            }
        }
    }

    /// The environment variable Selune names for the provider being
    /// configured: the picked entry's `api_key` reference, else the typed
    /// catalog id's. That field holds a `$NAME` reference, never a secret, so
    /// it is exactly what the env source needs.
    fn catalog_env_name(&self) -> Option<String> {
        if let Some(provider) = &self.origin {
            return provider
                .api_key
                .as_deref()
                .and_then(shuvarie_core::normalize_env_name);
        }
        let catalog = self.catalog.value.trim();
        if catalog.is_empty() {
            return None;
        }
        shuvarie_core::catalog::catalog_api_key_env(catalog)
    }

    /// Whether the env variable named by the field is set in this process, so
    /// the form can say a key will be found at connect time.
    fn key_env_is_set(&self) -> bool {
        shuvarie_core::normalize_env_name(&self.api_key_env.value)
            .is_some_and(|name| env_is_set(&name))
    }

    /// Whether the env source is on offer at all: the variable Selune names for
    /// the provider this form is configuring holds a value here. An unset
    /// variable is never offered — a key source that cannot resolve would only
    /// mislead.
    fn env_option_available(&self) -> bool {
        if self.oauth_login() {
            return false;
        }
        self.catalog_env_name().is_some_and(|env| env_is_set(&env))
    }

    /// The ghost text for the inline key row when the variable Selune names for
    /// this provider is missing: the env source is hidden, so the empty field
    /// names the variable it looked for instead of leaving its absence
    /// unexplained.
    fn missing_env_placeholder(&self) -> Option<String> {
        if self.oauth_login() {
            return None;
        }
        let env = self.catalog_env_name()?;
        (!env_is_set(&env)).then(|| format!("env var {env} not found"))
    }

    fn field_line(&self, row: FieldRow<'_>) -> Line<'static> {
        let FieldRow {
            label,
            prefix,
            placeholder,
            buffer,
            field,
            suffix,
        } = row;
        let label_style = Style::new().fg(theme::text_dim());
        let hint_style = Style::new().fg(theme::text_muted());
        let active_style = Style::new().fg(theme::accent());
        let inactive_style = Style::new().fg(theme::text_dim());
        let cursor_style = Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::REVERSED);

        let is_active = self.field == field;

        let mut spans = vec![Span::styled(format!("{label:<10}"), label_style)];
        if !prefix.is_empty() {
            spans.push(Span::styled(prefix.to_string(), hint_style));
        }

        if buffer.value.is_empty() && !placeholder.is_empty() {
            // An empty value shows its ghost text, with the cursor parked in
            // front of it when the field has focus.
            if is_active {
                spans.push(Span::styled(" ".to_string(), cursor_style));
            }
            spans.push(Span::styled(placeholder.to_string(), hint_style));
        } else if is_active {
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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool, fullscreen: bool) {
        match self.stage {
            AddProviderStage::Select => self.view_select(frame, area, dimmed, fullscreen),
            AddProviderStage::KindList => self.view_kind_list(frame, area, dimmed, fullscreen),
            AddProviderStage::Details => self.view_details(frame, area, dimmed, fullscreen),
        }
    }

    fn view_select(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool, fullscreen: bool) {
        let popup = dialog_rect(area, fullscreen, 50, 55);
        let title = if fullscreen {
            "Add your first provider"
        } else {
            "Add Provider"
        };
        popup::dialog(frame, popup, title, dimmed, |inner, buf| {
            let [search_area, list_area, hint_area] =
                Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

            self.search
                .view(buf, search_area, "Type to filter providers");

            let offset = scroll_offset_for(
                self.selected,
                self.offset,
                list_area.height as usize,
                self.rows.len(),
            );
            let visible_len = list_area.height as usize;
            let mut items: Vec<ListItem> = Vec::new();
            for row in offset..self.rows.len().min(offset + visible_len) {
                let is_selected = self.selected == row;
                let item = match &self.rows[row] {
                    SelectRow::Header { registry } => {
                        let header = self
                            .registries
                            .state(registry)
                            .map(|registry| registry.header_line())
                            .unwrap_or_else(|| Line::from(String::new()));
                        ListItem::new(header)
                    }
                    SelectRow::Provider { registry, index } => {
                        match self
                            .registries
                            .state(registry)
                            .and_then(|registry| registry.providers().get(*index))
                        {
                            Some(provider) => {
                                let mut line =
                                    vec![Span::raw(provider.name.clone()).fg(theme::text())];
                                if self.existing_catalog_ids.contains(&provider.id.0) {
                                    line.push(Span::raw("  ✓ configured").fg(theme::text_muted()));
                                }
                                render_list_item_line(Line::from(line), is_selected)
                            }
                            // Rows are always in step with the manager; defensive
                            // fallback that renders nothing.
                            None => ListItem::new(Line::from(String::new())),
                        }
                    }
                    SelectRow::Custom => {
                        render_list_item("Add custom provider…".to_string(), is_selected)
                    }
                };
                items.push(item);
            }
            Widget::render(List::new(items), list_area, buf);

            let mut hints = vec![
                ("Type", "to filter"),
                ("↑↓", "navigate"),
                ("Enter", "continue"),
            ];
            if self.registries.can_toggle() {
                hints.push(("Ctrl+O", "online registry"));
            }
            hints.push(("Ctrl+I", "custom"));
            hints.push(("Esc", "cancel"));
            Paragraph::new(theme::help_line(&hints))
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }

    fn view_kind_list(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool, fullscreen: bool) {
        let popup = dialog_rect(area, fullscreen, 50, 55);
        popup::dialog(frame, popup, "Provider Type", dimmed, |inner, buf| {
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
                    let (transport, description) = TRANSPORTS[i];
                    let line = Line::from(vec![
                        Span::raw(shuvarie_core::catalog::provider_kind_name(transport))
                            .fg(theme::text()),
                        Span::raw(format!("  — {description}")).fg(theme::text_muted()),
                    ]);
                    render_list_item_line(line, i == self.kind_selected)
                })
                .collect();
            Widget::render(List::new(items), list_area, buf);

            Paragraph::new(theme::help_line(&[
                ("↑↓", "navigate"),
                ("Enter", "pick"),
                ("Esc", "back"),
            ]))
            .fg(theme::text_muted())
            .render(hint_area, buf);
        });
    }

    fn view_details(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool, fullscreen: bool) {
        let popup = dialog_rect(area, fullscreen, 60, 55);
        let title = match &self.origin {
            Some(provider) => format!("Configure Provider — {}", provider.name),
            None => "Add Custom Provider".to_string(),
        };
        popup::dialog(frame, popup, &title, dimmed, |inner, buf| {
            let name_line = self.field_line(FieldRow::new("Name", &self.name, FormField::Name));
            let kind_line = self.field_line(
                FieldRow::new("Kind", &self.kind, FormField::Kind).suffix("  (Enter to pick)"),
            );
            let catalog_suffix = if self.origin.is_none() {
                "  (optional Selune id)"
            } else {
                ""
            };
            let catalog_line = self.field_line(
                FieldRow::new("Catalog", &self.catalog, FormField::Catalog).suffix(catalog_suffix),
            );
            let url_line = self.field_line(FieldRow::new(
                "Base URL",
                &self.base_url,
                FormField::BaseUrl,
            ));
            let key_line = if self.oauth_login() {
                Line::from(vec![
                    Span::styled("Sign in   ".to_string(), Style::new().fg(theme::text_dim())),
                    Span::styled(
                        "  device flow — opens your browser on submit",
                        Style::new().fg(theme::text_muted()),
                    ),
                ])
            } else {
                match self.key_source {
                    KeySource::Inline => {
                        // Where Selune names a variable this environment does not
                        // have, the empty field shows which one as ghost text —
                        // the env source is hidden, so the reason has to be
                        // visible here.
                        let placeholder = self.missing_env_placeholder().unwrap_or_default();
                        let suffix = if self.key_required() {
                            ""
                        } else {
                            "  (not required)"
                        };
                        self.field_line(
                            FieldRow::new("API key", &self.api_key, FormField::ApiKey)
                                .placeholder(&placeholder)
                                .suffix(suffix),
                        )
                    }
                    KeySource::Env => {
                        // The variable name is what this field edits; the note
                        // says whether a key will actually be found there.
                        let suffix = if self.key_env_is_set() {
                            ""
                        } else {
                            "  (not set in this environment)"
                        };
                        self.field_line(
                            FieldRow::new("API key", &self.api_key_env, FormField::ApiKey)
                                .prefix("[env] ")
                                .suffix(suffix),
                        )
                    }
                }
            };

            let lines = vec![name_line, kind_line, catalog_line, url_line, key_line];
            let body = Paragraph::new(lines).wrap(Wrap { trim: false });
            let [body_area, error_area, hint_area] =
                Layout::vertical([Min(0), Length(2), Length(1)]).areas(inner);
            body.render(body_area, buf);
            if let Some(e) = &self.error {
                Paragraph::new(e.as_str())
                    .fg(theme::error())
                    .render(error_area, buf);
            }
            let mut hints = vec![("Tab", "next"), ("Enter", "submit")];
            if self.env_option_available() {
                hints.push(("Alt+E", "read key from env"));
            }
            hints.push(("Esc", "back"));
            Paragraph::new(theme::help_line(&hints))
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }
}

/// The rect a dialog paints into: `centered_rect` for the ordinary popup, the
/// viewport inset by a small margin when the dialog is one fullscreen page of
/// the onboarding flow.
pub fn dialog_rect(area: Rect, fullscreen: bool, percent_x: u16, percent_y: u16) -> Rect {
    if fullscreen {
        fullscreen_rect(area)
    } else {
        centered_rect(percent_x, percent_y, area)
    }
}

/// The rect an onboarding page fills: the whole viewport, inset by a small
/// margin so a fullscreen dialog keeps a border's worth of room from the
/// terminal's edges.
pub fn fullscreen_rect(area: Rect) -> Rect {
    area.inner(Margin {
        vertical: 1,
        horizontal: 2,
    })
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
    use crate::tui::registry::FetchState;
    use selune::{InferenceProvider, Model as SeluneModel, ModelLimit};
    use termina::event::Modifiers;

    fn catalog_provider(id: &str, name: &str, ptype: selune::ProviderType) -> Provider {
        Provider {
            name: name.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: Some("$SHUVARIE_TEST_CATALOG_KEY".into()),
            auth: None,
            api_endpoint: Some("https://api.example.com/v1".into()),
            doc: None,
            r#type: Some(ptype),
            dialect: None,
            peaks: None,
            default_large_model_id: None,
            default_small_model_id: None,
            models: vec![SeluneModel {
                id: "test-model".into(),
                model_code: selune::ModelCode::new("test-org", "test-model", None).unwrap(),
                name: "Test Model".into(),
                reasoning: false,
                reasoning_options: Vec::new(),
                attachment: false,
                limit: ModelLimit::default(),
                cost: Default::default(),
                peaks: None,
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

    /// A fixture whose Selune entry names `env` as the variable the key lives
    /// in — the shape every real catalog entry has.
    fn keyed_provider(id: &str, env: &str) -> Provider {
        let mut provider = catalog_provider(id, "Acme", selune::ProviderType::Openai);
        provider.api_key = Some(format!("${env}"));
        provider
    }

    /// Set a variable for the body of one test. Each test uses its own name, so
    /// parallel tests never read each other's environment.
    fn set_env(name: &str, value: &str) {
        // SAFETY: the variable is unique to the calling test.
        unsafe { std::env::set_var(name, value) };
    }

    fn unset_env(name: &str) {
        // SAFETY: the variable is unique to the calling test.
        unsafe { std::env::remove_var(name) };
    }

    fn form() -> AddProviderForm {
        AddProviderForm::with_registries(
            RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default())
                .with_state_providers(shuvarie_core::catalog::SELUNE_REGISTRY, providers()),
            &["Existing".to_string()],
            &[],
        )
    }

    /// A second registry after selune, each with fixture providers.
    fn form_with_acme() -> AddProviderForm {
        let mut config = shuvarie_core::RegistriesConfig::default();
        config.custom.push(shuvarie_core::CustomRegistry {
            name: "acme".to_string(),
            url: Some("https://example.com/providers.json".to_string()),
            ..shuvarie_core::CustomRegistry::default()
        });
        AddProviderForm::with_registries(
            RegistryManager::from_config(&config)
                .with_state_providers(shuvarie_core::catalog::SELUNE_REGISTRY, providers())
                .with_state_providers(
                    "acme",
                    vec![catalog_provider(
                        "acme-x",
                        "Acme X",
                        selune::ProviderType::OpenaiCompat,
                    )],
                ),
            &[],
            &[],
        )
    }

    /// The grouped select-stage list flattened for assertions: group header
    /// rows as `HEADER:<registry>`, a provider row's provider name, and the
    /// pinned custom row as `custom`.
    fn list_strings(form: &AddProviderForm) -> Vec<String> {
        form.rows
            .iter()
            .map(|row| match row {
                SelectRow::Header { registry } => format!(
                    "HEADER:{}",
                    form.registries
                        .state(registry)
                        .map(|state| state.name().to_string())
                        .unwrap_or_default()
                ),
                SelectRow::Provider { registry, index } => {
                    form.registries.state(registry).unwrap().providers()[*index]
                        .name
                        .clone()
                }
                SelectRow::Custom => "custom".to_string(),
            })
            .collect()
    }

    fn custom_form() -> AddProviderForm {
        let mut form = form();
        form.open_details(None);
        form
    }

    fn oauth_provider(id: &str, name: &str, ptype: selune::ProviderType) -> Provider {
        let mut p = catalog_provider(id, name, ptype);
        p.auth = Some(selune::AuthMethod::Oauth2Device);
        p
    }

    #[test]
    fn transports_cover_every_protocol_kind_and_dialect_and_parse() {
        let seen: std::collections::HashSet<shuvarie_llm::ProviderKind> =
            TRANSPORTS.iter().map(|(t, _)| *t).collect();
        for (kind, _) in TRANSPORTS {
            let name = shuvarie_core::catalog::provider_kind_name(kind);
            assert_eq!(
                shuvarie_core::catalog::parse_provider_kind(&name),
                Some(kind),
                "{name} should parse"
            );
        }
        // Every protocol kind must be pickable in the form.
        for t in [
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
            selune::ProviderType::Llamafile,
            selune::ProviderType::Chatgpt,
            selune::ProviderType::Copilot,
            selune::ProviderType::Cohere,
            selune::ProviderType::Voyageai,
        ] {
            let kind = shuvarie_llm::ProviderKind::new(t, None);
            assert!(seen.contains(&kind), "{t:?} missing from TRANSPORTS");
        }
        // And every openai-compat vendor dialect.
        for t in [
            selune::Dialect::Deepseek,
            selune::Dialect::Doubleword,
            selune::Dialect::Groq,
            selune::Dialect::Huggingface,
            selune::Dialect::Hyperbolic,
            selune::Dialect::Minimax,
            selune::Dialect::Mira,
            selune::Dialect::Mistral,
            selune::Dialect::Moonshot,
            selune::Dialect::Perplexity,
            selune::Dialect::Together,
            selune::Dialect::Venice,
            selune::Dialect::Xai,
            selune::Dialect::Xiaomimimo,
            selune::Dialect::Zai,
        ] {
            let kind = shuvarie_llm::ProviderKind::new(selune::ProviderType::OpenaiCompat, Some(t));
            assert!(seen.contains(&kind), "{t:?} missing from TRANSPORTS");
        }
        assert_eq!(seen.len(), TRANSPORTS.len(), "duplicates in TRANSPORTS");
    }

    #[test]
    fn empty_query_lists_all_providers_grouped_by_registry() {
        let form = form();
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "OpenAI", "Ollama", "custom"]
        );
        assert_eq!(form.selected, 1, "selection starts past the header");
    }

    #[test]
    fn groups_follow_registry_order_and_the_custom_row_stays_last() {
        let form = form_with_acme();
        assert_eq!(
            list_strings(&form),
            vec![
                "HEADER:Selune",
                "OpenAI",
                "Ollama",
                "HEADER:acme",
                "Acme X",
                "custom"
            ]
        );
    }

    #[test]
    fn navigation_skips_header_rows() {
        let mut form = form_with_acme();
        assert_eq!(form.selected, 1, "the first header is skipped");
        form.update(AddProviderMessage::Next);
        assert_eq!(form.selected, 2, "Ollama");
        form.update(AddProviderMessage::Next);
        assert_eq!(form.selected, 4, "the acme header is skipped");
        form.update(AddProviderMessage::Prev);
        assert_eq!(form.selected, 2, "the acme header is skipped again");
        form.update(AddProviderMessage::Prev);
        form.update(AddProviderMessage::Prev);
        assert_eq!(form.selected, 1, "nothing selectable above the first row");
        for _ in 0..4 {
            form.update(AddProviderMessage::Next);
        }
        assert_eq!(
            form.selected,
            form.rows.len() - 1,
            "the custom row is the last selectable"
        );
    }

    /// A select-stage list long enough to overflow a small viewport.
    fn crowded_form() -> AddProviderForm {
        let providers: Vec<Provider> = (0..20)
            .map(|i| {
                catalog_provider(
                    &format!("p{i}"),
                    &format!("Provider {i}"),
                    selune::ProviderType::OpenaiCompat,
                )
            })
            .collect();
        AddProviderForm::with_registries(
            RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default())
                .with_state_providers(shuvarie_core::catalog::SELUNE_REGISTRY, providers),
            &[],
            &[],
        )
    }

    #[test]
    fn navigation_walks_the_page_before_the_list_scrolls() {
        let mut form = crowded_form();
        // A five-row viewport over twenty providers, a group header, and the
        // pinned custom row.
        form.update(AddProviderMessage::Resize { viewport_height: 5 });
        assert_eq!((form.selected, form.offset), (1, 0), "the first provider");

        // Down: the highlight walks down the page, and the list only follows
        // once it reaches the last visible row.
        for expected in [(2, 0), (3, 0), (4, 0), (5, 1), (6, 2)] {
            form.update(AddProviderMessage::Next);
            assert_eq!((form.selected, form.offset), expected);
        }

        // Up: the list holds its position while the highlight climbs back up
        // the page, then follows it so the top row stays visible.
        for expected in [(5, 2), (4, 2), (3, 2), (2, 2), (1, 1)] {
            form.update(AddProviderMessage::Prev);
            assert_eq!((form.selected, form.offset), expected);
        }
    }

    #[test]
    fn query_filters_providers_and_the_custom_row_stays_last() {
        let mut form = form();
        form.update(AddProviderMessage::Search(SearchMessage::Input('p')));
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "OpenAI", "custom"],
            "only OpenAI fuzzy-matches 'p'"
        );
        form.update(AddProviderMessage::Next);
        assert_eq!(
            form.selected,
            form.rows.len() - 1,
            "custom row after a Next"
        );
    }

    #[test]
    fn searching_hides_groups_without_matches() {
        let mut form = form_with_acme();
        form.update(AddProviderMessage::Search(SearchMessage::Input('p')));
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "OpenAI", "custom"],
            "the acme group matches nothing"
        );

        let mut form = form_with_acme();
        form.update(AddProviderMessage::Search(SearchMessage::Input('x')));
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:acme", "Acme X", "custom"],
            "only the acme group matches 'x'"
        );
    }

    #[test]
    fn an_idle_empty_group_keeps_its_header() {
        let registries = RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default())
            .with_state_providers(shuvarie_core::catalog::SELUNE_REGISTRY, vec![]);
        let form = AddProviderForm::with_registries(registries, &[], &[]);
        assert_eq!(list_strings(&form), vec!["HEADER:Selune", "custom"]);
        assert_eq!(form.selected, 1, "the custom row is selectable");
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
        form.selected = form.rows.len() - 1;
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
    fn ctrl_o_is_hidden_when_nothing_can_go_online() {
        let form = AddProviderForm::with_registries(RegistryManager::new(Vec::new()), &[], &[]);
        assert_eq!(form.rows.len(), 1, "just the custom row");
        assert!(!form.registries.can_toggle());
        assert_eq!(
            form.map_event(&key(KeyCode::Char('o'), Modifiers::CONTROL)),
            None,
            "no toggle mapping when nothing can go online"
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
    fn oauth_kind_skips_the_api_key_field() {
        let mut form = custom_form();
        form.name.set("ChatGPT");
        form.kind.set("chatgpt");
        assert!(form.oauth_login(), "chatgpt is an OAuth2 device-flow kind");
        assert!(
            !form.key_required(),
            "OAuth2 device-flow needs no pasted key"
        );
        form.field = FormField::BaseUrl;
        form.update(AddProviderMessage::NextField);
        assert_eq!(form.field, FormField::Name, "navigation skips ApiKey");
        form.update(AddProviderMessage::PrevField);
        assert_eq!(form.field, FormField::BaseUrl, "navigation skips ApiKey");
        assert!(matches!(
            form.update(AddProviderMessage::Submit),
            AddProviderOutcome::Submit { api_key: None, .. }
        ));
    }

    #[test]
    fn non_oauth_kind_keeps_the_api_key_field() {
        let mut form = custom_form();
        form.name.set("Acme");
        form.kind.set("openai-compat");
        assert!(!form.oauth_login());
        assert!(form.key_required());
        form.field = FormField::BaseUrl;
        form.update(AddProviderMessage::NextField);
        assert_eq!(form.field, FormField::ApiKey);
    }

    #[test]
    fn oauth_registry_origin_focuses_name() {
        let mut form = form();
        form.open_details(Some(&oauth_provider(
            "chatgpt",
            "ChatGPT",
            selune::ProviderType::Chatgpt,
        )));
        assert_eq!(
            form.field,
            FormField::Name,
            "OAuth2 origin has no key field"
        );
        assert!(form.oauth_login());
        assert!(!form.key_required());
    }

    #[test]
    fn registry_origin_defaults_to_the_catalog_env_var() {
        set_env("SHUVARIE_TEST_ENV_KEY_DEFAULT", "sk-env");
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-default",
            "SHUVARIE_TEST_ENV_KEY_DEFAULT",
        )));
        assert_eq!(
            form.key_source,
            KeySource::Env,
            "a set variable becomes the default key source"
        );
        assert_eq!(
            form.api_key_env.value, "SHUVARIE_TEST_ENV_KEY_DEFAULT",
            "seeded from the `$NAME` the catalog names"
        );
        assert!(form.api_key.value.is_empty(), "no secret is prefilled");
        assert!(form.env_option_available());

        let outcome = form.update(AddProviderMessage::Submit);
        assert!(matches!(
            outcome,
            AddProviderOutcome::Submit {
                api_key: None,
                api_key_env: Some(env),
                ..
            } if env == "SHUVARIE_TEST_ENV_KEY_DEFAULT"
        ));
        unset_env("SHUVARIE_TEST_ENV_KEY_DEFAULT");
    }

    #[test]
    fn an_unset_variable_is_not_offered_at_all() {
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-unset",
            "SHUVARIE_TEST_ENV_KEY_NEVER_SET",
        )));
        assert_eq!(
            form.key_source,
            KeySource::Inline,
            "the env source is hidden, so the form starts on the key"
        );
        assert!(form.api_key_env.value.is_empty(), "nothing to prefill");
        assert!(!form.env_option_available());
        assert_eq!(
            form.missing_env_placeholder().as_deref(),
            Some("env var SHUVARIE_TEST_ENV_KEY_NEVER_SET not found"),
            "the hidden source is named where the key is asked for"
        );
        assert_eq!(
            form.map_event(&key(KeyCode::Char('e'), Modifiers::ALT)),
            None,
            "Alt+E is not offered when there is no variable to read"
        );
        form.update(AddProviderMessage::ToggleKeySource);
        assert_eq!(
            form.key_source,
            KeySource::Inline,
            "the toggle is a no-op without a set variable"
        );
        form.update(AddProviderMessage::Submit);
        assert!(
            form.error.as_deref().unwrap().contains("API key"),
            "a key is still required from the source that is offered"
        );
    }

    #[test]
    fn the_missing_variable_placeholder_only_shows_for_a_named_unset_variable() {
        // No catalog entry at all: nothing to name, nothing to say.
        let mut custom = custom_form();
        assert_eq!(custom.missing_env_placeholder(), None);
        custom.catalog.set("not-a-provider");
        assert_eq!(custom.missing_env_placeholder(), None);

        // A catalog entry names the variable it wants; whether the placeholder
        // shows depends on whether this environment happens to have it.
        custom.catalog.set("openai");
        if env_is_set("OPENAI_API_KEY") {
            assert_eq!(
                custom.missing_env_placeholder(),
                None,
                "the variable is present"
            );
        } else {
            assert_eq!(
                custom.missing_env_placeholder().as_deref(),
                Some("env var OPENAI_API_KEY not found"),
                "the typed catalog id names the variable it looked for"
            );
        }

        // OAuth kinds have no key row to put a placeholder in.
        let mut oauth = form();
        oauth.open_details(Some(&oauth_provider(
            "chatgpt",
            "ChatGPT",
            selune::ProviderType::Chatgpt,
        )));
        assert_eq!(oauth.missing_env_placeholder(), None);
    }

    #[test]
    fn the_details_dialog_names_the_missing_variable() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        fn rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
            (0..buffer.area().height)
                .map(|y| {
                    (0..buffer.area().width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect()
        }

        fn key_row(form: &AddProviderForm, terminal: &mut Terminal<TestBackend>) -> String {
            terminal
                .draw(|frame| form.view(frame, frame.area(), false, false))
                .unwrap();
            rows(terminal.backend().buffer())
                .into_iter()
                // The dialog's left border precedes the label.
                .find(|row| row.contains("API key"))
                .unwrap_or_default()
        }

        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-rendered",
            "SHUVARIE_TEST_ENV_KEY_RENDERED_NEVER_SET",
        )));
        // Wide enough that the key row stays on one line, so the assertion
        // reads the placeholder whole.
        let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();

        let row = key_row(&form, &mut terminal);
        assert!(
            row.contains("env var SHUVARIE_TEST_ENV_KEY_RENDERED_NEVER_SET not found"),
            "the empty field is a placeholder naming the variable it looked for: {row:?}"
        );

        // A typed key takes the placeholder's place.
        form.api_key.set("sk-typed");
        let row = key_row(&form, &mut terminal);
        assert!(row.contains("sk-typed"), "{row:?}");
        assert!(
            !row.contains("not found"),
            "the placeholder steps aside: {row:?}"
        );

        // The env source is hidden, so its hint bar entry is too.
        let text = rows(terminal.backend().buffer()).join("\n");
        assert!(
            !text.contains("Alt+E"),
            "no env toggle without a variable:\n{text}"
        );
    }

    #[test]
    fn toggling_to_an_inline_key_requires_and_stores_it() {
        set_env("SHUVARIE_TEST_ENV_KEY_TOGGLE", "sk-env");
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-toggle",
            "SHUVARIE_TEST_ENV_KEY_TOGGLE",
        )));
        assert_eq!(form.key_source, KeySource::Env);
        form.update(AddProviderMessage::ToggleKeySource);
        assert_eq!(form.key_source, KeySource::Inline);
        form.update(AddProviderMessage::Submit);
        assert!(
            form.error.as_deref().unwrap().contains("API key"),
            "the env source no longer satisfies the requirement"
        );

        form.api_key.set("sk-test");
        assert!(matches!(
            form.update(AddProviderMessage::Submit),
            AddProviderOutcome::Submit {
                api_key: Some(key),
                api_key_env: None,
                ..
            } if key == "sk-test"
        ));
        unset_env("SHUVARIE_TEST_ENV_KEY_TOGGLE");
    }

    #[test]
    fn typing_edits_whichever_key_field_is_shown() {
        set_env("SHUVARIE_TEST_ENV_KEY_TYPING", "sk-env");
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-typing",
            "SHUVARIE_TEST_ENV_KEY_TYPING",
        )));
        form.field = FormField::ApiKey;
        assert_eq!(form.key_source, KeySource::Env);
        form.update(AddProviderMessage::Input('2'));
        assert_eq!(form.api_key_env.value, "SHUVARIE_TEST_ENV_KEY_TYPING2");
        assert!(form.api_key.value.is_empty());

        form.update(AddProviderMessage::ToggleKeySource);
        form.update(AddProviderMessage::Input('k'));
        assert_eq!(form.api_key.value, "k");
        assert_eq!(
            form.api_key_env.value, "SHUVARIE_TEST_ENV_KEY_TYPING2",
            "each source keeps its own text"
        );
        unset_env("SHUVARIE_TEST_ENV_KEY_TYPING");
    }

    #[test]
    fn alt_e_toggles_the_key_source() {
        set_env("SHUVARIE_TEST_ENV_KEY_ALT_E", "sk-env");
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-alt-e",
            "SHUVARIE_TEST_ENV_KEY_ALT_E",
        )));
        assert!(matches!(
            form.map_event(&key(KeyCode::Char('e'), Modifiers::ALT)),
            Some(AddProviderMessage::ToggleKeySource)
        ));
        unset_env("SHUVARIE_TEST_ENV_KEY_ALT_E");
    }

    #[test]
    fn catalog_env_name_resolves_the_typed_catalog_id() {
        let mut form = custom_form();
        form.kind.set("openai");
        assert_eq!(form.catalog_env_name(), None, "no catalog id yet");
        form.catalog.set("openai");
        assert_eq!(
            form.catalog_env_name().as_deref(),
            Some("OPENAI_API_KEY"),
            "the typed catalog id names the variable"
        );
        form.catalog.set("not-a-provider");
        assert_eq!(form.catalog_env_name(), None);
    }

    #[test]
    fn env_source_rejects_something_that_is_not_a_variable_name() {
        set_env("SHUVARIE_TEST_ENV_KEY_INVALID", "sk-env");
        let mut form = form();
        form.open_details(Some(&keyed_provider(
            "acme-env-invalid",
            "SHUVARIE_TEST_ENV_KEY_INVALID",
        )));
        assert_eq!(form.key_source, KeySource::Env);

        form.api_key_env.set("sk-abc/def");
        form.update(AddProviderMessage::Submit);
        assert!(
            form.error
                .as_deref()
                .unwrap()
                .contains("not an environment variable name"),
            "{:?}",
            form.error
        );

        form.api_key_env.set("SHUVARIE_TEST_ENV_KEY_NOT_EXPORTED");
        form.update(AddProviderMessage::Submit);
        assert!(
            form.error
                .as_deref()
                .unwrap()
                .contains("is not set in this environment"),
            "{:?}",
            form.error
        );

        // Clearing the field falls back to the variable the catalog names,
        // which is set.
        form.api_key_env.clear();
        assert!(matches!(
            form.update(AddProviderMessage::Submit),
            AddProviderOutcome::Submit {
                api_key_env: Some(env),
                ..
            } if env == "SHUVARIE_TEST_ENV_KEY_INVALID"
        ));
        unset_env("SHUVARIE_TEST_ENV_KEY_INVALID");
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
        assert_eq!(
            form.update(AddProviderMessage::ToggleSource),
            AddProviderOutcome::FetchRegistries(vec![
                shuvarie_core::catalog::SELUNE_REGISTRY.to_string()
            ])
        );
        form.update(AddProviderMessage::RegistryLoaded {
            registry: shuvarie_core::catalog::SELUNE_REGISTRY.to_string(),
            providers: vec![catalog_provider(
                "remote-only",
                "RemoteOnly",
                selune::ProviderType::Openai,
            )],
        });
        assert_eq!(
            form.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .state,
            FetchState::Idle
        );
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "RemoteOnly", "custom"],
            "snapshot shows the remote registry"
        );
    }

    #[test]
    fn registry_loaded_is_ignored_while_offline() {
        let mut form = form();
        form.update(AddProviderMessage::RegistryLoaded {
            registry: shuvarie_core::catalog::SELUNE_REGISTRY.to_string(),
            providers: vec![],
        });
        assert_eq!(
            form.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .state,
            FetchState::Idle
        );
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "OpenAI", "Ollama", "custom"],
            "offline snapshot untouched"
        );
    }

    #[test]
    fn registry_error_is_surfaced_and_keeps_list() {
        let mut form = form();
        form.update(AddProviderMessage::RegistryError {
            registry: shuvarie_core::catalog::SELUNE_REGISTRY.to_string(),
            error: "offline".into(),
        });
        assert_eq!(
            form.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .state,
            FetchState::Failed("offline".into())
        );
        assert_eq!(
            list_strings(&form),
            vec!["HEADER:Selune", "OpenAI", "Ollama", "custom"],
            "list keeps the offline snapshot"
        );
    }

    #[test]
    fn foreign_registry_events_are_ignored() {
        let mut form = form();
        assert!(matches!(
            form.update(AddProviderMessage::ToggleSource),
            AddProviderOutcome::FetchRegistries(_)
        ));
        let before = list_strings(&form);
        form.update(AddProviderMessage::RegistryLoaded {
            registry: "some-custom-registry".into(),
            providers: vec![catalog_provider(
                "remote-only",
                "RemoteOnly",
                selune::ProviderType::Openai,
            )],
        });
        assert_eq!(
            form.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .state,
            FetchState::Fetching,
            "another registry's fetch does not resolve this source"
        );
        assert_eq!(list_strings(&form), before, "snapshot untouched");

        form.update(AddProviderMessage::RegistryError {
            registry: "some-custom-registry".into(),
            error: "boom".into(),
        });
        assert_eq!(
            form.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .state,
            FetchState::Fetching,
            "another registry's failure does not mark this source"
        );
    }

    fn key(code: KeyCode, mods: Modifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }
}
