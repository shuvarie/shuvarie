use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph};
use shuvarie_core::SessionSummary;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::list::{render_list_item, render_list_item_dim, scroll_offset_for};
use super::search::{Search, SearchMessage};
use super::{add_provider::centered_rect, popup, theme};

pub enum SessionPickerMessage {
    Search(SearchMessage),
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
    /// The query row the dialog opens with focused; every session's title is
    /// matched against it, best match first.
    pub search: Search,
    /// The indices into `sessions` the query keeps, in display order. The list
    /// renders this, not `sessions`: the unfiltered order is what "no query"
    /// means, so an empty query keeps it whole.
    matches: Vec<usize>,
    /// Position within `matches` — the row the highlight is on.
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
            search: Search::new(),
            matches: Vec::new(),
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
        self.search.clear();
        self.matches.clear();
        self.selected = 0;
        self.offset = 0;
        self.confirm_delete = false;
        self.active_id = active_id;
        self.loading = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
        self.confirm_delete = false;
        self.loading = false;
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionSummary>) {
        self.sessions = sessions;
        self.loading = false;
        self.rebuild();
    }

    /// The session the highlight is on. `None` while nothing matches the query.
    fn selected_session(&self) -> Option<&SessionSummary> {
        self.matches.get(self.selected).map(|i| &self.sessions[*i])
    }

    /// The next session to show after `id` is gone: the one below it in the
    /// list on screen, else the one above it. A filtered list answers with a
    /// filtered successor — the deleted session's neighbours are the rows the
    /// user was looking at.
    pub fn session_after(&self, id: uuid::Uuid) -> Option<uuid::Uuid> {
        let idx = self
            .matches
            .iter()
            .position(|i| self.sessions[*i].id == id)?;
        let next = idx + 1;
        if next < self.matches.len() {
            Some(self.sessions[self.matches[next]].id)
        } else if idx > 0 {
            Some(self.sessions[self.matches[idx - 1]].id)
        } else {
            None
        }
    }

    /// Re-rank the sessions against the query, keeping the highlight on the
    /// session it was on whenever that session survives the narrower list — a
    /// keystroke that only appends narrows, and jumping to the top would lose
    /// the session being typed towards.
    fn rebuild(&mut self) {
        let previous = self.matches.get(self.selected).copied();
        self.matches = self
            .search
            .filter_indices(self.sessions.len(), |i| self.sessions[i].title.clone());
        self.selected = previous
            .and_then(|session| self.matches.iter().position(|i| *i == session))
            .unwrap_or(0);
        self.offset = 0;
        self.recompute_offset();
    }

    fn next(&mut self) {
        if !self.matches.is_empty() {
            self.selected = (self.selected + 1).min(self.matches.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.matches.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.matches.len();
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
                // The query row owns every plain character, so "new" needs a
                // chord: `Ctrl+O` opens a session without one to resume.
                KeyCode::Char('o') => Some(SessionPickerMessage::NewSession),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(SessionPickerMessage::Close),
            KeyCode::Down => Some(SessionPickerMessage::Next),
            KeyCode::Up => Some(SessionPickerMessage::Prev),
            KeyCode::Enter => Some(SessionPickerMessage::Select),
            KeyCode::Backspace => Some(SessionPickerMessage::Search(SearchMessage::Backspace)),
            KeyCode::Char(c) => Some(SessionPickerMessage::Search(SearchMessage::Input(c))),
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
            SessionPickerMessage::Search(m) => {
                self.search.update(m);
                self.rebuild();
                None
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
                let session = self.selected_session()?;
                if session.in_use {
                    return None;
                }
                let id = session.id;
                self.close();
                Some(SessionPickerEffect::LoadSession { id })
            }
            SessionPickerMessage::ToggleDelete => {
                let session = self.selected_session()?;
                if session.in_use {
                    return None;
                }
                if self.confirm_delete {
                    let id = session.id;
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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(64, 36, area);
        popup::dialog(frame, popup, "Sessions", dimmed, |inner, buf| {
            let [query_area, list_area, hint_area] =
                Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

            self.search.view(buf, query_area, "Type to filter sessions");

            if self.loading && self.sessions.is_empty() {
                Paragraph::new(Line::from(vec![
                    super::spinner::spinner(),
                    Span::raw(" Loading sessions...").fg(theme::text_muted()),
                ]))
                .render(list_area, buf);
            } else if self.matches.is_empty() && !self.search.is_empty() {
                Paragraph::new(Span::raw("  no matching session").fg(theme::text_muted()))
                    .render(list_area, buf);
            } else {
                let offset = scroll_offset_for(
                    self.selected,
                    self.offset,
                    list_area.height as usize,
                    self.matches.len(),
                );
                let visible: Vec<ListItem> = self
                    .matches
                    .iter()
                    .enumerate()
                    .skip(offset)
                    .take(list_area.height as usize)
                    .map(|(idx, session)| {
                        let s = &self.sessions[*session];
                        let marker = theme::active_marker(Some(s.id) == self.active_id);
                        if s.in_use {
                            render_list_item_dim(
                                format!(
                                    "{marker}{} · {} msgs · used elsewhere",
                                    s.title, s.message_count
                                ),
                                idx == self.selected,
                            )
                        } else {
                            render_list_item(
                                format!("{marker}{} · {} msgs", s.title, s.message_count),
                                idx == self.selected,
                            )
                        }
                    })
                    .collect();
                Widget::render(List::new(visible), list_area, buf);
            }

            let locked_selected = self.selected_session().is_some_and(|s| s.in_use);
            let hint = if self.confirm_delete {
                theme::help_line(&[("Ctrl+D", "confirm delete"), ("Esc", "cancel")])
            } else if self.sessions.is_empty() {
                theme::help_line(&[("Ctrl+O", "new session"), ("Esc", "close")])
            } else if locked_selected {
                theme::help_line(&[("Ctrl+O", "new"), ("Esc", "close")])
            } else {
                theme::help_line(&[
                    ("Enter", "resume"),
                    ("Ctrl+D", "delete"),
                    ("Ctrl+O", "new"),
                    ("Esc", "close"),
                ])
            };
            Paragraph::new(hint)
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }
}

impl Default for SessionPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn summary(title: &str) -> SessionSummary {
        SessionSummary {
            id: uuid::Uuid::now_v7(),
            title: title.to_string(),
            message_count: 1,
            updated_at_epoch_ms: 0,
            in_use: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), Modifiers::CONTROL)
    }

    /// A picker holding `titles`, opened the way the app opens it.
    fn picker(titles: &[&str]) -> SessionPicker {
        let mut picker = SessionPicker::new();
        picker.open(None);
        picker.set_sessions(titles.iter().map(|title| summary(title)).collect());
        picker
    }

    fn visible_titles(picker: &SessionPicker) -> Vec<String> {
        picker
            .matches
            .iter()
            .map(|i| picker.sessions[*i].title.clone())
            .collect()
    }

    fn type_query(picker: &mut SessionPicker, query: &str) {
        for c in query.chars() {
            picker.update(SessionPickerMessage::Search(SearchMessage::Input(c)));
        }
    }

    #[test]
    fn typing_a_query_narrows_the_list_to_matching_titles() {
        let mut picker = picker(&["refactor the parser", "add a search box", "fix flaky test"]);
        assert_eq!(
            visible_titles(&picker).len(),
            3,
            "no query lists everything"
        );

        type_query(&mut picker, "search");
        assert_eq!(visible_titles(&picker), vec!["add a search box"]);

        // Backspace widens the list again — the query is the only filter.
        while !picker.search.is_empty() {
            picker.update(SessionPickerMessage::Search(SearchMessage::Backspace));
        }
        assert_eq!(visible_titles(&picker).len(), 3);
    }

    #[test]
    fn a_plain_n_is_a_search_character_and_ctrl_o_starts_a_session() {
        let picker = picker(&["one"]);
        let typed = picker.map_event(&key(KeyCode::Char('n'))).expect("mapped");
        assert!(matches!(
            typed,
            SessionPickerMessage::Search(SearchMessage::Input('n'))
        ));
        let new = picker.map_event(&ctrl_key('o')).expect("mapped");
        assert!(matches!(new, SessionPickerMessage::NewSession));
    }

    #[test]
    fn the_highlight_follows_its_session_through_a_narrower_query() {
        let mut picker = picker(&["alpha", "beta", "gamma"]);
        picker.update(SessionPickerMessage::Next);
        picker.update(SessionPickerMessage::Next);
        let gamma = picker.selected_session().expect("a selection").id;

        // A query that keeps the highlighted session leaves the highlight on it.
        type_query(&mut picker, "gam");
        assert_eq!(
            picker.selected_session().expect("still selected").id,
            gamma,
            "narrowing keeps the row being typed towards"
        );

        // A query that drops it falls back to the best match.
        picker.update(SessionPickerMessage::Search(SearchMessage::Clear));
        type_query(&mut picker, "b");
        assert_eq!(visible_titles(&picker), vec!["beta"]);
        assert_eq!(
            picker.selected, 0,
            "a dropped highlight falls back to the first row"
        );
    }

    #[test]
    fn selecting_resumes_the_visible_row() {
        let mut picker = picker(&["alpha", "beta"]);
        type_query(&mut picker, "bet");
        let Some(SessionPickerEffect::LoadSession { id }) =
            picker.update(SessionPickerMessage::Select)
        else {
            panic!("the filtered row resumes");
        };
        assert_eq!(id, picker.sessions[1].id);
        assert!(!picker.open, "resuming closes the dialog");
    }

    #[test]
    fn deleting_needs_a_second_ctrl_d_and_targets_the_visible_row() {
        let mut picker = picker(&["alpha", "beta"]);
        type_query(&mut picker, "bet");
        assert!(picker.update(SessionPickerMessage::ToggleDelete).is_none());
        assert!(picker.confirm_delete);
        let Some(SessionPickerEffect::DeleteSession { id }) =
            picker.update(SessionPickerMessage::ToggleDelete)
        else {
            panic!("the second press deletes");
        };
        assert_eq!(id, picker.sessions[1].id, "the row shown, not row 0");
        assert!(!picker.confirm_delete);
    }

    #[test]
    fn the_successor_of_a_deleted_session_comes_from_the_filtered_list() {
        let mut picker = picker(&["plan one", "plan two", "other"]);
        type_query(&mut picker, "plan");
        assert_eq!(visible_titles(&picker), vec!["plan one", "plan two"]);
        let first = picker.sessions[0].id;
        let second = picker.sessions[1].id;
        // `plan two` is next in the list on screen even though `other` sits
        // between them in the store's order... which it does not here — the
        // point is the filtered order decides, not the unfiltered one.
        assert_eq!(picker.session_after(first), Some(second));
        // The last visible row falls back to the one above it, never to a row
        // the query filtered out.
        assert_eq!(picker.session_after(second), Some(first));
    }

    #[test]
    fn a_session_in_use_cannot_be_resumed_or_deleted() {
        let mut picker = picker(&["shared"]);
        picker.sessions[0].in_use = true;
        assert!(picker.update(SessionPickerMessage::Select).is_none());
        assert!(picker.update(SessionPickerMessage::ToggleDelete).is_none());
        assert!(!picker.confirm_delete);
    }

    #[test]
    fn reopening_clears_the_previous_query() {
        let mut picker = picker(&["alpha", "beta"]);
        type_query(&mut picker, "alpha");
        assert!(picker.update(SessionPickerMessage::Close).is_some());
        picker.open(None);
        assert!(picker.search.is_empty(), "a fresh open is unfiltered");
        picker.set_sessions(vec![summary("alpha"), summary("beta")]);
        assert_eq!(visible_titles(&picker).len(), 2);
    }
}
