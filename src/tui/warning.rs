use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent, KeyEventKind};

use super::add_provider::centered_rect;
use super::theme;

pub enum WarningMessage {
    Dismiss,
}

/// Transient startup notice (e.g. the configured shell was not found and the
/// default shell is used). Not part of the overlay stack: it paints over
/// every overlay and any key press dismisses it, leaving the underlying
/// screen untouched.
pub struct WarningPopup {
    pub open: bool,
    message: String,
}

impl WarningPopup {
    pub fn new() -> Self {
        Self {
            open: false,
            message: String::new(),
        }
    }

    pub fn open(&mut self, message: String) {
        self.message = message;
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<WarningMessage> {
        if key.kind == KeyEventKind::Press && key.code != KeyCode::CapsLock {
            return Some(WarningMessage::Dismiss);
        }
        None
    }

    pub fn update(&mut self, msg: WarningMessage) {
        match msg {
            WarningMessage::Dismiss => self.close(),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 25, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Warning");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        frame.render_widget(
            Paragraph::new(self.message.clone())
                .style(Style::new().fg(theme::text()))
                .wrap(Wrap { trim: false }),
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(1),
            ),
        );

        frame.render_widget(
            Paragraph::new(theme::help_line(&[("any key", "dismiss")])).fg(theme::text_muted()),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

impl Default for WarningPopup {
    fn default() -> Self {
        Self::new()
    }
}
