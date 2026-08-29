use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use shuvarie_core::ApprovalReason;
use termina::event::{KeyCode, KeyEvent};

use super::add_provider::centered_rect;
use super::theme;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApprovalChoice {
    Approve,
    AlwaysApprove,
    Deny,
}

pub enum ApprovalMessage {
    Left,
    Right,
    Confirm,
    Deny,
}

pub enum ApprovalEffect {
    Approve { id: u64 },
    AlwaysApprove { id: u64 },
    Deny { id: u64 },
}

pub struct ApprovalPrompt {
    pub open: bool,
    pub id: u64,
    pub tool: String,
    pub path: String,
    pub reason: ApprovalReason,
    pub choice: ApprovalChoice,
}

impl ApprovalPrompt {
    pub fn new() -> Self {
        Self {
            open: false,
            id: 0,
            tool: String::new(),
            path: String::new(),
            reason: ApprovalReason::OutsideWorkspace,
            choice: ApprovalChoice::Approve,
        }
    }

    pub fn open(&mut self, id: u64, tool: String, path: String, reason: ApprovalReason) {
        self.open = true;
        self.id = id;
        self.tool = tool;
        self.path = path;
        self.reason = reason;
        self.choice = ApprovalChoice::Approve;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ApprovalMessage> {
        match key.code {
            KeyCode::Left => Some(ApprovalMessage::Left),
            KeyCode::Right => Some(ApprovalMessage::Right),
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                Some(ApprovalMessage::Confirm)
            }
            KeyCode::Escape | KeyCode::Char('n') | KeyCode::Char('N') => {
                Some(ApprovalMessage::Deny)
            }
            _ => None,
        }
    }

    pub fn update(&mut self, msg: ApprovalMessage) -> Option<ApprovalEffect> {
        match msg {
            ApprovalMessage::Left => {
                self.choice = match self.choice {
                    ApprovalChoice::Approve => ApprovalChoice::Deny,
                    ApprovalChoice::AlwaysApprove => ApprovalChoice::Approve,
                    ApprovalChoice::Deny => ApprovalChoice::AlwaysApprove,
                };
                None
            }
            ApprovalMessage::Right => {
                self.choice = match self.choice {
                    ApprovalChoice::Approve => ApprovalChoice::AlwaysApprove,
                    ApprovalChoice::AlwaysApprove => ApprovalChoice::Deny,
                    ApprovalChoice::Deny => ApprovalChoice::Approve,
                };
                None
            }
            ApprovalMessage::Confirm => {
                let id = self.id;
                self.close();
                Some(match self.choice {
                    ApprovalChoice::Approve => ApprovalEffect::Approve { id },
                    ApprovalChoice::AlwaysApprove => ApprovalEffect::AlwaysApprove { id },
                    ApprovalChoice::Deny => ApprovalEffect::Deny { id },
                })
            }
            ApprovalMessage::Deny => {
                let id = self.id;
                self.close();
                Some(ApprovalEffect::Deny { id })
            }
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Approval required");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let reason_text = match self.reason {
            ApprovalReason::OutsideWorkspace => "outside the workspace",
            ApprovalReason::HiddenPath => "a hidden path",
            ApprovalReason::Network => "a network request",
        };

        let target_label = match self.reason {
            ApprovalReason::Network => "to ",
            _ => " on ",
        };

        let lines = vec![
            Line::from(""),
            Line::from(vec![
                Span::raw("The model wants to ").fg(theme::TEXT),
                Span::raw(self.tool.clone()).fg(theme::ACCENT).bold(),
                Span::raw(target_label).fg(theme::TEXT),
                Span::raw(self.path.clone()).fg(theme::ACCENT),
            ]),
            Line::from(vec![
                Span::raw("This is ").fg(theme::TEXT),
                Span::raw(reason_text).fg(theme::WARNING),
                Span::raw(".").fg(theme::TEXT),
            ]),
            Line::from(""),
            Line::from(""),
        ];

        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);

        let choices = [
            ("Approve", ApprovalChoice::Approve),
            ("Always approve", ApprovalChoice::AlwaysApprove),
            ("Deny", ApprovalChoice::Deny),
        ];
        let mut spans: Vec<Span> = Vec::new();
        for (i, (label, choice)) in choices.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw("    ").fg(theme::TEXT_MUTED));
            }
            let selected = self.choice == *choice;
            let style = if selected {
                Style::new()
                    .fg(theme::ACCENT)
                    .add_modifier(ratatui::style::Modifier::BOLD)
            } else {
                Style::new().fg(theme::TEXT_MUTED)
            };
            let prefix = if selected { "▶ " } else { "  " };
            spans.push(Span::raw(prefix).fg(theme::ACCENT));
            spans.push(Span::raw(*label).style(style));
        }
        let choice_line = Line::from(spans);
        let choice_area = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1);
        frame.render_widget(
            Paragraph::new(choice_line).alignment(Alignment::Left),
            choice_area,
        );

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("←/→", "choose"),
                ("Enter", "confirm"),
                ("Esc", "deny"),
            ]))
            .fg(theme::TEXT_MUTED)
            .alignment(Alignment::Center),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

impl Default for ApprovalPrompt {
    fn default() -> Self {
        Self::new()
    }
}
