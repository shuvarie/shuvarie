use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use super::theme;

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

    /// Height the prompt wants at the given content width (matches the
    /// TextArea's symmetric(2, 1) padding contract).
    pub fn desired_height(&self, width: usize) -> u16 {
        let lines = self.build_lines(width.max(10) as u16, usize::MAX).len();
        (lines.min(MAX_VIEW_ROWS) as u16) + 2
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let block = Block::new()
            .bg(theme::SURFACE)
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
                Span::raw("▸ ").fg(theme::ACCENT),
                Span::raw("Permission required").fg(theme::ACCENT).bold(),
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
                    Span::raw(chunk).fg(theme::TEXT)
                } else {
                    Span::raw(chunk).fg(theme::TEXT_DIM)
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

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split(' ') {
        let mut rest = word;
        while rest.chars().count() > width {
            if !row.is_empty() {
                rows.push(std::mem::take(&mut row));
            }
            let split = rest
                .char_indices()
                .nth(width)
                .map(|(i, _)| i)
                .unwrap_or(rest.len());
            rows.push(rest[..split].to_string());
            rest = &rest[split..];
        }
        if row.is_empty() {
            row = rest.to_string();
        } else if row.chars().count() + 1 + rest.chars().count() <= width {
            row.push(' ');
            row.push_str(rest);
        } else {
            rows.push(std::mem::take(&mut row));
            row = rest.to_string();
        }
    }
    rows.push(row);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_respects_width() {
        let rows = wrap_text("one two three", 8);
        assert_eq!(rows, vec!["one two".to_string(), "three".to_string()]);
        assert!(wrap_text("", 5) == vec![String::new()]);
        let long = "x".repeat(20);
        let rows = wrap_text(&long, 8);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.chars().count() <= 8));
    }
}
