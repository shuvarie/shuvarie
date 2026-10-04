//! The theme picker overlay: walks every selectable theme (the unset default,
//! the built-in palettes, user-defined themes), previews each highlighted
//! palette live by swapping the TUI's active colors, and commits the choice
//! to the config on select. Closing without a selection restores the palette
//! active at open.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::{popup, theme};
use crate::tui::add_provider::centered_rect;
use crate::tui::list::scroll_offset_for;
use crate::tui::utils::ctrl;

use shuvarie_core::ThemeChoice;

pub enum ThemePickerMessage {
    Next,
    Prev,
    Select,
    Close,
}

#[derive(Debug)]
pub enum ThemePickerEffect {
    /// Preview the newly highlighted entry's palette (Next/Prev): the caller
    /// swaps the active palette, and restores the previous one on close.
    Preview {
        colors: shuvarie_core::ThemeColors,
    },
    Select {
        pref: Option<String>,
    },
    Close,
}

pub struct ThemePicker {
    pub open: bool,
    entries: Vec<ThemeChoice>,
    pub selected: usize,
    pub offset: usize,
}

impl ThemePicker {
    pub fn new() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            selected: 0,
            offset: 0,
        }
    }

    /// Opens the picker with the given choices; the row matching the current
    /// `ui.theme` pref (`None` = the unset default) is preselected.
    pub fn open(&mut self, choices: &[ThemeChoice], current: Option<&str>) {
        self.entries = choices.to_vec();
        self.selected = self
            .entries
            .iter()
            .position(|choice| choice.pref.as_deref() == current)
            .unwrap_or(0);
        self.offset = 0;
        self.open = true;
        self.recompute_offset();
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// The highlighted choice, for the live preview.
    pub fn selected(&self) -> Option<&ThemeChoice> {
        self.entries.get(self.selected)
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ThemePickerMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(ThemePickerMessage::Next),
                KeyCode::Char('p') => Some(ThemePickerMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(ThemePickerMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(ThemePickerMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(ThemePickerMessage::Prev),
            KeyCode::Enter => Some(ThemePickerMessage::Select),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ThemePickerMessage) -> Option<ThemePickerEffect> {
        if !self.open {
            return None;
        }
        match msg {
            ThemePickerMessage::Close => {
                self.close();
                Some(ThemePickerEffect::Close)
            }
            ThemePickerMessage::Next => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    self.recompute_offset();
                    return Some(ThemePickerEffect::Preview {
                        colors: self.selected()?.colors,
                    });
                }
                None
            }
            ThemePickerMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset();
                Some(ThemePickerEffect::Preview {
                    colors: self.selected()?.colors,
                })
            }
            ThemePickerMessage::Select => {
                let entry = self.entries.get(self.selected)?;
                self.open = false;
                Some(ThemePickerEffect::Select {
                    pref: entry.pref.clone(),
                })
            }
        }
    }

    fn recompute_offset(&mut self) {
        self.offset = scroll_offset_for(self.selected, self.offset, 0, self.entries.len());
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 50, area);
        popup::dialog(frame, popup, "Theme", dimmed, |inner, buf| {
            let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
            if self.entries.is_empty() {
                Paragraph::new("no themes available".to_string())
                    .fg(theme::text_muted())
                    .render(list_area, buf);
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
                    .map(|(idx, choice)| self.render_row(choice, idx == self.selected))
                    .collect();
                Widget::render(List::new(items), list_area, buf);
            }

            let hint = theme::help_line(&[
                ("↑↓", "walk (previews live)"),
                ("Enter", "pick"),
                ("Esc", "cancel"),
            ]);
            Paragraph::new(hint)
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }

    fn render_row(&self, choice: &ThemeChoice, is_selected: bool) -> ListItem<'static> {
        let prefix: &str = if is_selected { "▶ " } else { "  " };
        let mut spans = vec![
            Span::raw(prefix).fg(theme::accent()),
            Span::raw(choice.label.clone()).fg(if is_selected {
                theme::text()
            } else {
                theme::text_dim()
            }),
        ];
        if let Some(variant) = &choice.variant {
            spans.push(Span::raw(format!(" · {variant}")).fg(theme::text_muted()));
        }
        // Swatches from the choice's own palette: its background, its accent
        // over the accent background, and its text color.
        let colors = choice.colors;
        spans.push(Span::raw("  ").fg(color_to_color(colors.bg)));
        spans.push(
            Span::raw("██")
                .fg(color_to_color(colors.accent))
                .bg(color_to_color(colors.accent_bg)),
        );
        spans.push(
            Span::raw("██")
                .fg(color_to_color(colors.text))
                .bg(color_to_color(colors.bg)),
        );
        if is_selected {
            spans.push(Span::raw(" ●").fg(color_to_color(colors.accent)));
        }
        ListItem::new(Line::from(spans)).style(if is_selected {
            ratatui::style::Style::new().bg(theme::surface_focused())
        } else {
            ratatui::style::Style::new()
        })
    }
}

