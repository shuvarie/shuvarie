use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use termina::event::KeyEvent;

use super::theme;

pub struct Sidebar {
    pub version: String,
    pub tokens: u64,
    pub cost: f64,
    pub provider: Option<String>,
    pub model: Option<String>,
}

pub enum SidebarMessage {
    UpdateConfig {
        provider: Option<String>,
        model: Option<String>,
    },
    #[allow(dead_code)]
    UpdateUsage { tokens: u64, cost: f64 },
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            tokens: 0,
            cost: 0.0,
            provider: None,
            model: None,
        }
    }

    pub fn update(&mut self, msg: SidebarMessage) {
        match msg {
            SidebarMessage::UpdateConfig { provider, model } => {
                self.provider = provider;
                self.model = model;
            }
            SidebarMessage::UpdateUsage { tokens, cost } => {
                self.tokens = tokens;
                self.cost = cost;
            }
        }
    }

    #[allow(dead_code)]
    pub fn map_event(&self, _key: &KeyEvent) -> Option<SidebarMessage> {
        None
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::new()
            .bg(theme::SURFACE)
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::from(vec![
            Span::raw("⚔️ Shuvarie ").fg(theme::ACCENT).bold(),
            Span::raw(format!("v{}", self.version)).fg(theme::TEXT_DIM),
        ]));
        lines.push(Line::from(""));

        if let Some(p) = &self.provider {
            let mut line = vec![Span::raw(p.clone()).fg(theme::TEXT)];
            if let Some(m) = &self.model {
                line.push(Span::raw(":").fg(theme::TEXT_MUTED));
                line.push(Span::raw(m.clone()).fg(theme::TEXT_DIM));
            }
            lines.push(Line::from(line));
            lines.push(Line::from(""));
        }

        lines.push(Line::from("Context").fg(theme::ACCENT).bold());
        lines.push(Line::from(format!("  Tokens: {}", self.tokens)).fg(theme::TEXT_DIM));
        lines.push(Line::from(format!("  Cost: ${:.2}", self.cost)).fg(theme::TEXT_DIM));
        lines.push(Line::from(""));

        lines.push(Line::from("LSP").fg(theme::ACCENT).bold());
        lines.push(Line::from("  inactive").fg(theme::TEXT_MUTED));
        lines.push(Line::from(""));

        lines.push(Line::from("Skills").fg(theme::ACCENT).bold());
        lines.push(Line::from("  inactive").fg(theme::TEXT_MUTED));

        let para = Paragraph::new(lines).alignment(Alignment::Left);
        frame.render_widget(para, inner);
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}
