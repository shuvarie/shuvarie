use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use shuvarie_core::Model;
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

#[derive(Debug, PartialEq)]
pub enum ModelPickerEffect {
    Selected { model: String },
    Close,
}

pub struct ModelPicker {
    pub open: bool,
    pub models: Vec<Model>,
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

    pub fn open(&mut self, models: &[Model]) {
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

    /// Whether the raw search query is offered as a selectable first row.
    /// Enabled only when a query is typed and no model matches it.
    fn query_row(&self) -> Option<String> {
        let q = self.search.query.trim();
        if q.is_empty() || !self.filtered.is_empty() {
            return None;
        }
        Some(q.to_string())
    }

    /// Total number of visible rows (the query row, when present, plus the
    /// matched models).
    fn visible_len(&self) -> usize {
        self.filtered.len() + usize::from(self.query_row().is_some())
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
        let len = self.visible_len();
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
                let len = self.visible_len();
                if len > 0 {
                    self.selected = (self.selected + 1).min(len - 1);
                    self.recompute_offset();
                }
                None
            }
            ModelPickerMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset();
                None
            }
            ModelPickerMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
                None
            }
            ModelPickerMessage::Select => {
                if let Some(model) = self.selected_model() {
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

    /// The model id of the currently selected row, if any. Row 0 is the raw
    /// query when the query row is active; otherwise rows map into `filtered`.
    fn selected_model(&self) -> Option<String> {
        if self.query_row().is_some() {
            if self.selected == 0 {
                return self.query_row();
            }
            return self
                .filtered
                .get(self.selected - 1)
                .map(|&orig| self.models[orig].id.clone());
        }
        self.filtered
            .get(self.selected)
            .map(|&orig| self.models[orig].id.clone())
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

        let mut items: Vec<ListItem> = Vec::new();
        let query_row_present = self.query_row().is_some();
        if let Some(q) = self.query_row() {
            let mut line = vec![Span::raw(format!("Use \"{q}\""))
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)];
            line.push(Span::raw("  (no match)").fg(theme::TEXT_MUTED));
            items.push(render_list_item_line(Line::from(line), self.selected == 0));
        }
        let skip = self.offset.saturating_sub(usize::from(query_row_present));
        let row_base = self.selected.saturating_sub(usize::from(query_row_present));
        for (idx, &orig) in self.filtered.iter().enumerate().skip(skip) {
            let m = &self.models[orig];
            let mut line =
                vec![Span::raw(shuvarie_llm::display_name(m).to_string()).fg(theme::TEXT)];
            if let Some(ctx) = m.context_length {
                line.push(Span::raw(format!(" · {}k ctx", ctx / 1024)).fg(theme::TEXT_MUTED));
            }
            items.push(render_list_item_line(Line::from(line), idx == row_base));
        }
        frame.render_widget(List::new(items), list_area);

        let hint = if self.query_row().is_some() {
            theme::help_line(&[("Enter", "use query"), ("Esc", "close"), ("↑↓", "navigate")])
        } else {
            theme::help_line(&[("Enter", "select"), ("Esc", "close"), ("↑↓", "navigate")])
        };
        frame.render_widget(Paragraph::new(hint).fg(theme::TEXT_MUTED), hint_area);
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

    fn models() -> Vec<Model> {
        vec![
            Model {
                id: "gpt-5".into(),
                name: Some("GPT-5".into()),
                context_length: Some(400000),
                description: None,
                r#type: None,
                created_at: None,
                owned_by: None,
                max_output_tokens: None,
            },
            Model {
                id: "claude-sonnet-4-5".into(),
                name: Some("Claude Sonnet 4.5".into()),
                context_length: Some(200000),
                description: None,
                r#type: None,
                created_at: None,
                owned_by: None,
                max_output_tokens: None,
            },
        ]
    }

    fn picker_with_query(q: &str) -> ModelPicker {
        let mut p = ModelPicker::new();
        p.open(&models());
        p.search.query = q.into();
        p.refilter();
        p
    }

    #[test]
    fn empty_query_lists_all() {
        let p = picker_with_query("");
        assert_eq!(p.filtered.len(), 2);
        assert_eq!(p.query_row(), None);
    }

    #[test]
    fn match_query_has_no_query_row() {
        let p = picker_with_query("gpt");
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.query_row(), None);
    }

    #[test]
    fn no_match_query_shows_query_row() {
        let p = picker_with_query("my-custom-model");
        assert!(p.filtered.is_empty());
        assert_eq!(p.query_row(), Some("my-custom-model".to_string()));
    }

    #[test]
    fn select_query_row_returns_raw_query() {
        let mut p = picker_with_query("custom-xyz");
        p.selected = 0;
        assert_eq!(
            p.update(ModelPickerMessage::Select),
            Some(ModelPickerEffect::Selected {
                model: "custom-xyz".into()
            })
        );
    }

    #[test]
    fn visible_len_counts_query_row() {
        let p = picker_with_query("zzz");
        assert_eq!(p.visible_len(), 1);
    }
}