fn color_to_color(rgb: shuvarie_core::Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_core::{ThemeColors, ThemeSet, ThemeVariant};
    use termina::event::Modifiers;

    fn choices() -> Vec<ThemeChoice> {
        ThemeSet::default().choices(ThemeVariant::Dark)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    #[test]
    fn open_preselects_the_current_theme() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), Some("Kanagawa:wave"));
        let selected = &picker.entries[picker.selected];
        assert_eq!(selected.pref.as_deref(), Some("Kanagawa:wave"));
        assert!(picker.open);
    }

    #[test]
    fn open_preselects_the_default_row_when_unset() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), None);
        assert_eq!(picker.selected, 0);
        assert_eq!(picker.entries[0].pref, None);
    }

    #[test]
    fn walking_previews_and_select_sends_the_pref() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), Some("Kanagawa:wave"));
        assert_eq!(picker.selected, 7, "Kanagawa:wave is the 8th row");
        let effect = picker.update(ThemePickerMessage::Next).expect("preview");
        assert!(
            matches!(
                effect,
                ThemePickerEffect::Preview {
                    colors: ThemeColors {
                        bg: (24, 22, 22),
                        ..
                    }
                },
            ),
            "the next row down is Kanagawa dragon"
        );
        assert_eq!(picker.selected, 8);
        let effect = picker.update(ThemePickerMessage::Select).expect("effect");
        match effect {
            ThemePickerEffect::Select { pref } => {
                assert_eq!(pref.as_deref(), Some("Kanagawa:dragon"));
            }
            other => panic!("unexpected effect: {other:?}"),
        }
        assert!(!picker.open, "the picker closes on select");
    }

    #[test]
    fn closing_without_a_selection_reports_cancel() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), None);
        picker.update(ThemePickerMessage::Next);
        assert!(matches!(
            picker.update(ThemePickerMessage::Close),
            Some(ThemePickerEffect::Close)
        ));
        assert!(!picker.open);
    }

    #[test]
    fn navigation_clamps_at_the_ends() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), None);
        picker.update(ThemePickerMessage::Prev);
        assert_eq!(picker.selected, 0);
        for _ in 0..picker.entries.len() + 4 {
            picker.update(ThemePickerMessage::Next);
        }
        assert_eq!(
            picker.selected,
            picker.entries.len() - 1,
            "walking stops at the last row"
        );
    }

    #[test]
    fn closed_picker_ignores_messages() {
        let mut picker = ThemePicker::new();
        assert!(picker.update(ThemePickerMessage::Next).is_none());
        assert!(picker.update(ThemePickerMessage::Select).is_none());
    }

    #[test]
    fn map_event_routes_the_keys() {
        let mut picker = ThemePicker::new();
        picker.open(&choices(), None);
        assert!(matches!(
            picker.map_event(&key(KeyCode::Down)),
            Some(ThemePickerMessage::Next)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Up)),
            Some(ThemePickerMessage::Prev)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Escape)),
            Some(ThemePickerMessage::Close)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Enter)),
            Some(ThemePickerMessage::Select)
        ));
        assert!(picker.map_event(&key(KeyCode::Char('x'))).is_none());
    }
}
