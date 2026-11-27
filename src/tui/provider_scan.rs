//! The `/scan-providers` confirmation dialog.
//!
//! Selune's catalog names the variable each provider reads its key from, so an
//! exported variable can register that provider without the secret ever being
//! written to `connections.kdl`. The dialog lists what a scan found and asks
//! once: `Enter`/`y` registers them all, `Esc`/`n` walks away.

use ratatui::layout::Alignment;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::layout::Layout;
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use shuvarie_core::DetectedProvider;
use termina::event::{KeyCode, KeyEvent};

use super::{popup, theme};

/// The most provider rows the dialog prints before it summarizes the rest.
const MAX_ROWS: usize = 8;

/// The dialog's answer; `Debug`/`PartialEq` so tests can compare it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderScanMessage {
    /// Register every detection the dialog was opened with.
    Confirm,
    /// Add nothing.
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProviderScanEffect {
    Confirm { detected: Vec<DetectedProvider> },
    Cancel,
}

pub struct ProviderScanPrompt {
    pub open: bool,
    /// What the scan found, snapshotted at open: a variable exported (or
    /// unset) while the dialog is up does not change the question it asks.
    detected: Vec<DetectedProvider>,
}

impl ProviderScanPrompt {
    pub fn new() -> Self {
        Self {
            open: false,
            detected: Vec::new(),
        }
    }

    pub fn open(&mut self, detected: Vec<DetectedProvider>) {
        self.open = true;
        self.detected = detected;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.detected.clear();
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ProviderScanMessage> {
        if !self.open {
            return None;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(ProviderScanMessage::Confirm)
            }
            KeyCode::Escape | KeyCode::Char('n') | KeyCode::Char('N') => {
                Some(ProviderScanMessage::Cancel)
            }
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ProviderScanMessage) -> Option<ProviderScanEffect> {
        if !self.open {
            return None;
        }
        match msg {
            ProviderScanMessage::Confirm => {
                // Nothing detected means there is nothing to confirm; leaving
                // the prompt open keeps the keys from landing on a stray Enter.
                if self.detected.is_empty() {
                    return None;
                }
                let detected = std::mem::take(&mut self.detected);
                self.open = false;
                Some(ProviderScanEffect::Confirm { detected })
            }
            ProviderScanMessage::Cancel => {
                self.close();
                Some(ProviderScanEffect::Cancel)
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let rect = dialog_rect(self.detected.len(), area);
        if rect.width < 2 || rect.height < 2 {
            return;
        }
        popup::dialog(frame, rect, "Scan providers", dimmed, |inner, buf| {
            let [body_area, help_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
            Paragraph::new(self.body_lines())
                .wrap(Wrap { trim: false })
                .render(body_area, buf);
            Paragraph::new(theme::help_line(&[
                ("Enter", "add"),
                ("y", "add"),
                ("Esc", "cancel"),
            ]))
            .fg(theme::text_muted())
            .alignment(Alignment::Center)
            .render(help_area, buf);
        });
    }

    /// The lines the dialog body prints: one row per detection (capped), with
    /// the variable that would be read beside each name.
    fn body_lines(&self) -> Vec<Line<'static>> {
        let count = self.detected.len();
        let mut lines = vec![
            Line::from("These provider keys are set in the environment:"),
            Line::from(""),
        ];
        for provider in self.detected.iter().take(MAX_ROWS) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(provider.name.clone(), Style::new().fg(theme::text())),
                Span::styled(
                    format!("  ${}", provider.env),
                    Style::new().fg(theme::text_muted()),
                ),
            ]));
        }
        if count > MAX_ROWS {
            lines.push(Line::from(Span::styled(
                format!("  … and {} more", count - MAX_ROWS),
                Style::new().fg(theme::text_muted()),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "Add {count} provider{}?",
            if count == 1 { "" } else { "s" }
        )));
        lines
    }
}

impl Default for ProviderScanPrompt {
    fn default() -> Self {
        Self::new()
    }
}

