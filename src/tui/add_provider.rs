use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, List, ListItem, Paragraph, Wrap};
use selune::Provider;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::{alt, ctrl};

use super::components::InputBuffer;
use super::list::{render_list_item, scroll_offset_for};
use super::search::{Search, SearchMessage};
use super::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AddProviderStage {
    SelectKind,
    Details,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Name,
    BaseUrl,
    ApiKey,
}

pub enum AddProviderMessage {
    NextKind,
    PrevKind,
    SelectKind,
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

pub enum AddProviderOutcome {
    None,
    Cancel,
    Submit {
        kind: String,
        catalog: Option<String>,
        name: String,
        api_key: Option<String>,
        base_url: Option<String>,
    },
}

pub struct AddProviderForm {
    pub stage: AddProviderStage,
    pub kind_selected: usize,
    pub kind_offset: usize,
    kind_viewport_height: u16,
    pub search: Search,
    pub filtered: Vec<usize>,
    providers: Vec<Provider>,
    pub name: InputBuffer,
    pub api_key: InputBuffer,
    pub base_url: InputBuffer,
    pub field: FormField,
    pub error: Option<String>,
    pub existing_names: Vec<String>,
}

impl AddProviderForm {
    pub fn new(providers: Vec<Provider>, existing_names: &[String]) -> Self {
        Self {
            stage: AddProviderStage::SelectKind,
            kind_selected: 0,
            kind_offset: 0,
            kind_viewport_height: 0,
            search: Search::new(),
            filtered: (0..providers.len()).collect(),
            providers,
            name: InputBuffer::new(),
            api_key: InputBuffer::new(),
            base_url: InputBuffer::new(),
            field: FormField::Name,
            error: None,
            existing_names: existing_names.to_vec(),
        }
    }

    fn kind(&self) -> &Provider {
        let idx = self
            .filtered
            .get(self.kind_selected)
            .copied()
            .unwrap_or(0)
            .min(self.providers.len().saturating_sub(1));
        &self.providers[idx]
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.providers.len(), |i| self.providers[i].name.clone());
        self.kind_selected = 0;
        self.kind_offset = 0;
        self.recompute_kind_offset();
    }

