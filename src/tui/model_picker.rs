use std::collections::HashMap;
use std::ops::Not;

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use selune::Provider;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;
use crate::tui::utils::num::fmt_scaled_number;

use super::add_provider::centered_rect;
use super::list::{render_list_item_line, scroll_offset_for};
use super::registry::RegistryManager;
use super::search::{Search, SearchMessage, filter_indices};
use super::theme;

use shuvarie_core::{Connections, Model};

pub enum ModelPickerMessage {
    Search(SearchMessage),
    Next,
    Prev,
    Select,
    Close,
    ToggleSource,
    RegistryLoaded {
        registry: String,
        providers: Vec<Provider>,
    },
    RegistryError {
        registry: String,
        error: String,
    },
    ProviderModels {
        provider_name: String,
        models: Vec<Model>,
    },
    Resize {
        viewport_height: u16,
    },
}

#[derive(Debug, PartialEq)]
pub enum ModelPickerEffect {
    /// A model was chosen; `provider` switches the active provider first
    /// when it differs (`None` keeps the active one — the raw-input row).
    Selected {
        provider: Option<String>,
        model: String,
    },
    FetchRegistries(Vec<String>),
    Close,
}

/// One configured provider's slot in the picker.
struct ProviderSpec {
    id: String,
    name: String,
    catalog: Option<String>,
    active: bool,
}

#[derive(Clone, Debug)]
enum Row {
    /// The pinned "Use this input anyway" row (only while a query is typed).
    Custom,
    /// A registry's group header; resolved against the manager at render time
    /// so badges stay live.
    RegistryHeader {
        registry: String,
    },
    /// A configured provider's group header.
    Header {
        name: String,
        active: bool,
    },
    Model {
        provider_id: String,
        id: String,
        context: Option<u32>,
    },
    Status {
        text: String,
    },
}

impl Row {
    fn selectable(&self) -> bool {
        matches!(self, Row::Custom | Row::Model { .. })
    }
}

pub struct ModelPicker {
    pub open: bool,
    pub search: Search,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub registries: RegistryManager,
    specs: Vec<ProviderSpec>,
    live: HashMap<String, Vec<Model>>,
    rows: Vec<Row>,
}