/// The dialog's rect: as tall as its body needs, centred on the page.
fn dialog_rect(rows: usize, area: Rect) -> Rect {
    let printed = rows.min(MAX_ROWS) + usize::from(rows > MAX_ROWS);
    // The title line, the prompt, the blank lines, the question, and the hint
    // bar all sit outside the provider rows.
    let height = (printed as u16 + 6).min(area.height);
    let width = (area.width * 60 / 100).min(area.width);
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn detected(name: &str, env: &str) -> DetectedProvider {
        DetectedProvider {
            catalog_id: name.to_lowercase(),
            name: name.to_string(),
            env: env.to_string(),
            kind: "openai-compat".to_string(),
            base_url: None,
        }
    }

    #[test]
    fn enter_hands_back_every_detection() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(vec![
            detected("Acme", "ACME_API_KEY"),
            detected("Beta", "BETA_API_KEY"),
        ]);
        assert_eq!(
            prompt.map_event(&key(KeyCode::Enter)),
            Some(ProviderScanMessage::Confirm)
        );
        match prompt.update(ProviderScanMessage::Confirm) {
            Some(ProviderScanEffect::Confirm { detected }) => {
                assert_eq!(detected.len(), 2);
                assert_eq!(detected[1].env, "BETA_API_KEY");
            }
            _ => panic!("expected a confirmation"),
        }
        assert!(!prompt.open, "confirming closes the prompt");
        assert!(prompt.detected.is_empty());
    }

    #[test]
    fn escape_adds_nothing() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(vec![detected("Acme", "ACME_API_KEY")]);
        assert_eq!(
            prompt.map_event(&key(KeyCode::Escape)),
            Some(ProviderScanMessage::Cancel)
        );
        assert!(matches!(
            prompt.update(ProviderScanMessage::Cancel),
            Some(ProviderScanEffect::Cancel)
        ));
        assert!(!prompt.open);
    }

    #[test]
    fn y_and_n_are_accepted_too() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(vec![detected("Acme", "ACME_API_KEY")]);
        assert_eq!(
            prompt.map_event(&key(KeyCode::Char('y'))),
            Some(ProviderScanMessage::Confirm)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Char('N'))),
            Some(ProviderScanMessage::Cancel)
        );
        assert_eq!(prompt.map_event(&key(KeyCode::Char('q'))), None);
    }

    #[test]
    fn nothing_detected_never_confirms() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(Vec::new());
        assert_eq!(prompt.update(ProviderScanMessage::Confirm), None);
        assert!(prompt.open);
    }

    #[test]
    fn updates_are_ignored_while_closed() {
        let mut prompt = ProviderScanPrompt::new();
        assert_eq!(prompt.update(ProviderScanMessage::Confirm), None);
        assert_eq!(prompt.update(ProviderScanMessage::Cancel), None);
        assert_eq!(prompt.map_event(&key(KeyCode::Enter)), None);
    }

    #[test]
    fn body_names_each_provider_and_its_variable() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(vec![detected("Acme", "ACME_API_KEY")]);
        let text: String = prompt
            .body_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Acme"), "{text}");
        assert!(text.contains("$ACME_API_KEY"), "{text}");
        assert!(text.contains("Add 1 provider?"), "{text}");
    }

    #[test]
    fn a_long_scan_is_capped_and_summarized() {
        let mut prompt = ProviderScanPrompt::new();
        prompt.open(
            (0..MAX_ROWS + 3)
                .map(|i| detected(&format!("P{i}"), &format!("P{i}_KEY")))
                .collect(),
        );
        let text: String = prompt
            .body_lines()
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("… and 3 more"), "{text}");
        assert!(!text.contains("P8"), "the tail is summarized: {text}");
        assert!(text.contains("Add 11 providers?"), "{text}");
    }

    #[test]
    fn dialog_rect_stays_inside_a_small_page() {
        let area = Rect::new(0, 0, 24, 5);
        let rect = dialog_rect(20, area);
        assert!(rect.height <= area.height);
        assert!(rect.width <= area.width);
        assert_eq!(rect.y, 0);
    }
}