    fn recompute_kind_offset(&mut self) {
        let vh = self.kind_viewport_height as usize;
        let len = self.filtered.len();
        self.kind_offset = scroll_offset_for(self.kind_selected, self.kind_offset, vh, len);
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

    fn transition_to_details(&mut self) {
        let default_name = self.compute_default_name(&self.kind().name);
        self.name.set(&default_name);
        self.base_url.clear();
        if let Some(url) =
            shuvarie_core::catalog::api_endpoint(self.kind()).filter(|u| !u.contains('$'))
        {
            self.base_url.set(&url);
        }
        self.api_key.clear();
        self.stage = AddProviderStage::Details;
        self.field = FormField::Name;
        self.error = None;
    }

    fn next_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::BaseUrl,
            FormField::BaseUrl => FormField::ApiKey,
            FormField::ApiKey => FormField::Name,
        };
    }

    fn prev_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::ApiKey,
            FormField::BaseUrl => FormField::Name,
            FormField::ApiKey => FormField::BaseUrl,
        };
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AddProviderMessage> {
        match self.stage {
            AddProviderStage::SelectKind => {
                if ctrl(key) {
                    return match key.code {
                        KeyCode::Char('n') => Some(AddProviderMessage::NextKind),
                        KeyCode::Char('p') => Some(AddProviderMessage::PrevKind),
                        _ => None,
                    };
                }
                match key.code {
                    KeyCode::Escape => Some(AddProviderMessage::Cancel),
                    KeyCode::Down | KeyCode::Char('j') => Some(AddProviderMessage::NextKind),
                    KeyCode::Up | KeyCode::Char('k') => Some(AddProviderMessage::PrevKind),
                    KeyCode::Enter => Some(AddProviderMessage::SelectKind),
                    KeyCode::Backspace => {
                        Some(AddProviderMessage::Search(SearchMessage::Backspace))
                    }
                    KeyCode::Char(c) => Some(AddProviderMessage::Search(SearchMessage::Input(c))),
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
            AddProviderMessage::Cancel => {
                if self.stage == AddProviderStage::Details {
                    self.stage = AddProviderStage::SelectKind;
                    self.error = None;
                    AddProviderOutcome::None
                } else {
                    AddProviderOutcome::Cancel
                }
            }
            AddProviderMessage::NextKind => {
                let len = self.filtered.len();
                if len > 0 {
                    self.kind_selected = (self.kind_selected + 1).min(len - 1);
                    self.recompute_kind_offset();
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::PrevKind => {
                self.kind_selected = self.kind_selected.saturating_sub(1);
                self.recompute_kind_offset();
                AddProviderOutcome::None
            }
            AddProviderMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
                AddProviderOutcome::None
            }
            AddProviderMessage::SelectKind => {
                if !self.filtered.is_empty() {
                    self.transition_to_details();
                }
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
                match self.field {
                    FormField::Name => self.name.push(c),
                    FormField::BaseUrl => self.base_url.push(c),
                    FormField::ApiKey => self.api_key.push(c),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Paste(text) => {
                let flat = super::components::flatten_newlines(&text);
                match self.stage {
                    AddProviderStage::SelectKind => {
                        for c in flat.chars() {
                            self.search.update(SearchMessage::Input(c));
                        }
                        self.refilter();
                    }
                    AddProviderStage::Details => match self.field {
                        FormField::Name => self.name.insert_str(&flat),
                        FormField::BaseUrl => self.base_url.insert_str(&flat),
                        FormField::ApiKey => self.api_key.insert_str(&flat),
                    },
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Backspace => {
                match self.field {
                    FormField::Name => self.name.backspace(),
                    FormField::BaseUrl => self.base_url.backspace(),
                    FormField::ApiKey => self.api_key.backspace(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Delete => {
                match self.field {
                    FormField::Name => self.name.delete(),
                    FormField::BaseUrl => self.base_url.delete(),
                    FormField::ApiKey => self.api_key.delete(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::KillToEnd => {
                match self.field {
                    FormField::Name => self.name.kill_to_end(),
                    FormField::BaseUrl => self.base_url.kill_to_end(),
                    FormField::ApiKey => self.api_key.kill_to_end(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Left => {
                match self.field {
                    FormField::Name => self.name.left(),
                    FormField::BaseUrl => self.base_url.left(),
                    FormField::ApiKey => self.api_key.left(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Right => {
                match self.field {
                    FormField::Name => self.name.right(),
                    FormField::BaseUrl => self.base_url.right(),
                    FormField::ApiKey => self.api_key.right(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::LeftWord => {
                match self.field {
                    FormField::Name => self.name.left_word(),
                    FormField::BaseUrl => self.base_url.left_word(),
                    FormField::ApiKey => self.api_key.left_word(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::RightWord => {
                match self.field {
                    FormField::Name => self.name.right_word(),
                    FormField::BaseUrl => self.base_url.right_word(),
                    FormField::ApiKey => self.api_key.right_word(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Home => {
                match self.field {
                    FormField::Name => self.name.home(),
                    FormField::BaseUrl => self.base_url.home(),
                    FormField::ApiKey => self.api_key.home(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::End => {
                match self.field {
                    FormField::Name => self.name.end(),
                    FormField::BaseUrl => self.base_url.end(),
                    FormField::ApiKey => self.api_key.end(),
                }
                AddProviderOutcome::None
            }
            AddProviderMessage::Submit => self.submit(),
            AddProviderMessage::Resize { viewport_height } => {
                if self.kind_viewport_height != viewport_height {
                    self.kind_viewport_height = viewport_height;
                    self.recompute_kind_offset();
                }
                AddProviderOutcome::None
            }
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
        let kind = self.kind();
        let kind_name = kind.name.clone();
        let requires_key = shuvarie_core::catalog::requires_api_key(kind);
        let api_key = if requires_key {
            let k = self.api_key.value.trim().to_string();
            if k.is_empty() {
                self.error = Some(format!("{kind_name} requires an API key"));
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
        let catalog = kind.id.0.clone();
        let transport = kind
            .r#type
            .map(|t| shuvarie_core::catalog::provider_type_name(t).to_string())
            .unwrap_or_else(|| catalog.clone());
        AddProviderOutcome::Submit {
            kind: transport,
            catalog: Some(catalog),
            name,
            api_key,
            base_url,
        }
    }

    fn field_line(
        &self,
        label: &str,
        buffer: &InputBuffer,
        field: FormField,
        suffix: &str,
    ) -> Line<'static> {
        let label_style = Style::new().fg(theme::TEXT_DIM);
        let hint_style = Style::new().fg(theme::TEXT_MUTED);
        let active_style = Style::new().fg(theme::ACCENT);
        let inactive_style = Style::new().fg(theme::TEXT_DIM);
        let cursor_style = Style::new()
            .fg(theme::ACCENT)
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
            AddProviderStage::SelectKind => self.view_select_kind(frame, area),
            AddProviderStage::Details => self.view_details(frame, area),
        }
    }

    fn view_select_kind(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(50, 55, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Add Provider");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [heading_area, search_area, list_area, hint_area] =
            Layout::vertical([Length(2), Length(1), Min(0), Length(1)]).areas(inner);

        frame.render_widget(
            Paragraph::new("Select a provider:").fg(theme::TEXT),
            heading_area,
        );

        self.search
            .view(frame, search_area, "Type to filter providers");

        let offset = scroll_offset_for(
            self.kind_selected,
            self.kind_offset,
            list_area.height as usize,
            self.filtered.len(),
        );
        let visible: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_area.height as usize)
            .map(|(i, &orig)| {
                render_list_item(self.providers[orig].name.clone(), i == self.kind_selected)
            })
            .collect();
        frame.render_widget(List::new(visible), list_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Type", "to filter"),
                ("↑↓", "navigate"),
                ("Enter", "continue"),
                ("Esc", "cancel"),
            ]))
            .fg(theme::TEXT_MUTED),
            hint_area,
        );
    }

    fn view_details(&self, frame: &mut Frame<'_>, area: Rect) {
        let popup = centered_rect(60, 55, area);
        frame.render_widget(Clear, popup);
        let kind = self.kind();
        let title = format!("Add Provider — {}", kind.name);
        let block = theme::overlay_block(&title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let needs_key = shuvarie_core::catalog::requires_api_key(kind);

        let name_line = self.field_line("Name", &self.name, FormField::Name, "");
        let url_line = self.field_line("Base URL", &self.base_url, FormField::BaseUrl, "");
        let key_suffix = if needs_key { "" } else { "  (not required)" };
        let key_line = self.field_line("API key", &self.api_key, FormField::ApiKey, key_suffix);

        let lines = vec![name_line, url_line, key_line];
        let body = Paragraph::new(lines).wrap(Wrap { trim: false });
        let [body_area, error_area, hint_area] =
            Layout::vertical([Min(0), Length(2), Length(1)]).areas(inner);
        frame.render_widget(body, body_area);
        if let Some(e) = &self.error {
            frame.render_widget(Paragraph::new(e.as_str()).fg(theme::ERROR), error_area);
        }
        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Tab", "next"),
                ("Enter", "submit"),
                ("Esc", "back"),
            ]))
            .fg(theme::TEXT_MUTED),
            hint_area,
        );
    }
}

impl Default for AddProviderForm {
    fn default() -> Self {
        Self::new(Vec::new(), &[])
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

    fn test_providers() -> Vec<Provider> {
        vec![
            Provider {
                name: "OpenAI".into(),
                id: selune::InferenceProvider("openai".into()),
                api_key: Some("$OPENAI_API_KEY".into()),
                api_endpoint: Some("https://api.openai.com/v1".into()),
                doc: None,
                r#type: Some(selune::ProviderType::Openai),
                default_large_model_id: None,
                default_small_model_id: None,
                models: Vec::new(),
                default_headers: None,
            },
            Provider {
                name: "Ollama".into(),
                id: selune::InferenceProvider("ollama".into()),
                api_key: None,
                api_endpoint: Some("http://localhost:11434".into()),
                doc: None,
                r#type: Some(selune::ProviderType::Ollama),
                default_large_model_id: None,
                default_small_model_id: None,
                models: Vec::new(),
                default_headers: None,
            },
        ]
    }

    fn form_with_query(query: &str) -> AddProviderForm {
        let mut form = AddProviderForm::new(test_providers(), &[]);
        if !query.is_empty() {
            form.search.query = query.into();
            form.refilter();
        }
        form
    }

    #[test]
    fn empty_query_lists_all_kinds() {
        let form = form_with_query("");
        assert_eq!(form.filtered.len(), 2);
        assert_eq!(form.filtered, vec![0, 1]);
    }

    #[test]
    fn query_filters_kinds() {
        let form = form_with_query("open");
        assert!(!form.filtered.is_empty());
        assert!(
            form.filtered
                .iter()
                .all(|&i| { form.providers[i].name.to_lowercase().contains("open") })
        );
    }

    #[test]
    fn no_match_query_yields_empty_filter() {
        let form = form_with_query("zzzzzz");
        assert!(form.filtered.is_empty());
    }

    #[test]
    fn kind_resolves_through_filtered() {
        let mut form = form_with_query("ollama");
        form.kind_selected = 0;
        assert_eq!(form.kind().id.0, "ollama");
    }

    #[test]
    fn kind_falls_back_when_filtered_empty() {
        let form = form_with_query("zzzzzz");
        assert_eq!(form.kind().id.0, "openai");
    }

    #[test]
    fn search_update_refilters_and_resets_selection() {
        let mut form = AddProviderForm::new(test_providers(), &[]);
        form.kind_selected = 1;
        form.update(AddProviderMessage::Search(SearchMessage::Input('o')));
        assert!(!form.filtered.is_empty());
        assert_eq!(form.kind_selected, 0);
    }
}