impl ModelPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            search: Search::new(),
            selected: 0,
            offset: 0,
            viewport_height: 0,
            registries: RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default()),
            specs: Vec::new(),
            live: HashMap::new(),
            rows: Vec::new(),
        }
    }

    pub fn open(
        &mut self,
        connections: &Connections,
        live: &HashMap<String, Vec<Model>>,
        registries: &shuvarie_core::RegistriesConfig,
    ) {
        self.open_with(
            Self::provider_specs(connections),
            live.clone(),
            RegistryManager::from_config(registries),
        );
    }

    fn open_with(
        &mut self,
        specs: Vec<ProviderSpec>,
        live: HashMap<String, Vec<Model>>,
        registries: RegistryManager,
    ) {
        self.open = true;
        self.registries = registries;
        self.specs = specs;
        self.live = live;
        self.search.clear();
        self.search.active = true;
        self.rebuild();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
    }

    /// The registry ids to fetch when the picker opens.
    pub fn ids_needing_fetch(&self) -> Vec<String> {
        self.registries.ids_needing_fetch()
    }

    /// Providers whose models must be fetched live (`ListModels`): no cached
    /// live models and no registry knows their catalog id.
    pub fn pending_live_providers(&self) -> Vec<String> {
        self.specs
            .iter()
            .filter(|s| {
                let in_registry = s
                    .catalog
                    .as_deref()
                    .is_some_and(|id| self.registries.resolving_registry_index(id).is_some());
                !self.live.contains_key(&s.id) && !in_registry
            })
            .map(|s| s.id.clone())
            .collect()
    }

    /// The first catalog/live model under the active provider — the default
    /// pick when the picker closes without a selection.
    pub fn active_default_model(&self) -> Option<String> {
        let active_id = self.specs.iter().find(|s| s.active)?.id.clone();
        self.rows.iter().find_map(|row| match row {
            Row::Model {
                provider_id, id, ..
            } if *provider_id == active_id => Some(id.clone()),
            _ => None,
        })
    }

    fn provider_specs(connections: &Connections) -> Vec<ProviderSpec> {
        let active_id = connections
            .active
            .as_ref()
            .map(|a| a.provider.clone())
            .unwrap_or_default();
        let mut specs: Vec<ProviderSpec> = connections
            .providers
            .iter()
            .map(|(id, pc)| ProviderSpec {
                id: id.clone(),
                name: pc.name.clone(),
                catalog: pc.catalog_id().map(str::to_string),
                active: *id == active_id,
            })
            .collect();
        specs.sort_by(|a, b| {
            b.active
                .cmp(&a.active)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.id.cmp(&b.id))
        });
        specs
    }

    /// The matched model rows for one provider's group: `None` when the group
    /// has no match while searching (its header drops out with it).
    fn matched_spec_rows(
        &self,
        spec: &ProviderSpec,
        query: &str,
        filtering: bool,
    ) -> Option<Vec<Row>> {
        let models = self.provider_model_rows(spec);
        let matched: Vec<Row> = if filtering {
            let filtered_ids = filter_indices(query, models.len(), |i| match &models[i] {
                Row::Model { id, .. } => id.clone(),
                _ => String::new(),
            });
            if filtered_ids.len() == models.len() {
                models
            } else {
                filtered_ids
                    .into_iter()
                    .map(|i| models[i].clone())
                    .collect()
            }
        } else {
            models
        };
        matched.is_empty().not().then_some(matched)
    }

    fn provider_header(spec: &ProviderSpec) -> Row {
        Row::Header {
            name: spec.name.clone(),
            active: spec.active,
        }
    }

    fn rebuild(&mut self) {
        let query = self.search.query.trim().to_string();
        let filtering = !query.is_empty();
        let mut rows: Vec<Row> = Vec::new();
        if filtering {
            rows.push(Row::Custom);
        }
        // Specs resolve to the first registry whose active snapshot knows
        // their catalog id; the rest list live models with no registry above
        // them. Live providers lead the list — the active provider often has
        // no registry behind it — then registry groups follow in registry
        // order. While searching, groups with no match disappear — headers
        // included; a registry nobody resolves to has no header at all.
        let mut ungrouped: Vec<usize> = Vec::new();
        let mut grouped: Vec<Vec<usize>> = vec![Vec::new(); self.registries.registries().len()];
        for (index, spec) in self.specs.iter().enumerate() {
            match spec
                .catalog
                .as_deref()
                .and_then(|id| self.registries.resolving_registry_index(id))
            {
                Some(registry) => grouped[registry].push(index),
                None => ungrouped.push(index),
            }
        }
        for index in ungrouped {
            let spec = &self.specs[index];
            if let Some(matched) = self.matched_spec_rows(spec, &query, filtering) {
                rows.push(Self::provider_header(spec));
                rows.extend(matched);
            }
        }
        for (registry_index, specs) in grouped.into_iter().enumerate() {
            let mut group_rows: Vec<Row> = Vec::new();
            for index in specs {
                let spec = &self.specs[index];
                if let Some(matched) = self.matched_spec_rows(spec, &query, filtering) {
                    group_rows.push(Self::provider_header(spec));
                    group_rows.extend(matched);
                }
            }
            if group_rows.is_empty() {
                continue;
            }
            rows.push(Row::RegistryHeader {
                registry: self.registries.registries()[registry_index]
                    .id()
                    .to_string(),
            });
            rows.extend(group_rows);
        }
        self.rows = rows;
        self.selected = self.first_selectable();
        self.offset = 0;
        self.recompute_offset();
    }

    /// The model rows for one provider: registry models when its catalog id
    /// resolves in a registry's active snapshot, else the live `ListModels`
    /// cache. An unresolved catalog id during a pending remote fetch shows a
    /// loading row instead of falling through to live data — the curated list
    /// is imminent, and the spec migrates into its registry group once the
    /// fetch resolves.
    fn provider_model_rows(&self, spec: &ProviderSpec) -> Vec<Row> {
        if let Some(catalog_id) = &spec.catalog {
            if let Some(provider) = self
                .registries
                .resolving_registry_index(catalog_id)
                .and_then(|index| self.registries.registries().get(index))
                .and_then(|registry| registry.providers().iter().find(|p| &p.id.0 == catalog_id))
            {
                return provider
                    .models
                    .iter()
                    .map(|m| Row::Model {
                        provider_id: spec.id.clone(),
                        id: m.id.clone(),
                        context: m.limit.context.map(|c| c.max(0) as u32),
                    })
                    .collect();
            }
            if self.registries.any_fetching() {
                return vec![Row::Status {
                    text: "loading registry…".to_string(),
                }];
            }
        }
        match self.live.get(&spec.id) {
            Some(models) if !models.is_empty() => models
                .iter()
                .map(|m| Row::Model {
                    provider_id: spec.id.clone(),
                    id: m.id.clone(),
                    context: m.context_length,
                })
                .collect(),
            Some(_) => vec![Row::Status {
                text: "no models found".to_string(),
            }],
            None => vec![Row::Status {
                text: "loading…".to_string(),
            }],
        }
    }

    fn first_selectable(&self) -> usize {
        self.rows
            .iter()
            .position(|row| row.selectable())
            .unwrap_or(0)
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        self.offset = scroll_offset_for(self.selected, self.offset, vh, self.rows.len());
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ModelPickerMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(ModelPickerMessage::Next),
                KeyCode::Char('p') => Some(ModelPickerMessage::Prev),
                KeyCode::Char('o') if self.registries.can_toggle() => {
                    Some(ModelPickerMessage::ToggleSource)
                }
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(ModelPickerMessage::Close),
            KeyCode::Down => Some(ModelPickerMessage::Next),
            KeyCode::Up => Some(ModelPickerMessage::Prev),
            KeyCode::Enter => Some(ModelPickerMessage::Select),
            KeyCode::Backspace => Some(ModelPickerMessage::Search(SearchMessage::Backspace)),
            KeyCode::Char(c) => Some(ModelPickerMessage::Search(SearchMessage::Input(c))),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ModelPickerMessage) -> Option<ModelPickerEffect> {
        if !self.open && !matches!(msg, ModelPickerMessage::Close) {
            return None;
        }
        match msg {
            ModelPickerMessage::Close => {
                self.close();
                Some(ModelPickerEffect::Close)
            }
            ModelPickerMessage::ToggleSource => {
                let ids = self.registries.initiate_online();
                // Going online may have switched sources; the rows re-read
                // the manager either way.
                self.rebuild();
                if ids.is_empty() {
                    None
                } else {
                    Some(ModelPickerEffect::FetchRegistries(ids))
                }
            }
            ModelPickerMessage::RegistryLoaded {
                registry,
                providers,
            } => {
                // Only registries this picker tracks resolve; other
                // registries' fetches (e.g. `remote-first` ones at startup)
                // refresh nothing here.
                if self.registries.on_loaded(&registry, providers) {
                    self.rebuild();
                }
                None
            }
            ModelPickerMessage::RegistryError { registry, error } => {
                self.registries.on_error(&registry, error);
                None
            }
            ModelPickerMessage::ProviderModels {
                provider_name,
                models,
            } => {
                self.live.insert(provider_name, models);
                self.rebuild();
                None
            }
            ModelPickerMessage::Next => {
                self.move_selection(1);
                None
            }
            ModelPickerMessage::Prev => {
                self.move_selection(-1);
                None
            }
            ModelPickerMessage::Search(m) => {
                self.search.update(m);
                self.rebuild();
                None
            }
            ModelPickerMessage::Select => {
                let effect = match self.rows.get(self.selected) {
                    Some(Row::Custom) => Some(ModelPickerEffect::Selected {
                        provider: None,
                        model: self.search.query.trim().to_string(),
                    }),
                    Some(Row::Model {
                        provider_id, id, ..
                    }) => Some(ModelPickerEffect::Selected {
                        provider: Some(provider_id.clone()),
                        model: id.clone(),
                    }),
                    _ => None,
                };
                if effect.is_some() {
                    self.close();
                }
                effect
            }
            ModelPickerMessage::Resize { viewport_height } => {
                if self.viewport_height != viewport_height {
                    self.viewport_height = viewport_height;
                    self.recompute_offset();
                }
                None
            }
        }
    }

    /// Steps the selection by `dir`, skipping non-selectable rows.
    fn move_selection(&mut self, dir: isize) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let mut next = self.selected as isize;
        loop {
            next += dir;
            if next < 0 {
                next = 0;
            }
            if next >= len as isize {
                next = len as isize - 1;
            }
            if self.rows[next as usize].selectable() || next == 0 || next == len as isize - 1 {
                break;
            }
        }
        self.selected = next as usize;
        self.recompute_offset();
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 60, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Select Model");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [input_area, list_area, hint_area] =
            Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

        self.search.view(frame, input_area, "Type to filter models");

        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            list_area.height as usize,
            self.rows.len(),
        );
        let visible_len = list_area.height as usize;
        let mut items: Vec<ListItem> = Vec::new();
        for idx in offset..self.rows.len().min(offset + visible_len) {
            let is_selected = self.selected == idx;
            let row = &self.rows[idx];
            let item = match row {
                Row::Custom => {
                    let mut line = vec![
                        Span::raw("Use this input anyway")
                            .fg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    ];
                    line.push(
                        Span::raw(format!("  — {}", self.search.query.trim()))
                            .fg(theme::text_muted()),
                    );
                    render_list_item_line(Line::from(line), is_selected)
                }
                Row::RegistryHeader { registry } => {
                    let header = self
                        .registries
                        .state(registry)
                        .map(|registry| registry.header_line())
                        .unwrap_or_else(|| Line::from(String::new()));
                    ListItem::new(header)
                }
                Row::Header { name, active } => {
                    let mut spans = vec![Span::raw("  ".to_string())];
                    spans.push(
                        Span::raw(name.clone())
                            .fg(theme::text_dim())
                            .add_modifier(Modifier::BOLD),
                    );
                    if *active {
                        spans.push(Span::raw("  · active").fg(theme::accent()));
                    }
                    ListItem::new(Line::from(spans))
                }
                Row::Model { id, context, .. } => {
                    let mut line = vec![Span::raw(id.clone()).fg(theme::text())];
                    if let Some(ctx) = context {
                        line.push(
                            Span::raw(format!(" · {} ctx", fmt_scaled_number(u64::from(*ctx))))
                                .fg(theme::text_muted()),
                        );
                    }
                    render_list_item_line(Line::from(line), is_selected)
                }
                Row::Status { text } => ListItem::new(Line::from(
                    Span::raw(format!("  {text}")).fg(theme::text_muted()),
                )),
            };
            items.push(item);
        }
        if items.is_empty() {
            items.push(ListItem::new(Line::from(
                Span::raw("  no providers configured").fg(theme::text_muted()),
            )));
        }
        frame.render_widget(List::new(items), list_area);

        let mut hints = vec![("↑↓", "navigate")];
        if self
            .rows
            .first()
            .is_some_and(|row| matches!(row, Row::Custom))
        {
            hints.push(("Enter", "use input as model"));
        } else {
            hints.push(("Enter", "select"));
        }
        if self.registries.can_toggle() {
            hints.push(("Ctrl+O", "online registry"));
        }
        hints.push(("Esc", "close"));
        frame.render_widget(
            Paragraph::new(theme::help_line(&hints)).fg(theme::text_muted()),
            hint_area,
        );
    }
}

