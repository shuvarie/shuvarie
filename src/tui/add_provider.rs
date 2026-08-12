use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph, Wrap};
use shuvarie_llm::Provider;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::theme;
use super::widgets::InputBuffer;

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
    Input(char),
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
}

pub enum AddProviderOutcome {
    None,
    Cancel,
    Submit {
        kind: Provider,
        name: String,
        api_key: Option<String>,
        base_url: Option<String>,
    },
}

pub struct AddProviderForm {
    pub stage: AddProviderStage,
    pub kind_state: ListState,
    pub name: InputBuffer,
    pub api_key: InputBuffer,
    pub base_url: InputBuffer,
    pub field: FormField,
    pub error: Option<String>,
    pub existing_names: Vec<String>,
}

impl AddProviderForm {
    pub fn new(existing_names: &[String]) -> Self {
        let mut kind_state = ListState::default();
        kind_state.select(Some(0));
        Self {
            stage: AddProviderStage::SelectKind,
            kind_state,
            name: InputBuffer::new(),
            api_key: InputBuffer::new(),
            base_url: InputBuffer::new(),
            field: FormField::Name,
            error: None,
            existing_names: existing_names.to_vec(),
        }
    }

    fn kind(&self) -> Provider {
        Provider::ALL[self.kind_state.selected().unwrap_or(0)]
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
        let kind = self.kind();
        let default_name = self.compute_default_name(kind.display_name());
        self.name.set(&default_name);
        self.base_url.clear();
        if let Some(url) = kind.default_base_url() {
            self.base_url.set(url);
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

    pub fn handle_event(key: KeyEvent, stage: AddProviderStage) -> Option<AddProviderMessage> {
        match stage {
            AddProviderStage::SelectKind => {
                if ctrl(&key) {
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
                    _ => None,
                }
            }
            AddProviderStage::Details => {
                if ctrl(&key) {
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
                if alt(&key) {
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
                let len = Provider::ALL.len();
                let i = self.kind_state.selected().unwrap_or(0);
                self.kind_state.select(Some((i + 1).min(len - 1)));
                AddProviderOutcome::None
            }
            AddProviderMessage::PrevKind => {
                let i = self.kind_state.selected().unwrap_or(0);
                self.kind_state.select(Some(i.saturating_sub(1)));
                AddProviderOutcome::None
            }
            AddProviderMessage::SelectKind => {
                self.transition_to_details();
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
        let api_key = if kind.requires_api_key() {
            let k = self.api_key.value.trim().to_string();
            if k.is_empty() {
                self.error = Some(format!("{} requires an API key", kind.display_name()));
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
        AddProviderOutcome::Submit {
            kind,
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

        let [heading_area, list_area, hint_area] =
            Layout::vertical([Length(2), Min(0), Length(1)]).areas(inner);

        frame.render_widget(
            Paragraph::new("Select a provider:").fg(theme::TEXT),
            heading_area,
        );

        let items: Vec<ListItem> = Provider::ALL
            .iter()
            .map(|&p| ListItem::new(p.display_name()).fg(theme::TEXT))
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(theme::ACCENT_BG).fg(theme::TEXT))
            .highlight_symbol("▶ ");
        let mut state = self.kind_state;
        frame.render_stateful_widget(list, list_area, &mut state);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
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
        let title = format!("Add Provider — {}", kind.display_name());
        let block = theme::overlay_block(&title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let needs_key = kind.requires_api_key();

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
        Self::new(&[])
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::ALT)
}

pub fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}
