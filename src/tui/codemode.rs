//! The `/codemode` overlay: whether the scripting tool is offered, how it
//! presents the roster, and the limits a script runs under.
//!
//! The three switches here are the ones a user changes while working (offer it,
//! hide the tools it can run, keep state between scripts). The budgets are shown
//! but not edited: they are a one-time decision about cost, made where the rest
//! of the config is — `shuvarie.kdl` — and a number typed into a popup is harder
//! to review than a line in a file.
//!
//! Which tools a *script* may run is deliberately not here either. That is a
//! per-tool decision (`codemode { tools { run_shell { disabled #true } } }`), it
//! belongs beside the same tool's permission rules, and the overlay would need
//! the turn's roster — which lives in the core, not the TUI — to list the
//! candidates.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent};

use super::{popup, theme};
use crate::tui::add_provider::centered_rect;
use crate::tui::utils::ctrl;

use shuvarie_core::{CodemodeConfig, CodemodeEnabled, CodemodeMode};

/// The settings the overlay edits and reports, in the shape the core needs to
/// write them back. The budgets ride along so the overlay can show them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodemodePrefs {
    pub enabled: CodemodeEnabled,
    pub mode: CodemodeMode,
    pub store: bool,
    /// Estimated tokens of inlined declarations (`0` = nothing inlined).
    pub inline_budget: usize,
    /// Wall-clock budget for one script (`0` = none).
    pub timeout_ms: u64,
    /// Nested calls per script (`0` = unlimited).
    pub max_operations: u64,
    pub max_tool_calls: usize,
}

impl CodemodePrefs {
    pub fn from_config(config: &CodemodeConfig) -> Self {
        Self {
            enabled: config.enabled,
            mode: config.mode,
            store: config.store,
            inline_budget: config.inline_budget,
            timeout_ms: config.timeout_ms,
            max_operations: config.max_operations,
            max_tool_calls: config.max_tool_calls,
        }
    }
}

impl Default for CodemodePrefs {
    fn default() -> Self {
        Self::from_config(&CodemodeConfig::default())
    }
}

/// A row of the settings list. The budgets are rows too, so the list reads as
/// one answer to "what would a script run under?".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Enabled,
    Mode,
    Store,
    InlineBudget,
    Timeout,
    MaxOperations,
    MaxToolCalls,
}

impl Row {
    const ALL: [Row; 7] = [
        Row::Enabled,
        Row::Mode,
        Row::Store,
        Row::InlineBudget,
        Row::Timeout,
        Row::MaxOperations,
        Row::MaxToolCalls,
    ];

    fn label(self) -> &'static str {
        match self {
            Row::Enabled => "Offered",
            Row::Mode => "Roster",
            Row::Store => "Keep state",
            Row::InlineBudget => "Inline budget",
            Row::Timeout => "Timeout",
            Row::MaxOperations => "Operation limit",
            Row::MaxToolCalls => "Tool calls per script",
        }
    }

    /// Whether the row is a switch, and so reacts to Enter.
    fn switchable(self) -> bool {
        matches!(self, Row::Enabled | Row::Mode | Row::Store)
    }
}

pub enum CodemodeMessage {
    Next,
    Prev,
    Toggle,
    Close,
}

#[derive(Debug)]
pub enum CodemodeEffect {
    /// A switch was flipped: the caller writes it back.
    Changed(CodemodePrefs),
    Close,
}

pub struct CodemodeEditor {
    pub open: bool,
    prefs: CodemodePrefs,
    pub selected: usize,
}

impl CodemodeEditor {
    pub fn new() -> Self {
        Self {
            open: false,
            prefs: CodemodePrefs::default(),
            selected: 0,
        }
    }

    pub fn open(&mut self, prefs: CodemodePrefs) {
        self.prefs = prefs;
        self.selected = 0;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<CodemodeMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(CodemodeMessage::Next),
                KeyCode::Char('p') => Some(CodemodeMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(CodemodeMessage::Close),
            KeyCode::Down | KeyCode::Char('j') => Some(CodemodeMessage::Next),
            KeyCode::Up | KeyCode::Char('k') => Some(CodemodeMessage::Prev),
            KeyCode::Enter | KeyCode::Char(' ') => Some(CodemodeMessage::Toggle),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: CodemodeMessage) -> Option<CodemodeEffect> {
        if !self.open {
            return None;
        }
        match msg {
            CodemodeMessage::Close => {
                self.close();
                Some(CodemodeEffect::Close)
            }
            CodemodeMessage::Next => {
                self.selected = (self.selected + 1).min(Row::ALL.len() - 1);
                None
            }
            CodemodeMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                None
            }
            CodemodeMessage::Toggle => {
                // Enter on a budget row does nothing rather than moving the
                // cursor: there is no field to type into, and silently jumping
                // would look like the key was swallowed.
                if !self.selected_row().switchable() {
                    return None;
                }
                self.flip();
                Some(CodemodeEffect::Changed(self.prefs))
            }
        }
    }

