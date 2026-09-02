use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use shuvarie_core::SessionSummary;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::add_provider::centered_rect;
use super::list::{render_list_item, scroll_offset_for};
use super::theme;

pub enum SessionPickerMessage {
    Next,
    Prev,
    Select,
    ToggleDelete,
    CancelDelete,
    NewSession,
    Close,
    Resize { viewport_height: u16 },
}

pub enum SessionPickerEffect {
    LoadSession { id: uuid::Uuid },
    DeleteSession { id: uuid::Uuid },
    NewSession,
    Close,
}

pub struct SessionPicker {
    pub open: bool,
    pub sessions: Vec<SessionSummary>,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub confirm_delete: bool,
    pub active_id: Option<uuid::Uuid>,
    pub loading: bool,
}

impl SessionPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            sessions: Vec::new(),
            selected: 0,
            offset: 0,
            viewport_height: 0,
            confirm_delete: false,
            active_id: None,
            loading: false,
        }
    }

    pub fn open(&mut self, active_id: Option<uuid::Uuid>) {
        self.open = true;
        self.sessions.clear();
        self.selected = 0;
        self.offset = 0;
        self.confirm_delete = false;
        self.active_id = active_id;
        self.loading = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.confirm_delete = false;
        self.loading = false;
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
        self.loading = false;
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        self.recompute_offset();
    }

    pub fn session_after(&self, id: uuid::Uuid) -> Option<uuid::Uuid> {
        let idx = self.sessions.iter().position(|s| s.id == id)?;
        let next = idx + 1;
        if next < self.sessions.len() {
            Some(self.sessions[next].id)
        } else if idx > 0 {
            Some(self.sessions[idx - 1].id)
        } else {
            None
        }
    }

    fn next(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1).min(self.sessions.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.sessions.len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SessionPickerMessage> {
        if self.confirm_delete {
            return match key.code {
                KeyCode::Char('d') if ctrl(key) => Some(SessionPickerMessage::ToggleDelete),
                KeyCode::Escape => Some(SessionPickerMessage::CancelDelete),
                _ => None,
            };
        }
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(SessionPickerMessage::Next),
                KeyCode::Char('p') => Some(SessionPickerMessage::Prev),
                KeyCode::Char('d') => Some(SessionPickerMessage::ToggleDelete),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(SessionPickerMessage::Close),
            KeyCode::Down => Some(SessionPickerMessage::Next),
            KeyCode::Up => Some(SessionPickerMessage::Prev),
            KeyCode::Enter => Some(SessionPickerMessage::Select),
            KeyCode::Char('n') => Some(SessionPickerMessage::NewSession),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: SessionPickerMessage) -> Option<SessionPickerEffect> {
        if !self.open && !matches!(msg, SessionPickerMessage::Close) {
            return None;
        }
        match msg {
            SessionPickerMessage::Close => {
                self.close();
                Some(SessionPickerEffect::Close)
            }
            SessionPickerMessage::Next => {
                self.next();
                None
            }
            SessionPickerMessage::Prev => {
                self.prev();
                None
            }
            SessionPickerMessage::Select => {
                if let Some(s) = self.sessions.get(self.selected) {
                    let id = s.id;
                    self.close();
                    Some(SessionPickerEffect::LoadSession { id })
                } else {
                    None
                }
            }
            SessionPickerMessage::ToggleDelete => {
                if self.sessions.is_empty() {
                    return None;
                }
                if self.confirm_delete {
                    let id = self.sessions[self.selected].id;
                    self.confirm_delete = false;
                    Some(SessionPickerEffect::DeleteSession { id })
                } else {
                    self.confirm_delete = true;
                    None
                }
            }
            SessionPickerMessage::CancelDelete => {
                self.confirm_delete = false;
                None
            }
            SessionPickerMessage::NewSession => {
                self.close();
                Some(SessionPickerEffect::NewSession)
            }
            SessionPickerMessage::Resize { viewport_height } => {
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
        let popup = centered_rect(64, 36, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Sessions");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);

        if self.loading && self.sessions.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    super::spinner::spinner(),
                    Span::raw(" Loading sessions...").fg(theme::TEXT_MUTED),
                ])),
                list_area,
            );
        } else {
            let offset = scroll_offset_for(
                self.selected,
                self.offset,
                list_area.height as usize,
                self.sessions.len(),
            );
            let visible: Vec<ListItem> = self
                .sessions
                .iter()
                .enumerate()
                .skip(offset)
                .take(list_area.height as usize)
                .map(|(idx, s)| {
                    let marker = theme::active_marker(Some(s.id) == self.active_id);
                    let content = format!("{marker}{} · {} msgs", s.title, s.message_count);
                    render_list_item(content, idx == self.selected)
                })
                .collect();
            frame.render_widget(List::new(visible), list_area);
        }

        let hint = if self.confirm_delete {
            theme::help_line(&[("Ctrl+D", "confirm delete"), ("Esc", "cancel")])
        } else if self.sessions.is_empty() {
            theme::help_line(&[("N", "new session"), ("Esc", "close")])
        } else {
            theme::help_line(&[
                ("Enter", "resume"),
                ("Ctrl+D", "delete"),
                ("N", "new"),
                ("Esc", "close"),
            ])
        };
        frame.render_widget(Paragraph::new(hint).fg(theme::TEXT_MUTED), hint_area);
    }
}

impl Default for SessionPicker {
    fn default() -> Self {
        Self::new()
    }
}
