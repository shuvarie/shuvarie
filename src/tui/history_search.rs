use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use shuvarie_core::SearchHit;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::add_provider::centered_rect;
use super::components::InputBuffer;
use super::list::{render_list_item_line, scroll_offset_for};
use super::theme;
pub enum HistorySearchMessage {
    Open,
    Input(char),
    Backspace,
    Next,
    Prev,
    Submit,
    Close,
    Resize { viewport_height: u16 },
    Results { hits: Vec<SearchHit> },
    Error { error: String },
}

pub enum HistorySearchEffect {
    Open,
    Search { query: String },
    LoadSession { id: uuid::Uuid },
    Close,
}

pub struct HistorySearch {
    pub open: bool,
    pub query: InputBuffer,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub loading: bool,
    pub error: Option<String>,
}

impl HistorySearch {
    pub fn new() -> Self {
        Self {
            open: false,
            query: InputBuffer::new(),
            hits: Vec::new(),
            selected: 0,
            offset: 0,
            viewport_height: 0,
            loading: false,
            error: None,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.query.clear();
        self.hits.clear();
        self.selected = 0;
        self.offset = 0;
        self.loading = false;
        self.error = None;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.hits.clear();
        self.loading = false;
        self.error = None;
    }

    fn set_hits(&mut self, hits: Vec<SearchHit>) {
        self.hits = hits;
        self.loading = false;
        self.error = None;
        self.selected = 0;
        self.recompute_offset();
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.hits.len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    fn next(&mut self) {
        if !self.hits.is_empty() {
            self.selected = (self.selected + 1).min(self.hits.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.hits.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<HistorySearchMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(HistorySearchMessage::Next),
                KeyCode::Char('p') => Some(HistorySearchMessage::Prev),
                KeyCode::Char('b') => Some(HistorySearchMessage::Backspace),
                KeyCode::Char('h') => Some(HistorySearchMessage::Backspace),
                KeyCode::Char('r') => Some(HistorySearchMessage::Submit),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(HistorySearchMessage::Close),
            KeyCode::Down => Some(HistorySearchMessage::Next),
            KeyCode::Up => Some(HistorySearchMessage::Prev),
            KeyCode::Enter => Some(HistorySearchMessage::Submit),
            KeyCode::Backspace => Some(HistorySearchMessage::Backspace),
            KeyCode::Char(c) => Some(HistorySearchMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: HistorySearchMessage) -> Option<HistorySearchEffect> {
        if !self.open
            && !matches!(
                msg,
                HistorySearchMessage::Open | HistorySearchMessage::Close
            )
        {
            return None;
        }
        match msg {
            HistorySearchMessage::Open => {
                self.open();
                Some(HistorySearchEffect::Open)
            }
            HistorySearchMessage::Close => {
                self.close();
                Some(HistorySearchEffect::Close)
            }
            HistorySearchMessage::Input(c) => {
                self.query.push(c);
                self.loading = true;
                Some(HistorySearchEffect::Search {
                    query: self.query.value.clone(),
                })
            }
            HistorySearchMessage::Backspace => {
                self.query.backspace();
                self.loading = true;
                Some(HistorySearchEffect::Search {
                    query: self.query.value.clone(),
                })
            }
            HistorySearchMessage::Next => {
                self.next();
                None
            }
            HistorySearchMessage::Prev => {
                self.prev();
                None
            }
            HistorySearchMessage::Submit => {
                if self.loading || self.hits.is_empty() {
                    return None;
                }
                if let Some(hit) = self.hits.get(self.selected) {
                    let id = hit.session_id;
                    self.close();
                    Some(HistorySearchEffect::LoadSession { id })
                } else {
                    None
                }
            }
            HistorySearchMessage::Results { hits } => {
                self.set_hits(hits);
                None
            }
            HistorySearchMessage::Error { error } => {
                self.loading = false;
                self.error = Some(error);
                self.hits.clear();
                self.selected = 0;
                self.recompute_offset();
                None
            }
            HistorySearchMessage::Resize { viewport_height } => {
                if self.viewport_height != viewport_height {
                    self.viewport_height = viewport_height;
                    self.recompute_offset();
                }
                None
            }
        }
    }

    fn snippet(content: &str) -> String {
        let compact = content.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.chars().count() > 160 {
            let mut s: String = compact.chars().take(157).collect();
            s.push('…');
            s
        } else {
            compact
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(64, 40, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Search history");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [input_area, status_area, list_area, hint_area] =
            Layout::vertical([Length(1), Length(1), Min(0), Length(1)]).areas(inner);

        let input_line = self.query.cursor_line(theme::TEXT, theme::ACCENT);
        let mut input_spans = vec![Span::raw("/ ").fg(theme::TEXT_MUTED)];
        input_spans.extend(input_line.spans);
        frame.render_widget(Paragraph::new(Line::from(input_spans)), input_area);

        let status = if let Some(err) = &self.error {
            Paragraph::new(err.as_str()).fg(theme::ERROR)
        } else if self.loading {
            Paragraph::new(Line::from(vec![
                super::spinner::spinner(),
                Span::raw(" Searching...").fg(theme::TEXT_MUTED),
            ]))
        } else if self.query.value.is_empty() {
            Paragraph::new("type to search all sessions").fg(theme::TEXT_MUTED)
        } else if self.hits.is_empty() {
            Paragraph::new("no matches").fg(theme::TEXT_MUTED)
        } else {
            Paragraph::new(format!(
                "{} matches · ⌕ semantic results are merged as they arrive",
                self.hits.len()
            ))
            .fg(theme::TEXT_MUTED)
        };
        frame.render_widget(status, status_area);

        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            list_area.height as usize,
            self.hits.len(),
        );
        let visible: Vec<ListItem> = self
            .hits
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_area.height as usize)
            .map(|(idx, hit)| {
                let role_tag = match hit.role {
                    shuvarie_core::MsgRole::System => "sys",
                    shuvarie_core::MsgRole::User => "user",
                    shuvarie_core::MsgRole::Assistant => "asst",
                };
                let source_tag = match hit.source {
                    shuvarie_core::SearchSource::Semantic => "⌕ ",
                    shuvarie_core::SearchSource::Fts => "",
                };
                let line = Line::from(vec![
                    Span::raw(format!("{} · ", hit.session_title)).fg(theme::ACCENT),
                    Span::raw(role_tag).fg(theme::TEXT_MUTED),
                    Span::raw("  "),
                    Span::raw(source_tag).fg(theme::ACCENT),
                    Span::raw(Self::snippet(&hit.content)).fg(theme::TEXT),
                    Span::raw(format!("  {:.2}", hit.score)).fg(theme::TEXT_MUTED),
                ]);
                render_list_item_line(line, idx == self.selected)
            })
            .collect();
        frame.render_widget(List::new(visible), list_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Type", "to search"),
                ("↑↓", "navigate"),
                ("Enter", "open session"),
                ("Esc", "close"),
            ]))
            .fg(theme::TEXT_MUTED),
            hint_area,
        );
    }
}

impl Default for HistorySearch {
    fn default() -> Self {
        Self::new()
    }
}
