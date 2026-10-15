//! The slash-command guardrail prompt.

use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::theme;
use super::utils::text::wrap_text;

/// Preview rows the held text may take before it is clipped with an ellipsis.
const MAX_PREVIEW_ROWS: usize = 6;

#[derive(Debug, PartialEq, Eq)]
pub enum SlashGuardMessage {
    /// `Enter`: send the held submit as plain prompt text.
    Send,
    /// `Esc`: drop the prompt and hand the text back to the composer.
    Edit,
}

/// The slash-command guardrail: a submit that opens with a trigger char
/// (`/`, `:`) yet resolves to no builtin, custom, or skill command is more
/// likely a typo'd command than prose, so it is held here instead of being
/// sent. The prompt replaces the composer: `Enter` sends the text anyway,
/// `Esc` aborts the send and puts the text back to keep editing.
pub struct SlashGuard {
    /// The held submit's text; `None` while the prompt is closed.
    held: Option<String>,
}

impl SlashGuard {
    pub fn new() -> Self {
        Self { held: None }
    }

    /// Whether the prompt is up (it owns the composer while it is).
    pub fn active(&self) -> bool {
        self.held.is_some()
    }

    /// Hold a submit for confirmation.
    pub fn hold(&mut self, content: String) {
        self.held = Some(content);
    }

    /// Close the prompt, returning the held text (`None` when it was closed).
    pub fn take(&mut self) -> Option<String> {
        self.held.take()
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SlashGuardMessage> {
        if !self.active() {
            return None;
        }
        match key.code {
            KeyCode::Enter => Some(SlashGuardMessage::Send),
            KeyCode::Escape => Some(SlashGuardMessage::Edit),
            _ => None,
        }
    }

    /// Height the prompt wants at the given content width: measured at the
    /// inner width the symmetric(2, 1) padded block paints at.
    pub fn desired_height(&self, width: usize) -> u16 {
        let inner = (width.max(10) as u16).saturating_sub(4);
        (self.build_lines(inner, usize::MAX).len() as u16) + 2
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.active() {
            return;
        }
        let block = Block::new()
            .bg(theme::surface())
            .padding(Padding::symmetric(2, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let lines = self.build_lines(inner.width, inner.height as usize);
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    }

    fn build_lines(&self, width: u16, max_rows: usize) -> Vec<Line<'static>> {
        let Some(held) = &self.held else {
            return Vec::new();
        };
        let width = width.max(10);
        let mut lines = vec![
            Line::from(vec![
                Span::raw("▸ ").fg(theme::warning()),
                Span::raw("This might be an invalid slash command")
                    .fg(theme::warning())
                    .bold(),
            ]),
            Line::from(""),
        ];
        // Echo what Enter would send; the submit is trimmed, so the preview is
        // too. Everything past the row budget collapses into an ellipsis.
        let mut preview = wrap_text(held.trim(), width as usize);
        let clipped = preview.len() > MAX_PREVIEW_ROWS;
        preview.truncate(MAX_PREVIEW_ROWS);
        for (i, row) in preview.into_iter().enumerate() {
            if lines.len() + 1 >= max_rows {
                break;
            }
            if clipped && i + 1 == MAX_PREVIEW_ROWS {
                lines.push(Line::from(
                    Span::raw(format!("{row}…")).fg(theme::text_dim()),
                ));
            } else {
                lines.push(Line::from(Span::raw(row).fg(theme::text_dim())));
            }
        }
        lines.push(theme::help_line(&[
            ("Enter", "send anyway"),
            ("Esc", "keep editing"),
        ]));
        lines
    }
}

impl Default for SlashGuard {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    #[test]
    fn closed_prompt_maps_nothing() {
        let guard = SlashGuard::new();
        assert!(!guard.active());
        assert_eq!(guard.map_event(&key(KeyCode::Enter)), None);
        assert_eq!(guard.desired_height(40), 2, "only the padded block remains");
    }

    #[test]
    fn enter_sends_and_escape_edits() {
        let mut guard = SlashGuard::new();
        guard.hold("/nope".into());
        assert!(guard.active());
        assert_eq!(
            guard.map_event(&key(KeyCode::Enter)),
            Some(SlashGuardMessage::Send)
        );
        assert_eq!(
            guard.map_event(&key(KeyCode::Escape)),
            Some(SlashGuardMessage::Edit)
        );
        assert_eq!(
            guard.map_event(&key(KeyCode::Char('x'))),
            None,
            "the prompt is modal: other keys are swallowed"
        );
        assert_eq!(guard.take().as_deref(), Some("/nope"));
        assert!(!guard.active());
        assert_eq!(guard.take(), None, "closing twice yields nothing");
    }

    #[test]
    fn prompt_lists_the_held_text_and_the_bindings() {
        let mut guard = SlashGuard::new();
        guard.hold("/nonexistent-command".into());
        let lines = guard.build_lines(40, usize::MAX);
        let rendered: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "▸ This might be an invalid slash command".to_string(),
                String::new(),
                "/nonexistent-command".to_string(),
                "Enter send anyway    Esc keep editing".to_string(),
            ]
        );
    }

    #[test]
    fn long_held_text_is_clipped_with_an_ellipsis() {
        let mut guard = SlashGuard::new();
        let long = "/word ".repeat(40);
        guard.hold(long);
        let rows = guard.build_lines(20, usize::MAX);
        // Title, blank, the preview budget, and the help row.
        assert_eq!(rows.len(), MAX_PREVIEW_ROWS + 3);
        let last_preview: String = rows[MAX_PREVIEW_ROWS + 1]
            .spans
            .iter()
            .map(|s| s.content.clone())
            .collect();
        assert!(last_preview.ends_with('…'), "clipped: {last_preview:?}");
    }

    #[test]
    fn height_budget_keeps_the_help_row() {
        let mut guard = SlashGuard::new();
        guard.hold("/nope".into());
        // A tall enough area shows the whole prompt (title, blank, preview,
        // help) plus the padded block's two rows.
        assert_eq!(guard.desired_height(40), 6);
        // A short one drops preview rows rather than the bindings.
        let lines = guard.build_lines(40, 3);
        assert_eq!(lines.len(), 3);
        let help: String = lines[2].spans.iter().map(|s| s.content.clone()).collect();
        assert!(help.contains("send anyway"), "help row: {help:?}");
    }
}
