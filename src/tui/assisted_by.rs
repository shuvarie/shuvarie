use ratatui::layout::{Constraint::*, Layout, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent};

use shuvarie_core::ModelUsage;

use super::add_provider::centered_rect;
use super::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistedByMessage {
    /// Copy the trailer to the clipboard.
    Copy,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssistedByEffect {
    Copy { content: String },
    Close,
}

/// The `/assisted-by` popup: shows the `Assisted-By:` trailer built from the
/// session's per-model usage (`Enter` copies it, `Escape` closes). The model
/// list is snapshotted at open; a turn committing while the popup is up
/// refreshes the content on the next open.
pub struct AssistedByPopup {
    pub open: bool,
    models: Vec<ModelUsage>,
    copied: bool,
}

impl AssistedByPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            models: Vec::new(),
            copied: false,
        }
    }

    /// Open the popup with the session's deduped per-model usage.
    pub fn open(&mut self, models: Vec<ModelUsage>) {
        self.open = true;
        self.models = models;
        self.copied = false;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.models.clear();
        self.copied = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AssistedByMessage> {
        match key.code {
            KeyCode::Enter => Some(AssistedByMessage::Copy),
            KeyCode::Escape => Some(AssistedByMessage::Cancel),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: AssistedByMessage) -> Option<AssistedByEffect> {
        if !self.open {
            return None;
        }
        match msg {
            AssistedByMessage::Copy => {
                if self.models.is_empty() {
                    return None;
                }
                self.copied = true;
                Some(AssistedByEffect::Copy {
                    content: self.trailer(),
                })
            }
            AssistedByMessage::Cancel => {
                self.close();
                Some(AssistedByEffect::Close)
            }
        }
    }

    /// The trailer line: `Assisted-By: <code>, <code> (Scene) via Shuvarie`.
    /// Codes appear in first-use order; the first non-Default scene a model
    /// served is annotated after it.
    fn trailer(&self) -> String {
        let parts: Vec<String> = self
            .models
            .iter()
            .map(|usage| match &usage.scene {
                Some(scene) => format!("{} ({})", usage.code, scene),
                None => usage.code.clone(),
            })
            .collect();
        format!("Assisted-By: {} via Shuvarie", parts.join(", "))
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(70, 30, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Assisted By");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let [body_area, _spacer, help_area] =
            Layout::vertical([Min(1), Length(1), Length(1)]).areas(inner);

        if self.models.is_empty() {
            frame.render_widget(
                Paragraph::new("No models used yet in this session.").fg(theme::text_muted()),
                body_area,
            );
        } else {
            let line = Line::from(vec![
                Span::styled("Assisted-By: ", Style::new().fg(theme::accent())),
                Span::styled(self.trailer_body(), Style::new().fg(theme::text())),
            ]);
            frame.render_widget(Paragraph::new(line).wrap(Wrap { trim: false }), body_area);
        }

        let help = if self.copied {
            theme::help_line(&[("Enter", "copied"), ("Esc", "close")])
        } else {
            theme::help_line(&[("Enter", "copy"), ("Esc", "close")])
        };
        frame.render_widget(Paragraph::new(help).fg(theme::text_muted()), help_area);
    }

    /// The trailer without the `Assisted-By: ` prefix (the view styles that
    /// part separately).
    fn trailer_body(&self) -> String {
        let parts: Vec<String> = self
            .models
            .iter()
            .map(|usage| match &usage.scene {
                Some(scene) => format!("{} ({})", usage.code, scene),
                None => usage.code.clone(),
            })
            .collect();
        format!("{} via Shuvarie", parts.join(", "))
    }
}

impl Default for AssistedByPopup {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, termina::event::Modifiers::NONE)
    }

    fn usage(code: &str, scene: Option<&str>) -> ModelUsage {
        ModelUsage {
            code: code.to_string(),
            scene: scene.map(str::to_string),
        }
    }

    #[test]
    fn enter_copies_the_trailer_and_keeps_the_popup_open() {
        let mut popup = AssistedByPopup::new();
        popup.open(vec![
            usage("zai-org/glm-5.3-flash", None),
            usage("deepseek-ai/deepseek-v4", Some("Plan")),
        ]);
        assert_eq!(
            popup.update(AssistedByMessage::Copy),
            Some(AssistedByEffect::Copy {
                content: "Assisted-By: zai-org/glm-5.3-flash, \
                          deepseek-ai/deepseek-v4 (Plan) via Shuvarie"
                    .into()
            })
        );
        assert!(popup.open, "copy keeps the popup open");
    }

    #[test]
    fn escape_closes_the_popup() {
        let mut popup = AssistedByPopup::new();
        popup.open(vec![usage("zai-org/glm-5.3-flash", None)]);
        assert_eq!(
            popup.update(AssistedByMessage::Cancel),
            Some(AssistedByEffect::Close)
        );
        assert!(!popup.open);
        assert_eq!(popup.update(AssistedByMessage::Copy), None);
    }

    #[test]
    fn empty_session_copies_nothing() {
        let mut popup = AssistedByPopup::new();
        popup.open(Vec::new());
        assert_eq!(popup.update(AssistedByMessage::Copy), None);
        assert!(popup.open, "an empty popup stays open");
    }

    #[test]
    fn updates_ignored_while_closed() {
        let mut popup = AssistedByPopup::new();
        assert_eq!(popup.update(AssistedByMessage::Copy), None);
        assert_eq!(popup.update(AssistedByMessage::Cancel), None);
    }

    #[test]
    fn map_event_routes_enter_and_escape() {
        let popup = AssistedByPopup::new();
        assert_eq!(
            popup.map_event(&key(KeyCode::Enter)),
            Some(AssistedByMessage::Copy)
        );
        assert_eq!(
            popup.map_event(&key(KeyCode::Escape)),
            Some(AssistedByMessage::Cancel)
        );
        assert_eq!(popup.map_event(&key(KeyCode::Char('q'))), None);
    }
}