    fn selected_row(&self) -> Row {
        Row::ALL[self.selected.min(Row::ALL.len() - 1)]
    }

    fn flip(&mut self) {
        match self.selected_row() {
            Row::Enabled => self.prefs.enabled = self.prefs.enabled.next(),
            Row::Mode => self.prefs.mode = self.prefs.mode.next(),
            Row::Store => self.prefs.store = !self.prefs.store,
            // The budgets are shown, not edited.
            Row::InlineBudget | Row::Timeout | Row::MaxOperations | Row::MaxToolCalls => {}
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup_area = centered_rect(64, 50, area);
        popup::dialog(frame, popup_area, "Codemode", dimmed, |inner, buf| {
            let [list_area, note_area, hint_area] =
                Layout::vertical([Min(0), Length(2), Length(1)]).areas(inner);

            let rows: Vec<Line> = Row::ALL
                .iter()
                .enumerate()
                .map(|(index, row)| self.render_row(*row, index == self.selected))
                .collect();
            Paragraph::new(rows).render(list_area, buf);

            Paragraph::new(self.note())
                .fg(theme::text_muted())
                .wrap(Wrap { trim: true })
                .render(note_area, buf);

            let hint = theme::help_line(&[("↑↓", "walk"), ("Enter", "change"), ("Esc", "close")]);
            Paragraph::new(hint)
                .fg(theme::text_muted())
                .render(hint_area, buf);
        });
    }

    /// The note for the selected row: what the highlighted setting means. The
    /// values themselves stay short enough for the label column, so the sentence
    /// that explains them lives here instead of widening the row past the popup.
    fn note(&self) -> &'static str {
        if self.prefs.enabled == CodemodeEnabled::Off {
            return "Off: the tool is not offered, and no script can run.";
        }
        match self.selected_row() {
            Row::Enabled => match self.prefs.enabled {
                CodemodeEnabled::On => "Offered on every turn; `disabled #true` turns it off.",
                CodemodeEnabled::Auto => {
                    "Offered only on turns that have a configured tool or an MCP server to \
                     orchestrate."
                }
                CodemodeEnabled::Off => "Never offered.",
            },
            Row::Mode => match self.prefs.mode {
                CodemodeMode::Declared => {
                    "Every tool stays in the roster; the model picks a script or a direct call."
                }
                CodemodeMode::DeclareBuiltin => {
                    "Built-in tools stay in the roster; stdio and MCP tools are reachable only \
                     from a script."
                }
                CodemodeMode::Only => {
                    "Only codemode is in the roster; every other tool is reachable only from a \
                     script."
                }
            },
            Row::Store => "Whether a script's `store`/`load` values survive between scripts.",
            Row::InlineBudget | Row::Timeout | Row::MaxOperations | Row::MaxToolCalls => {
                "Set in `shuvarie.kdl`, under `codemode { }`."
            }
        }
    }

