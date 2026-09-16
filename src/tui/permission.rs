use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::theme;
use super::utils::text::wrap_text;

pub enum PermissionMessage {
    Allow,
    Deny,
}

pub enum PermissionEffect {
    Decide { id: u64, allow: bool },
}

/// The permission prompt floating above the input while a tool call is
/// paused on an `ask` verdict: Enter (or `y`) allows, Escape (or `n`) denies.
pub struct PermissionUI {
    pub id: u64,
    pub open: bool,
    pub description: String,
}

const MAX_VIEW_ROWS: usize = 10;

impl PermissionUI {
    pub fn new() -> Self {
        Self {
            id: 0,
            open: false,
            description: String::new(),
        }
    }

    pub fn open(&mut self, id: u64, description: String) {
        self.id = id;
        self.open = true;
        self.description = description;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.description.clear();
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<PermissionMessage> {
        if !self.open {
            return None;
        }
        match key.code {
            KeyCode::Enter => Some(PermissionMessage::Allow),
            KeyCode::Escape => Some(PermissionMessage::Deny),
            KeyCode::Char('y') | KeyCode::Char('a') => Some(PermissionMessage::Allow),
            KeyCode::Char('n') | KeyCode::Char('d') => Some(PermissionMessage::Deny),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: PermissionMessage) -> Option<PermissionEffect> {
        if !self.open {
            return None;
        }
        let id = self.id;
        match msg {
            PermissionMessage::Allow => {
                self.close();
                Some(PermissionEffect::Decide { id, allow: true })
            }
            PermissionMessage::Deny => {
                self.close();
                Some(PermissionEffect::Decide { id, allow: false })
            }
        }
    }

    /// Height the prompt wants at the given content width: measured at the
    /// inner width the symmetric(2, 1) padded block paints at.
    pub fn desired_height(&self, width: usize) -> u16 {
        let inner = (width.max(10) as u16).saturating_sub(4);
        let lines = self.build_lines(inner, usize::MAX).len();
        (lines.min(MAX_VIEW_ROWS) as u16) + 2
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
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
        let width = width.max(10);
        let mut lines = vec![
            Line::from(vec![
                Span::raw("▸ ").fg(theme::accent()),
                Span::raw("Permission required").fg(theme::accent()).bold(),
            ]),
            Line::from(""),
        ];
        for text in self.description.lines() {
            if lines.len() + 1 >= max_rows {
                break;
            }
            let mut first = true;
            for chunk in wrap_text(text, width as usize) {
                let styled = if first {
                    Span::raw(chunk).fg(theme::text())
                } else {
                    Span::raw(chunk).fg(theme::text_dim())
                };
                first = false;
                lines.push(Line::from(styled));
                if lines.len() + 1 >= max_rows {
                    break;
                }
            }
        }
        lines.push(theme::help_line(&[("Enter/y", "allow"), ("Esc/n", "deny")]));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_description_rows() {
        let mut ui = PermissionUI::new();
        ui.open(1, "a short note".to_string());
        assert_eq!(ui.build_lines(40, usize::MAX).len(), 4);
    }
}
