use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Paragraph, Wrap};
use termina::event::{KeyCode, KeyEvent, KeyEventKind};

use super::{add_provider::centered_rect, popup, theme};

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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 25, area);
        popup::dialog(frame, popup, "Warning", dimmed, |inner, buf| {
            let [body_area, help_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
            Paragraph::new(self.message.as_str())
                .style(Style::new().fg(theme::text()))
                .wrap(Wrap { trim: false })
                .render(body_area, buf);
            Paragraph::new(theme::help_line(&[("any key", "dismiss")]))
                .fg(theme::text_muted())
                .render(help_area, buf);
        });
    }
}

impl Default for WarningPopup {
    fn default() -> Self {
        Self::new()
    }
}
