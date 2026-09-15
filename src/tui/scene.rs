use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::theme;
use crate::tui::add_provider::centered_rect;
use crate::tui::list::scroll_offset_for;
use shuvarie_core::scenes::SceneListEntry;

pub enum SceneMessage {
    Next,
    Prev,
    Select,
    Close,
}

pub enum SceneEffect {
    /// Switch the session's scene (`None` = the built-in default scene).
    Switch {
        name: Option<String>,
    },
    Close,
}

/// One row of the switcher: a configured scene or the built-in Default,
/// carried by identity (`None` = built-in) rather than by name, so a
/// configured scene named "Default" stays switchable. `switchable` is the
/// effective mid-session availability: an interlude-less scene only
/// qualifies while the session has no messages yet.
struct SceneEntry {
    id: Option<String>,
    name: String,
    description: Option<String>,
    current: bool,
    switchable: bool,
}

pub struct ScenePicker {
    pub open: bool,
    entries: Vec<SceneEntry>,
    pub selected: usize,
    pub offset: usize,
}

impl ScenePicker {
    pub fn new() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            selected: 0,
            offset: 0,
        }
    }

    /// Opens the switcher. `mid_session` marks a session that already has
    /// messages: scenes without an interlude cannot be entered anymore (the
    /// core refuses the switch — the rows render dimmed as start-only).
    pub fn open(&mut self, entries: Vec<SceneListEntry>, current: Option<&str>, mid_session: bool) {
        self.entries = entries
            .into_iter()
            .map(|entry| {
                let current = match (&entry.id, current) {
                    (None, None) => true,
                    (Some(id), Some(current)) => id == current,
                    _ => false,
                };
                SceneEntry {
                    id: entry.id,
                    name: entry.name,
                    description: entry.description,
                    current,
                    switchable: entry.switchable || !mid_session,
                }
            })
            .collect();
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.current)
            .unwrap_or(0);
        self.offset = 0;
        self.open = true;
        self.recompute_offset();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SceneMessage> {
        match key.code {
            KeyCode::Escape => Some(SceneMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(SceneMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(SceneMessage::Prev),
            KeyCode::Enter => Some(SceneMessage::Select),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: SceneMessage) -> Option<SceneEffect> {
        if !self.open {
            return None;
        }
        match msg {
            SceneMessage::Close => {
                self.close();
                Some(SceneEffect::Close)
            }
            SceneMessage::Next => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    self.recompute_offset();
                }
                None
            }
            SceneMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset();
                None
            }
            SceneMessage::Select => {
                let entry = self.entries.get(self.selected)?;
                self.open = false;
                Some(SceneEffect::Switch {
                    name: entry.id.clone(),
                })
            }
        }
    }

    fn recompute_offset(&mut self) {
        self.offset = scroll_offset_for(self.selected, self.offset, 0, self.entries.len());
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(56, 30, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Scene");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
        if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new("no scenes configured".to_string()).fg(theme::TEXT_MUTED),
                list_area,
            );
        } else {
            let offset = scroll_offset_for(
                self.selected,
                self.offset,
                list_area.height as usize,
                self.entries.len(),
            );
            let items: Vec<ListItem> = self
                .entries
                .iter()
                .enumerate()
                .skip(offset)
                .take(list_area.height as usize)
                .map(|(idx, entry)| self.render_row(entry, idx == self.selected))
                .collect();
            frame.render_widget(List::new(items), list_area);
        }

        let hint = theme::help_line(&[("↑↓", "walk"), ("Enter", "switch"), ("Esc", "close")]);
        frame.render_widget(Paragraph::new(hint).fg(theme::TEXT_MUTED), hint_area);
    }

    fn render_row(&self, entry: &SceneEntry, is_selected: bool) -> ListItem<'static> {
        let prefix: &str = if is_selected { "▶ " } else { "  " };
        let name_color = if !entry.switchable {
            theme::TEXT_MUTED
        } else if entry.current {
            theme::ACCENT
        } else {
            theme::TEXT
        };
        let mut spans = vec![
            Span::raw(prefix).fg(theme::ACCENT),
            Span::raw("⌗ ").fg(theme::TEXT_MUTED),
            Span::raw(entry.name.clone()).fg(name_color),
        ];
        if let Some(description) = &entry.description {
            spans.push(Span::raw(" — ").fg(theme::TEXT_MUTED));
            spans.push(Span::raw(description.clone()).fg(theme::TEXT_DIM));
        }
        if !entry.switchable {
            spans.push(Span::raw(" (no interlude)").fg(theme::TEXT_MUTED));
        }
        if entry.current {
            spans.push(Span::raw(" ●").fg(theme::ACCENT));
        }
        ListItem::new(Line::from(spans)).style(if is_selected {
            ratatui::style::Style::new().bg(theme::ACCENT_BG)
        } else {
            ratatui::style::Style::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    fn entries() -> Vec<SceneListEntry> {
        vec![
            SceneListEntry {
                id: None,
                name: shuvarie_core::scenes::DEFAULT_SCENE_NAME.to_string(),
                description: Some("built-in".into()),
                switchable: false,
            },
            SceneListEntry {
                id: Some("Plan".into()),
                name: "Plan".into(),
                description: Some("plan first".into()),
                switchable: true,
            },
        ]
    }

    fn entries_with_configured_default() -> Vec<SceneListEntry> {
        vec![
            SceneListEntry {
                id: None,
                name: shuvarie_core::scenes::DEFAULT_SCENE_NAME.to_string(),
                description: Some("built-in".into()),
                switchable: false,
            },
            SceneListEntry {
                id: Some("Default".into()),
                name: "Default".into(),
                description: Some("custom default".into()),
                switchable: true,
            },
        ]
    }

    #[test]
    fn select_sends_none_for_the_builtin_scene() {
        let mut picker = ScenePicker::new();
        picker.open(entries(), Some("Plan"), false);
        // The current scene is preselected: Plan.
        assert_eq!(picker.selected, 1);
        let effect = picker.update(SceneMessage::Select).expect("effect");
        assert!(matches!(
            effect,
            SceneEffect::Switch { name: Some(name) } if name == "Plan"
        ));
        assert!(!picker.open, "the picker closes on select");
    }

    #[test]
    fn a_configured_default_switches_by_identity() {
        let mut picker = ScenePicker::new();
        picker.open(entries_with_configured_default(), Some("Default"), false);
        assert_eq!(picker.selected, 1, "only the configured row is current");
        let effect = picker.update(SceneMessage::Select).expect("effect");
        assert!(matches!(
            effect,
            SceneEffect::Switch { name: Some(name) } if name == "Default"
        ));
    }

    #[test]
    fn the_builtin_row_marks_only_when_no_scene_is_active() {
        let mut picker = ScenePicker::new();
        picker.open(entries_with_configured_default(), None, false);
        assert_eq!(picker.selected, 0, "the built-in row is current");
        let effect = picker.update(SceneMessage::Select).expect("effect");
        assert!(matches!(effect, SceneEffect::Switch { name: None }));
    }

    #[test]
    fn walking_to_the_default_selects_none() {
        let mut picker = ScenePicker::new();
        picker.open(entries(), Some("Plan"), false);
        picker.update(SceneMessage::Prev);
        let effect = picker.update(SceneMessage::Select).expect("effect");
        assert!(matches!(effect, SceneEffect::Switch { name: None }));
    }

    #[test]
    fn navigation_clamps_at_the_ends() {
        let mut picker = ScenePicker::new();
        picker.open(entries(), None, false);
        picker.update(SceneMessage::Prev);
        assert_eq!(picker.selected, 0);
        picker.update(SceneMessage::Next);
        assert_eq!(picker.selected, 1);
        picker.update(SceneMessage::Next);
        assert_eq!(picker.selected, 1);
    }

    #[test]
    fn closed_picker_ignores_messages() {
        let mut picker = ScenePicker::new();
        assert!(picker.update(SceneMessage::Next).is_none());
        assert!(picker.update(SceneMessage::Select).is_none());
    }

    #[test]
    fn mid_session_opens_dim_interlude_less_rows() {
        let mut picker = ScenePicker::new();
        picker.open(entries(), Some("Plan"), true);
        assert!(
            !picker.entries[0].switchable,
            "the built-in row is start-only"
        );
        assert!(picker.entries[1].switchable, "Plan carries an interlude");
    }

    #[test]
    fn a_fresh_session_opens_everything_switchable() {
        let mut picker = ScenePicker::new();
        picker.open(entries(), None, false);
        assert!(
            picker.entries.iter().all(|entry| entry.switchable),
            "before the first message every scene can be entered"
        );
    }

    #[test]
    fn map_event_routes_the_keys() {
        let picker = ScenePicker::new();
        assert!(matches!(
            picker.map_event(&key(KeyCode::Down)),
            Some(SceneMessage::Next)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Escape)),
            Some(SceneMessage::Close)
        ));
        assert!(picker.map_event(&key(KeyCode::Char('x'))).is_none());
    }
}
