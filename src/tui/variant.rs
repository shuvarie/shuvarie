use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::theme;
use crate::tui::add_provider::centered_rect;
use crate::tui::list::scroll_offset_for;
use crate::tui::utils::ctrl;

/// The label of the unset-default row — also the `/variant` argument that
/// selects it.
pub const DEFAULT_VARIANT: &str = "default";

pub enum VariantMessage {
    Next,
    Prev,
    Select,
    Close,
}

pub enum VariantEffect {
    /// Pick a reasoning-effort variant (`None` = the unset default).
    Set {
        variant: Option<String>,
    },
    Close,
}

/// Resolves a `/variant <name>` argument: `default` (any case) picks the
/// unset default, anything else must case-insensitively match one of the
/// model's declared variants (returning its canonical casing). `None` when
/// the argument names nothing the model declares.
pub fn resolve_arg(variants: &[String], arg: &str) -> Option<Option<String>> {
    if arg.eq_ignore_ascii_case(DEFAULT_VARIANT) {
        return Some(None);
    }
    variants
        .iter()
        .find(|v| v.eq_ignore_ascii_case(arg))
        .map(|v| Some(v.clone()))
}

/// One row of the selector: the unset default leads, then the model's
/// declared reasoning-effort variants in catalog order.
struct VariantEntry {
    variant: Option<String>,
    current: bool,
}

pub struct VariantPicker {
    pub open: bool,
    entries: Vec<VariantEntry>,
    pub selected: usize,
    pub offset: usize,
}

impl VariantPicker {
    pub fn new() -> Self {
        Self {
            open: false,
            entries: Vec::new(),
            selected: 0,
            offset: 0,
        }
    }