    fn render_row(&self, row: Row, is_selected: bool) -> Line<'static> {
        let prefix: &str = if is_selected { "▶ " } else { "  " };
        let mut spans = vec![
            Span::raw(prefix).fg(theme::accent()),
            Span::raw(format!("{:<24}", row.label())).fg(if is_selected {
                theme::text()
            } else {
                theme::text_dim()
            }),
        ];
        // A switch reads as a value, not a checkbox: the row says what the
        // setting *is*, which is what the config file says too.
        spans.push(Span::raw(self.value(row)).fg(if row.switchable() {
            theme::accent()
        } else {
            theme::text_muted()
        }));
        if !row.switchable() {
            spans.push(Span::raw("  (shuvarie.kdl)").fg(theme::text_muted()));
        }
        Line::from(spans)
    }

    fn value(&self, row: Row) -> String {
        let on_off = |flag: bool| if flag { "on" } else { "off" }.to_string();
        match row {
            Row::Enabled => self.prefs.enabled.label().to_string(),
            Row::Store => on_off(self.prefs.store),
            Row::Mode => match self.prefs.mode {
                CodemodeMode::Declared => "all tools".to_string(),
                CodemodeMode::DeclareBuiltin => "builtins only".to_string(),
                CodemodeMode::Only => "codemode only".to_string(),
            },
            Row::InlineBudget => format!("{} tokens", self.prefs.inline_budget),
            Row::Timeout => match self.prefs.timeout_ms {
                0 => "none".to_string(),
                ms => format!("{ms} ms"),
            },
            Row::MaxOperations => match self.prefs.max_operations {
                0 => "unlimited".to_string(),
                operations => operations.to_string(),
            },
            Row::MaxToolCalls => match self.prefs.max_tool_calls {
                0 => "unlimited".to_string(),
                calls => calls.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn opened() -> CodemodeEditor {
        let mut editor = CodemodeEditor::new();
        editor.open(CodemodePrefs::from_config(&CodemodeConfig {
            enabled: CodemodeEnabled::On,
            ..CodemodeConfig::default()
        }));
        editor
    }

    #[test]
    fn flipping_the_first_row_reports_the_new_settings() {
        let mut editor = opened();
        let effect = editor.update(CodemodeMessage::Toggle).unwrap();
        let CodemodeEffect::Changed(prefs) = effect else {
            panic!("expected a change");
        };
        // `on` walks to `auto`, the next value in the declaration order.
        assert_eq!(prefs.enabled, CodemodeEnabled::Auto);
        // The switch travels with the settings, so the core writes one value.
        assert_eq!(prefs.inline_budget, 3000);
    }

    /// `enabled` is three-valued now, so the row has to walk all three and come
    /// back around — a plain toggle would strand `auto`.
    #[test]
    fn the_enabled_row_cycles_through_auto_on_and_off() {
        let mut editor = opened();
        assert_eq!(editor.prefs.enabled, CodemodeEnabled::On);

        let mut seen = Vec::new();
        for _ in 0..CodemodeEnabled::ALL.len() {
            let Some(CodemodeEffect::Changed(prefs)) = editor.update(CodemodeMessage::Toggle)
            else {
                panic!("expected a change");
            };
            seen.push(prefs.enabled);
        }

        assert_eq!(
            seen,
            vec![
                CodemodeEnabled::Auto,
                CodemodeEnabled::Off,
                CodemodeEnabled::On,
            ],
            "a full cycle visits each value once and wraps"
        );
    }

    /// The overlay reads `on`/`off`, not the config file's `true`/`false`:
    /// the row is prose beside the other switches, not a dump of the file.
    #[test]
    fn the_enabled_row_speaks_on_and_off_not_kdl_booleans() {
        let mut editor = opened();
        assert_eq!(editor.value(Row::Enabled), "on");
        editor.prefs.enabled = CodemodeEnabled::Auto;
        assert_eq!(editor.value(Row::Enabled), "auto");
        editor.prefs.enabled = CodemodeEnabled::Off;
        assert_eq!(editor.value(Row::Enabled), "off");
    }

    /// The overlay walks the modes in the order the config declares, wrapping
    /// back to the top: every mode has to be reachable from every other.
    #[test]
    fn the_mode_cycles_through_all_three_and_wraps() {
        let mut editor = opened();
        editor.selected = Row::ALL.iter().position(|r| *r == Row::Mode).unwrap();
        // `declare-builtin` is the default, so the cycle starts with it.
        assert_eq!(editor.prefs.mode, CodemodeMode::DeclareBuiltin);

        let mut seen = vec![editor.prefs.mode];
        for _ in 0..CodemodeMode::ALL.len() {
            let Some(CodemodeEffect::Changed(prefs)) = editor.update(CodemodeMessage::Toggle)
            else {
                panic!("expected a change");
            };
            seen.push(prefs.mode);
        }

        assert_eq!(
            seen,
            vec![
                CodemodeMode::DeclareBuiltin,
                CodemodeMode::Only,
                CodemodeMode::Declared,
                CodemodeMode::DeclareBuiltin,
            ],
            "a full cycle visits each mode once and wraps"
        );
    }

    #[test]
    fn the_mode_row_names_the_mode_and_the_note_explains_it() {
        let mut editor = opened();
        // The default mode is the one this configuration exists for.
        assert_eq!(editor.value(Row::Mode), "builtins only");

        editor.prefs.mode = CodemodeMode::Declared;
        assert_eq!(editor.value(Row::Mode), "all tools");
        editor.selected = Row::ALL.iter().position(|r| *r == Row::Mode).unwrap();
        assert!(
            editor.note().contains("tool stays in the roster"),
            "the note says what the mode leaves callable: {}",
            editor.note()
        );

        editor.prefs.mode = CodemodeMode::DeclareBuiltin;
        assert!(
            editor.note().contains("stdio and MCP tools"),
            "the note says what the mode hides: {}",
            editor.note()
        );

        editor.prefs.mode = CodemodeMode::Only;
        assert_eq!(editor.value(Row::Mode), "codemode only");
        assert!(
            editor.note().contains("Only codemode is in the roster"),
            "the note says what the mode hides: {}",
            editor.note()
        );
    }

    /// The row is `prefix(2) + label(24) + value`, and the popup body is 49
    /// columns on an 80-column terminal: a value past 23 is cut off rather than
    /// wrapped, so every value the overlay can show has to fit.
    #[test]
    fn every_value_fits_the_row_inside_the_popup() {
        let mut editor = opened();
        for mode in CodemodeMode::ALL {
            editor.prefs.mode = mode;
            for row in Row::ALL {
                let value = editor.value(row);
                assert!(
                    value.chars().count() <= 23,
                    "`{value}` ({} chars) would be clipped in the {row:?} row",
                    value.chars().count()
                );
            }
        }
    }

    /// With the tool off nothing else matters, so the note says why rather than
    /// describing a setting that cannot take effect.
    #[test]
    fn the_note_says_off_first() {
        let mut editor = opened();
        editor.prefs.enabled = CodemodeEnabled::Off;
        assert!(editor.note().starts_with("Off:"), "{}", editor.note());
    }

    /// `auto` is the default, so its note has to explain the condition rather
    /// than claim the tool is always offered.
    #[test]
    fn the_note_explains_what_auto_waits_for() {
        let mut editor = opened();
        editor.prefs.enabled = CodemodeEnabled::Auto;
        editor.selected = 0;
        assert!(
            editor.note().contains("MCP server"),
            "auto's condition: {}",
            editor.note()
        );
    }

    /// A budget row is shown, not edited: Enter there must not pretend to have
    /// changed anything, or the core would rewrite the config for nothing.
    #[test]
    fn the_budget_rows_do_not_react_to_enter() {
        let mut editor = opened();
        for _ in 0..3 {
            editor.update(CodemodeMessage::Next);
        }
        assert_eq!(editor.selected_row(), Row::InlineBudget);
        assert!(editor.update(CodemodeMessage::Toggle).is_none());

        for _ in 0..3 {
            editor.update(CodemodeMessage::Next);
        }
        assert_eq!(editor.selected_row(), Row::MaxToolCalls);
        assert!(editor.update(CodemodeMessage::Toggle).is_none());
    }

    #[test]
    fn walking_stops_at_both_ends() {
        let mut editor = opened();
        for _ in 0..20 {
            editor.update(CodemodeMessage::Next);
        }
        assert_eq!(editor.selected_row(), Row::MaxToolCalls);

        for _ in 0..20 {
            editor.update(CodemodeMessage::Prev);
        }
        assert_eq!(editor.selected_row(), Row::Enabled);
    }

    #[test]
    fn escape_closes_and_says_so() {
        let mut editor = opened();
        assert!(matches!(
            editor.map_event(&key(KeyCode::Escape)),
            Some(CodemodeMessage::Close)
        ));
        assert!(matches!(
            editor.update(CodemodeMessage::Close),
            Some(CodemodeEffect::Close)
        ));
        assert!(!editor.open);
        // A closed editor ignores what arrives late.
        assert!(editor.update(CodemodeMessage::Toggle).is_none());
    }

    #[test]
    fn the_keys_a_list_is_walked_with_work() {
        let editor = opened();
        assert!(matches!(
            editor.map_event(&key(KeyCode::Down)),
            Some(CodemodeMessage::Next)
        ));
        assert!(matches!(
            editor.map_event(&key(KeyCode::Char('k'))),
            Some(CodemodeMessage::Prev)
        ));
        assert!(matches!(
            editor.map_event(&key(KeyCode::Char(' '))),
            Some(CodemodeMessage::Toggle)
        ));
        assert!(matches!(
            editor.map_event(&key(KeyCode::Enter)),
            Some(CodemodeMessage::Toggle)
        ));
        assert!(editor.map_event(&key(KeyCode::Char('x'))).is_none());
    }

    #[test]
    fn settings_round_trip_from_the_config() {
        let config = CodemodeConfig {
            enabled: CodemodeEnabled::On,
            mode: CodemodeMode::Only,
            store: false,
            inline_budget: 900,
            timeout_ms: 5_000,
            max_operations: 1_000_000,
            max_tool_calls: 25,
            ..CodemodeConfig::default()
        };
        let prefs = CodemodePrefs::from_config(&config);
        assert_eq!(
            prefs,
            CodemodePrefs {
                enabled: CodemodeEnabled::On,
                mode: CodemodeMode::Only,
                store: false,
                inline_budget: 900,
                timeout_ms: 5_000,
                max_operations: 1_000_000,
                max_tool_calls: 25,
            }
        );
    }

    #[test]
    fn a_budget_of_zero_reads_as_unset() {
        let editor = CodemodeEditor::new();
        assert_eq!(editor.value(Row::Timeout), "none");
        assert_eq!(editor.value(Row::MaxOperations), "unlimited");
        assert_eq!(editor.value(Row::MaxToolCalls), "unlimited");
        // A fresh editor shows the shipped defaults: offered, and the roster
        // split that pays for itself.
        assert_eq!(editor.value(Row::Enabled), "on");
    }
}