impl Default for ModelPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{InferenceProvider, Model as SeluneModel, ModelLimit};

    fn model_code(id: &str) -> selune::ModelCode {
        let (body, variant) = id.split_once(':').unwrap_or((id, ""));
        let (org, model) = body.split_once('/').unwrap_or(("test-org", body));
        let variant = (!variant.is_empty()).then_some(variant);
        selune::ModelCode::new(org, model, variant).unwrap()
    }

    fn registry_provider(id: &str, name: &str, models: &[&str]) -> Provider {
        Provider {
            name: name.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: None,
            auth: None,
            api_endpoint: None,
            doc: None,
            r#type: None,
            default_large_model_id: None,
            default_small_model_id: None,
            models: models
                .iter()
                .map(|m| SeluneModel {
                    id: (*m).to_string(),
                    model_code: model_code(m),
                    name: (*m).to_string(),
                    reasoning: false,
                    reasoning_options: Vec::new(),
                    attachment: false,
                    limit: ModelLimit {
                        context: Some(128_000),
                        input: None,
                        output: None,
                    },
                    cost: Default::default(),
                    options: None,
                })
                .collect(),
            default_headers: None,
        }
    }

    fn live_model(id: &str) -> Model {
        Model {
            id: id.to_string(),
            name: Some(id.to_string()),
            context_length: Some(4096),
            description: None,
            r#type: None,
            created_at: None,
            owned_by: None,
            max_output_tokens: None,
        }
    }

    fn connections() -> Connections {
        let mut connections = Connections::default();
        connections.providers.insert(
            "acme".into(),
            shuvarie_core::ProviderConfig::new("Acme", "openai-compat", None, None),
        );
        connections.providers.insert(
            "cat".into(),
            shuvarie_core::ProviderConfig::new("Catalog Co", "openai", None, None)
                .with_catalog(Some("catalog-co")),
        );
        connections.active = Some(shuvarie_core::Active {
            provider: "acme".into(),
            model: None,
            variant: None,
        });
        connections
    }

    /// The fixture list: Acme (active, live models only) leads, then the
    /// built-in registry's group serving Catalog Co's catalog id.
    fn picker() -> ModelPicker {
        picker_with_registries(
            RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default())
                .with_state_providers(
                    shuvarie_core::catalog::SELUNE_REGISTRY,
                    vec![registry_provider(
                        "catalog-co",
                        "Catalog Co",
                        &["co-1", "co-2"],
                    )],
                ),
        )
    }

    fn picker_with_registries(registries: RegistryManager) -> ModelPicker {
        let mut picker = ModelPicker::new();
        picker.open_with(
            ModelPicker::provider_specs(&connections()),
            HashMap::new(),
            registries,
        );
        picker
    }

    #[test]
    fn groups_are_ordered_live_first_then_by_registry() {
        let p = picker();
        assert!(matches!(p.rows[0], Row::Header { ref name, active: true } if name == "Acme"));
        assert!(
            matches!(p.rows[1], Row::Status { .. }),
            "live provider loading"
        );
        assert!(
            matches!(
                p.rows[2],
                Row::RegistryHeader {
                    ref registry
                }
                if registry == shuvarie_core::catalog::SELUNE_REGISTRY
            ),
            "the built-in registry groups catalog-backed providers"
        );
        assert!(
            matches!(p.rows[3], Row::Header { ref name, active: false } if name == "Catalog Co")
        );
        assert!(matches!(
            p.rows[4],
            Row::Model { ref id, context: Some(128_000), .. } if id == "co-1"
        ));
    }

    #[test]
    fn selection_skips_headers_and_status_rows() {
        let mut p = picker();
        assert_eq!(p.selected, 4, "starts on the first model row");
        // Walk to the end and back without landing on a header.
        for _ in 0..10 {
            p.update(ModelPickerMessage::Next);
        }
        assert!(p.rows[p.selected].selectable(), "never rests on a header");
        for _ in 0..10 {
            p.update(ModelPickerMessage::Prev);
        }
        assert_eq!(p.selected, 0, "rests at the top boundary");
    }

    #[test]
    fn live_models_arrive_via_provider_models_message() {
        let mut p = picker();
        p.update(ModelPickerMessage::ProviderModels {
            provider_name: "acme".into(),
            models: vec![live_model("acme-mini"), live_model("acme-big")],
        });
        assert!(matches!(
            p.rows[1],
            Row::Model { ref id, context: Some(4096), .. } if id == "acme-mini"
        ));
        assert!(matches!(
            p.rows[2],
            Row::Model { ref id, .. } if id == "acme-big"
        ));
        assert_eq!(p.active_default_model().as_deref(), Some("acme-mini"));
    }

    #[test]
    fn selecting_model_reports_its_provider() {
        let mut p = picker();
        // First selectable row is the active (acme) group; give it live models.
        p.update(ModelPickerMessage::ProviderModels {
            provider_name: "acme".into(),
            models: vec![live_model("acme-big")],
        });
        p.selected = 1;
        assert_eq!(
            p.update(ModelPickerMessage::Select),
            Some(ModelPickerEffect::Selected {
                provider: Some("acme".into()),
                model: "acme-big".into(),
            })
        );
        assert!(!p.open);
    }
    #[test]
    fn query_row_is_always_first_when_typing() {
        let mut p = picker();
        p.update(ModelPickerMessage::Search(SearchMessage::Input('m')));
        assert!(matches!(p.rows[0], Row::Custom));
        p.update(ModelPickerMessage::Search(SearchMessage::Backspace));
        assert!(!matches!(p.rows[0], Row::Custom));
    }

    #[test]
    fn raw_input_row_selects_query_for_active_provider() {
        let mut p = picker();
        p.update(ModelPickerMessage::Search(SearchMessage::Input('x')));
        p.selected = 0;
        assert_eq!(
            p.update(ModelPickerMessage::Select),
            Some(ModelPickerEffect::Selected {
                provider: None,
                model: "x".into(),
            })
        );
    }

    #[test]
    fn query_filters_groups_and_models() {
        let mut p = picker();
        p.update(ModelPickerMessage::ProviderModels {
            provider_name: "acme".into(),
            models: vec![live_model("acme-big"), live_model("acme-mini")],
        });
        for c in "mini".chars() {
            p.update(ModelPickerMessage::Search(SearchMessage::Input(c)));
        }
        // Custom row + Acme header + the mini match only.
        assert_eq!(p.rows.len(), 3);
        assert!(matches!(p.rows[1], Row::Header { ref name, .. } if name == "Acme"));
        assert!(matches!(p.rows[2], Row::Model { ref id, .. } if id == "acme-mini"));
    }

    #[test]
    fn toggle_source_requests_fetch_when_unloaded() {
        let mut p = picker();
        assert_eq!(
            p.update(ModelPickerMessage::ToggleSource),
            Some(ModelPickerEffect::FetchRegistries(vec![
                shuvarie_core::catalog::SELUNE_REGISTRY.to_string()
            ]))
        );
        assert!(
            p.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .online
        );
        p.update(ModelPickerMessage::RegistryLoaded {
            registry: shuvarie_core::catalog::SELUNE_REGISTRY.to_string(),
            providers: vec![registry_provider("remote-co", "Remote Co", &["r-1"])],
        });
        assert!(p.ids_needing_fetch().is_empty());
        assert_eq!(
            p.registries
                .state(shuvarie_core::catalog::SELUNE_REGISTRY)
                .unwrap()
                .providers()
                .len(),
            1,
            "the loaded snapshot replaces the offline one"
        );
    }

    #[test]
    fn foreign_registry_events_are_ignored() {
        let mut p = picker();
        assert_eq!(
            p.update(ModelPickerMessage::ToggleSource),
            Some(ModelPickerEffect::FetchRegistries(vec![
                shuvarie_core::catalog::SELUNE_REGISTRY.to_string()
            ]))
        );
        let before = p.rows.len();
        p.update(ModelPickerMessage::RegistryLoaded {
            registry: "some-custom-registry".into(),
            providers: vec![registry_provider("remote-co", "Remote Co", &["r-1"])],
        });
        assert_eq!(
            p.rows.len(),
            before,
            "another registry's fetch does not refresh this picker"
        );
        assert!(!p.rows.iter().any(|row| matches!(
            row,
            Row::Model { id, .. } if id == "r-1"
        )));
    }

    #[test]
    fn pending_live_providers_lists_uncached_custom_providers() {
        let mut p = picker();
        assert_eq!(p.pending_live_providers(), vec!["acme".to_string()]);
        p.update(ModelPickerMessage::ProviderModels {
            provider_name: "acme".into(),
            models: vec![live_model("acme-big")],
        });
        assert!(p.pending_live_providers().is_empty());
    }

    #[test]
    fn custom_registries_group_after_the_built_in_one() {
        let config = shuvarie_core::RegistriesConfig {
            custom: vec![shuvarie_core::CustomRegistry {
                name: "zeta".to_string(),
                url: Some("https://zeta.example/providers.json".to_string()),
                ..shuvarie_core::CustomRegistry::default()
            }],
            ..shuvarie_core::RegistriesConfig::default()
        };
        let manager = RegistryManager::from_config(&config)
            .with_state_providers(
                shuvarie_core::catalog::SELUNE_REGISTRY,
                vec![registry_provider("catalog-co", "Catalog Co", &["co-1"])],
            )
            .with_state_providers(
                "zeta",
                vec![registry_provider("zeta-co", "Zeta Co", &["z-1"])],
            );
        let mut picker = ModelPicker::new();
        picker.open_with(
            vec![
                ProviderSpec {
                    id: "acme".into(),
                    name: "Acme".into(),
                    catalog: None,
                    active: true,
                },
                ProviderSpec {
                    id: "cat".into(),
                    name: "Catalog Co".into(),
                    catalog: Some("catalog-co".into()),
                    active: false,
                },
                ProviderSpec {
                    id: "zeta".into(),
                    name: "Zeta Direct".into(),
                    catalog: Some("zeta-co".into()),
                    active: false,
                },
            ],
            HashMap::new(),
            manager,
        );
        // Live acme leads, then the built-in registry, then the custom one —
        // registry order, matching the provider selector.
        assert!(matches!(picker.rows[0], Row::Header { ref name, active: true } if name == "Acme"));
        assert!(matches!(
            picker.rows[2],
            Row::RegistryHeader { ref registry } if registry == shuvarie_core::catalog::SELUNE_REGISTRY
        ));
        assert!(matches!(picker.rows[3], Row::Header { ref name, .. } if name == "Catalog Co"));
        assert!(matches!(picker.rows[4], Row::Model { ref id, .. } if id == "co-1"));
        assert!(
            matches!(picker.rows[5], Row::RegistryHeader { ref registry } if registry == "zeta"),
            "the custom registry groups its own providers"
        );
        assert!(matches!(picker.rows[6], Row::Header { ref name, .. } if name == "Zeta Direct"));
        assert!(matches!(picker.rows[7], Row::Model { ref id, .. } if id == "z-1"));
    }

    #[test]
    fn catalog_specs_migrate_into_their_registry_after_the_fetch() {
        // Unseeded: the catalog id resolves nowhere, so while the fetch is
        // pending the spec shows the registry loading row instead of falling
        // through to live data.
        let mut p = picker_with_registries(RegistryManager::from_config(
            &shuvarie_core::RegistriesConfig::default(),
        ));
        p.update(ModelPickerMessage::ToggleSource);
        assert!(
            p.rows.iter().any(|row| matches!(
                row,
                Row::Status { text } if text == "loading registry…"
            )),
            "{:?}",
            p.rows
        );
        // Once loaded the spec resolves and takes its place in the group.
        p.update(ModelPickerMessage::RegistryLoaded {
            registry: shuvarie_core::catalog::SELUNE_REGISTRY.to_string(),
            providers: vec![registry_provider("catalog-co", "Catalog Co", &["co-1"])],
        });
        assert!(matches!(
            p.rows[2],
            Row::RegistryHeader { ref registry } if registry == shuvarie_core::catalog::SELUNE_REGISTRY
        ));
        assert!(matches!(p.rows[4], Row::Model { ref id, .. } if id == "co-1"));
        assert!(!p.rows.iter().any(|row| matches!(
            row,
            Row::Status { text } if text == "loading registry…"
        )));
    }

    #[test]
    fn no_providers_shows_placeholder_row() {
        let mut p = ModelPicker::new();
        p.open_with(
            Vec::new(),
            HashMap::new(),
            RegistryManager::from_config(&shuvarie_core::RegistriesConfig::default()),
        );
        assert!(p.rows.is_empty());
        assert_eq!(p.active_default_model(), None);
    }
}
