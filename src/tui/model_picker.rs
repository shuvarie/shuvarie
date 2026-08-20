use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use shuvarie_core::ModelInfo;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::add_provider::centered_rect;
use super::list::{render_list_item_line, scroll_offset_for};
use super::search::{Search, SearchMessage};
use super::theme;

pub enum ModelPickerMessage {
    Search(SearchMessage),
    Next,
    Prev,
    Select,
    Close,
    Resize { viewport_height: u16 },
}

pub enum ModelPickerEffect {
    Selected { model: String },
    Close,
}

pub struct ModelPicker {
    pub open: bool,
    pub models: Vec<ModelInfo>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub search: Search,
}

impl ModelPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            models: Vec::new(),
            filtered: Vec::new(),
            selected: 0,
            offset: 0,
            viewport_height: 0,
            search: Search::new(),
        }
    }

    pub fn open(&mut self, models: &[ModelInfo]) {
        self.open = true;
        self.models = models.to_vec();
        self.search.clear();
        self.search.active = true;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.models.len(), |i| self.models[i].id.clone());
        self.selected = 0;
        self.offset = 0;
        self.recompute_offset();
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.filtered.len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ModelPickerMessage> {
        if ctrl(key) {
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
                    self.selected = (self.selected + 1).min(self.filtered.len() - 1);
                    self.recompute_offset();
                }
                None
            }
            ModelPickerMessage::Prev => {
                if !self.filtered.is_empty() {
                    self.selected = self.selected.saturating_sub(1);
                    self.recompute_offset();
                }
                None
            }
            ModelPickerMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
                None
            }
            ModelPickerMessage::Select => {
                if let Some(&orig) = self.filtered.get(self.selected) {
                    let model = self.models[orig].id.clone();
                    self.close();
                    return Some(ModelPickerEffect::Selected { model });
                }
                None
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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
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

        let visible: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(list_area.height as usize)
            .map(|(idx, &orig)| {
                let m = &self.models[orig];
                let mut line = vec![Span::raw(m.display_name().to_string()).fg(theme::TEXT)];
                if let Some(ctx) = m.context_length {
                    line.push(Span::raw(format!(" · {}k ctx", ctx / 1024)).fg(theme::TEXT_MUTED));
                }
                render_list_item_line(Line::from(line), idx == self.selected)
            })
            .collect();
        frame.render_widget(List::new(visible), list_area);

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