    /// Opens the selector with the unset default followed by the model's
    /// declared variants; the current pick is preselected.
    pub fn open(&mut self, variants: &[String], current: Option<&str>) {
        let mut entries = vec![VariantEntry {
            variant: None,
            current: current.is_none(),
        }];
        entries.extend(variants.iter().map(|v| VariantEntry {
            variant: Some(v.clone()),
            current: current == Some(v.as_str()),
        }));
        self.entries = entries;
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

    pub fn map_event(&self, key: &KeyEvent) -> Option<VariantMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(VariantMessage::Next),
                KeyCode::Char('p') => Some(VariantMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(VariantMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(VariantMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(VariantMessage::Prev),
            KeyCode::Enter => Some(VariantMessage::Select),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: VariantMessage) -> Option<VariantEffect> {
        if !self.open {
            return None;
        }
        match msg {
            VariantMessage::Close => {
                self.close();
                Some(VariantEffect::Close)
            }
            VariantMessage::Next => {
                if !self.entries.is_empty() {
                    self.selected = (self.selected + 1).min(self.entries.len() - 1);
                    self.recompute_offset();
                }
                None
            }
            VariantMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset();
                None
            }
            VariantMessage::Select => {
                let entry = self.entries.get(self.selected)?;
                self.open = false;
                Some(VariantEffect::Set {
                    variant: entry.variant.clone(),
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
        let block = theme::overlay_block("Variant");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
        if self.entries.is_empty() {
            frame.render_widget(
                Paragraph::new("no variants available".to_string()).fg(theme::text_muted()),
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

        let hint = theme::help_line(&[("↑↓", "walk"), ("Enter", "pick"), ("Esc", "close")]);
        frame.render_widget(Paragraph::new(hint).fg(theme::text_muted()), hint_area);
    }

    fn render_row(&self, entry: &VariantEntry, is_selected: bool) -> ListItem<'static> {
        let prefix: &str = if is_selected { "▶ " } else { "  " };
        let mut spans = vec![
            Span::raw(prefix).fg(theme::accent()),
            Span::raw("⌗ ").fg(theme::text_muted()),
        ];
        match &entry.variant {
            Some(variant) => {
                let color = if entry.current {
                    theme::accent()
                } else {
                    theme::text()
                };
                spans.push(Span::raw(variant.clone()).fg(color));
            }
            None => {
                let color = if entry.current {
                    theme::accent()
                } else {
                    theme::text()
                };
                spans.push(Span::raw(DEFAULT_VARIANT.to_string()).fg(color));
                spans.push(Span::raw(" (unset)").fg(theme::text_muted()));
            }
        }
        if entry.current {
            spans.push(Span::raw(" ●").fg(theme::accent()));
        }
        ListItem::new(Line::from(spans)).style(if is_selected {
            ratatui::style::Style::new().bg(theme::accent_bg())
        } else {
            ratatui::style::Style::new()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn variants() -> Vec<String> {
        vec!["low".into(), "high".into(), "max".into()]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    #[test]
    fn open_preselects_the_current_variant() {
        let mut picker = VariantPicker::new();
        picker.open(&variants(), Some("high"));
        assert_eq!(picker.selected, 2);
        assert_eq!(picker.entries[2].variant.as_deref(), Some("high"));
        assert!(picker.entries.iter().filter(|e| e.current).count() == 1);
    }

    #[test]
    fn open_preselects_the_default_row_when_unset() {
        let mut picker = VariantPicker::new();
        picker.open(&variants(), None);
        assert_eq!(picker.selected, 0);
        assert!(picker.entries[0].current, "the default row is current");
        assert!(
            picker.entries[1..].iter().all(|e| !e.current),
            "no variant row is current"
        );
    }

    #[test]
    fn select_sends_the_picked_variant_and_closes() {
        let mut picker = VariantPicker::new();
        picker.open(&variants(), Some("low"));
        picker.update(VariantMessage::Next);
        let effect = picker.update(VariantMessage::Select).expect("effect");
        assert!(matches!(
            effect,
            VariantEffect::Set {
                variant: Some(v)
            } if v == "high"
        ));
        assert!(!picker.open, "the picker closes on select");
    }

    #[test]
    fn selecting_the_default_row_picks_none() {
        let mut picker = VariantPicker::new();
        picker.open(&variants(), Some("low"));
        picker.update(VariantMessage::Prev);
        let effect = picker.update(VariantMessage::Select).expect("effect");
        assert!(matches!(effect, VariantEffect::Set { variant: None }));
    }

    #[test]
    fn navigation_clamps_at_the_ends() {
        let mut picker = VariantPicker::new();
        picker.open(&variants(), None);
        picker.update(VariantMessage::Prev);
        assert_eq!(picker.selected, 0);
        picker.update(VariantMessage::Next);
        assert_eq!(picker.selected, 1);
        picker.update(VariantMessage::Next);
        picker.update(VariantMessage::Next);
        picker.update(VariantMessage::Next);
        assert_eq!(picker.selected, 3, "three variants plus the default row");
    }

    #[test]
    fn closed_picker_ignores_messages() {
        let mut picker = VariantPicker::new();
        assert!(picker.update(VariantMessage::Next).is_none());
        assert!(picker.update(VariantMessage::Select).is_none());
    }

    #[test]
    fn resolve_arg_matches_declared_variants_case_insensitively() {
        assert_eq!(
            resolve_arg(&variants(), "HIGH"),
            Some(Some("high".to_string()))
        );
        assert_eq!(resolve_arg(&variants(), "Low"), Some(Some("low".into())));
        assert_eq!(
            resolve_arg(&variants(), "max"),
            Some(Some("max".to_string()))
        );
    }

    #[test]
    fn resolve_arg_accepts_default_in_any_case() {
        assert_eq!(resolve_arg(&variants(), "default"), Some(None));
        assert_eq!(resolve_arg(&variants(), "Default"), Some(None));
        assert_eq!(resolve_arg(&variants(), "DEFAULT"), Some(None));
    }

    #[test]
    fn resolve_arg_rejects_unknown_values() {
        assert_eq!(resolve_arg(&variants(), "xhigh"), None);
        assert_eq!(resolve_arg(&variants(), "medium"), None, "must be declared");
        assert_eq!(resolve_arg(&variants(), ""), None);
    }

    #[test]
    fn map_event_routes_the_keys() {
        let picker = VariantPicker::new();
        assert!(matches!(
            picker.map_event(&key(KeyCode::Down)),
            Some(VariantMessage::Next)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Up)),
            Some(VariantMessage::Prev)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Escape)),
            Some(VariantMessage::Close)
        ));
        assert!(matches!(
            picker.map_event(&key(KeyCode::Enter)),
            Some(VariantMessage::Select)
        ));
        assert!(picker.map_event(&key(KeyCode::Char('x'))).is_none());
    }
}
