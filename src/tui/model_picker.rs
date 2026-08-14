use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use shuvarie_core::ModelInfo;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::add_provider::centered_rect;
use super::search::{Search, SearchMessage};
use super::theme;

pub enum ModelPickerMessage {
    Search(SearchMessage),
    Next,
    Prev,
    Select,
    Close,
}

pub enum ModelPickerEffect {
    Selected { model: String },
    Close,
}

pub struct ModelPicker {
    pub open: bool,
    pub models: Vec<ModelInfo>,
    pub filtered: Vec<usize>,
    pub state: ListState,
    pub search: Search,
}

impl ModelPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            models: Vec::new(),
            filtered: Vec::new(),
            state: ListState::default(),
            search: Search::new(),
        }
    }

    pub fn open(&mut self, models: &[ModelInfo]) {
        self.open = true;
        self.models = models.to_vec();
        self.search.clear();
        self.search.active = true;
        self.refilter();
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.models.len(), |i| self.models[i].id.clone());
        if !self.filtered.is_empty() {
            self.state.select(Some(0));
        } else {
            self.state.select(None);
        }
    }

    pub fn handle_event(key: KeyEvent) -> Option<ModelPickerMessage> {
        if ctrl(&key) {
            return match key.code {
                KeyCode::Char('n') => Some(ModelPickerMessage::Next),
                KeyCode::Char('p') => Some(ModelPickerMessage::Prev),
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
            ModelPickerMessage::Next => {
                if !self.filtered.is_empty() {
                    let i = self.state.selected().unwrap_or(0);
                    let next = (i + 1).min(self.filtered.len() - 1);
                    self.state.select(Some(next));
                }
                None
            }
            ModelPickerMessage::Prev => {
                if !self.filtered.is_empty() {
                    let i = self.state.selected().unwrap_or(0);
                    let prev = i.saturating_sub(1);
                    self.state.select(Some(prev));
                }
                None
            }
            ModelPickerMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
                None
            }
            ModelPickerMessage::Select => {
                if let Some(idx) = self.state.selected()
                    && let Some(&orig) = self.filtered.get(idx)
                {
                    let model = self.models[orig].id.clone();
                    self.close();
                    return Some(ModelPickerEffect::Selected { model });
                }
                None
            }
        }
    }

    pub fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(55, 50, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Select Model");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [input_area, list_area, hint_area] =
            Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

        self.search.view(frame, input_area, "/ to search models");

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&orig| {
                let m = &self.models[orig];
                ListItem::new(m.display_name().to_string()).fg(theme::TEXT)
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(theme::ACCENT_BG).fg(theme::TEXT))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, list_area, &mut self.state);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Enter", "select"),
                ("Esc", "close"),
                ("↑↓", "navigate"),
            ]))
            .fg(theme::TEXT_MUTED),
            hint_area,
        );
    }
}

impl Default for ModelPicker {
    fn default() -> Self {
        Self::new()
    }
}
