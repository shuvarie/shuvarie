use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::{LspStatus, Skill};
use shuvarie_llm::TokenUsage;
use termina::event::KeyEvent;

use crate::tui::components::VersionBar;
use crate::tui::sidebar::context::ContextDisplay;

use super::theme;

mod context;

pub struct Sidebar {
    pub version_bar: VersionBar,
    pub provider: Option<String>,
    pub model: Option<String>,
    context: ContextDisplay,
    pub lsp_servers: Vec<LspStatus>,
    pub lsp_enabled: bool,
    pub skills: Vec<Skill>,
    pub todos_done: usize,
    pub todos_total: usize,
    dirty: Cell<bool>,
    lines_cache: RefCell<Vec<Line<'static>>>,
}

pub enum SidebarMessage {
    UpdateConfig {
        provider: Option<String>,
        model: Option<String>,
        context_length: Option<u64>,
    },
    UpdateUsage {
        usage: TokenUsage,
        cost: f64,
        /// The request's context footprint; `None` (worker requests, or a
        /// zero footprint) leaves the sidebar's anchor untouched.
        context_tokens: Option<u64>,
    },
    SetUsage {
        usage: TokenUsage,
        cost: f64,
    },
    /// Replaces (or clears, when `None`) the context-occupancy anchor — the
    /// latest main-request footprint the window percentage is taken against.
    SetContextTokens {
        tokens: Option<u64>,
    },
    UpdateLsp {
        servers: Vec<LspStatus>,
    },
    UpdateSkills {
        skills: Vec<Skill>,
    },
    SetTodos {
        done: usize,
        total: usize,
    },
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            version_bar: VersionBar::new(HorizontalAlignment::Left),
            provider: None,
            model: None,
            context: ContextDisplay::new(),
            lsp_servers: Vec::new(),
            lsp_enabled: true,
            skills: Vec::new(),
            todos_done: 0,
            todos_total: 0,
            dirty: Cell::new(true),
            lines_cache: RefCell::new(Vec::new()),
        }
    }

    pub fn update(&mut self, msg: SidebarMessage) {
        self.dirty.set(true);
        match msg {
            SidebarMessage::UpdateConfig {
                provider,
                model,
                context_length,
            } => {
                self.provider = provider;
                self.model = model;
                self.context.set_context_length(context_length);
            }
            SidebarMessage::UpdateUsage {
                usage,
                cost,
                context_tokens,
            } => {
                self.context.add_usage(&usage, cost, context_tokens);
            }
            SidebarMessage::SetUsage { usage, cost } => {
                self.context.set_usage(&usage, cost);
            }
            SidebarMessage::SetContextTokens { tokens } => {
                self.context.set_context_tokens(tokens);
            }
            SidebarMessage::UpdateLsp { servers } => {
                self.lsp_servers = servers;
            }
            SidebarMessage::UpdateSkills { skills } => {
                self.skills = skills;
            }
            SidebarMessage::SetTodos { done, total } => {
                self.todos_done = done;
                self.todos_total = total;
            }
        }
    }

    #[allow(dead_code)]
    pub fn map_event(&self, _key: &KeyEvent) -> Option<SidebarMessage> {
        None
    }

    /// Mark the cached lines dirty so an animated spinner re-renders.
    pub fn mark_dirty(&self) {
        self.dirty.set(true);
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let block = Block::new()
            .bg(theme::SURFACE)
            .padding(Padding::new(2, 2, 1, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let [version_area, lines_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
                .spacing(1)
                .areas(inner);

        self.version_bar.view(frame, version_area);

        if self.dirty.replace(false) {
            let lines = self.build_lines();
            *self.lines_cache.borrow_mut() = lines;
        }

        let lines = self.lines_cache.borrow().clone();
        let para = Paragraph::new(lines).alignment(Alignment::Left);
        frame.render_widget(para, lines_area);
    }

    #[cfg(test)]
    fn rendered_lines(&self) -> Vec<Line<'static>> {
        if self.dirty.get() {
            *self.lines_cache.borrow_mut() = self.build_lines();
            self.dirty.set(false);
        }
        self.lines_cache.borrow().clone()
    }

    fn build_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();

        if let Some(p) = &self.provider {
            let mut line = vec![Span::raw(p.clone()).fg(theme::TEXT)];
            if let Some(m) = &self.model {
                line.push(Span::raw(":").fg(theme::TEXT_MUTED));
                line.push(Span::raw(m.clone()).fg(theme::TEXT_DIM));
            }
            lines.push(Line::from(line));
            lines.push(Line::from(""));
        }

        self.context.view(&mut lines);

        if self.todos_total > 0 {
            lines.push(Line::from("Todos").fg(theme::ACCENT).bold());
            let done_color = if self.todos_done == self.todos_total {
                theme::SUCCESS
            } else {
                theme::TEXT
            };
            lines.push(Line::from(vec![
                Span::raw("  ").fg(theme::TEXT_MUTED),
                Span::raw(format!("{}", self.todos_done))
                    .fg(done_color)
                    .bold(),
                Span::raw(format!("/{} done", self.todos_total)).fg(theme::TEXT_DIM),
            ]));
            lines.push(Line::from(""));
        }

        lines.push(Line::from("LSP").fg(theme::ACCENT).bold());
        if !self.lsp_enabled {
            lines.push(Line::from("  disabled").fg(theme::TEXT_MUTED));
        } else if self.lsp_servers.is_empty() {
            lines.push(Line::from("  no servers").fg(theme::TEXT_MUTED));
        } else {
            for s in &self.lsp_servers {
                let (marker, color) = match s.status {
                    shuvarie_core::ServerStatus::Running => ("✓", theme::SUCCESS),
                    shuvarie_core::ServerStatus::Starting => ("", theme::WARNING),
                    shuvarie_core::ServerStatus::Stopping => ("", theme::WARNING),
                    shuvarie_core::ServerStatus::Stopped => ("○", theme::TEXT_MUTED),
                    shuvarie_core::ServerStatus::Failed => ("✗", theme::ERROR),
                };
                let mut row = vec![
                    Span::raw("  ").fg(theme::TEXT_MUTED),
                    if matches!(
                        s.status,
                        shuvarie_core::ServerStatus::Starting
                            | shuvarie_core::ServerStatus::Stopping
                    ) {
                        super::spinner::spinner()
                    } else {
                        Span::raw(marker.to_string()).fg(color)
                    },
                    Span::raw(" ").fg(theme::TEXT_MUTED),
                    Span::raw(s.name.clone()).fg(theme::TEXT),
                ];
                if let Some(pid) = s.pid {
                    row.push(Span::raw(format!(" #{pid}")).fg(theme::TEXT_MUTED));
                }
                if s.diagnostics > 0 {
                    row.push(Span::raw(format!("  ⚑{}", s.diagnostics)).fg(theme::WARNING));
                }
                lines.push(Line::from(row));
                if let Some(err) = &s.error {
                    lines.push(
                        Line::from(format!("    {err}"))
                            .fg(theme::TEXT_MUTED)
                            .italic(),
                    );
                }
            }
        }
        lines.push(Line::from(""));

        lines.push(Line::from("Skills").fg(theme::ACCENT).bold());
        if self.skills.is_empty() {
            lines.push(Line::from("  none").fg(theme::TEXT_MUTED));
        } else {
            for skill in &self.skills {
                lines.push(
                    Line::from(format!("  {}", skill.name))
                        .fg(theme::TEXT)
                        .bold(),
                );
            }
        }

        lines
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn todos_section_hidden_when_empty() {
        let sidebar = Sidebar::new();
        assert!(!text(&sidebar.rendered_lines()).contains("Todos"));
    }

    #[test]
    fn todos_section_shows_done_and_total() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetTodos { done: 2, total: 5 });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("Todos"), "body: {rendered}");
        assert!(rendered.contains("2/5 done"), "body: {rendered}");
    }

    #[test]
    fn todos_section_reset_hides_again() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetTodos { done: 3, total: 3 });
        sidebar.update(SidebarMessage::SetTodos { done: 0, total: 0 });
        assert!(!text(&sidebar.rendered_lines()).contains("Todos"));
    }

    #[test]
    fn context_section_shows_tokens_and_cost() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 22_400,
                input_tokens: 10_100,
                output_tokens: 12_300,
                ..Default::default()
            },
            cost: 0.125,
            context_tokens: None,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("↑10.1k"), "body: {rendered}");
        assert!(rendered.contains("↓12.3k"), "body: {rendered}");
        assert!(rendered.contains("Cost $0.12"), "body: {rendered}");
        assert!(!rendered.contains("Think"), "body: {rendered}");
        assert!(!rendered.contains("Cache"), "body: {rendered}");
        assert!(!rendered.contains('%'), "body: {rendered}");
    }

    #[test]
    fn context_section_shows_think_and_cache_when_present() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 11_756,
                input_tokens: 10_100,
                output_tokens: 1_656,
                reasoning_tokens: 12_000,
                cached_input_tokens: 456,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: None,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("Think 12k"), "body: {rendered}");
        assert!(rendered.contains("Cache 456"), "body: {rendered}");
    }

    #[test]
    fn context_section_shows_window_fraction() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            provider: None,
            model: None,
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                output_tokens: 12_300,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(84_000),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("↑0 ↓12.3k"), "body: {rendered}");
        assert!(rendered.contains("84k/200k (42%)"), "body: {rendered}");
    }

    #[test]
    fn context_section_window_line_below_token_line() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            provider: None,
            model: None,
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 10_100,
                output_tokens: 12_300,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(84_000),
        });
        let lines = sidebar.rendered_lines();
        let pos = |needle: &str| {
            lines
                .iter()
                .position(|l| text(std::slice::from_ref(l)).contains(needle))
        };
        let tokens = pos("↑10.1k");
        let window = pos("84k/200k (42%)");
        assert!(tokens.is_some(), "body: {}", text(&lines));
        assert!(window.is_some(), "body: {}", text(&lines));
        assert!(
            window > tokens,
            "window line must come after tokens: {}",
            text(&lines)
        );
        let tokens_line = text(std::slice::from_ref(&lines[tokens.unwrap()]));
        assert!(
            !tokens_line.contains('%'),
            "percentage lives on the window line: {tokens_line:?}"
        );
    }

    #[test]
    fn context_section_window_caps_at_100_percent() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            provider: None,
            model: None,
            context_length: Some(1_048_576),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                output_tokens: 5_000_000,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(5_000_000),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("(100%)"), "body: {rendered}");
        assert!(rendered.contains("5M/1M"), "body: {rendered}");
    }

    #[test]
    fn context_section_live_updates_accumulate_then_snapshot_replaces() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 1_000,
                output_tokens: 500,
                ..Default::default()
            },
            cost: 0.01,
            context_tokens: Some(2_500),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 2_000,
                output_tokens: 1_000,
                ..Default::default()
            },
            cost: 0.02,
            context_tokens: None,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("↑3k ↓1.5k"), "body: {rendered}");
        assert!(rendered.contains("Cost $0.03"), "body: {rendered}");
        sidebar.update(SidebarMessage::SetUsage {
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                ..Default::default()
            },
            cost: 0.5,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("↑10 ↓20"), "body: {rendered}");
        assert!(rendered.contains("Cost $0.50"), "body: {rendered}");
    }

    #[test]
    fn context_section_set_usage_replaces_totals() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 50_000,
                input_tokens: 50_000,
                ..Default::default()
            },
            cost: 1.0,
            context_tokens: Some(84_000),
        });
        sidebar.update(SidebarMessage::SetUsage {
            usage: TokenUsage {
                total_tokens: 10_000,
                input_tokens: 10_000,
                ..Default::default()
            },
            cost: 0.2,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("10k"), "body: {rendered}");
        assert!(rendered.contains("$0.20"), "body: {rendered}");
        assert!(!rendered.contains("50k"), "body: {rendered}");
    }

    #[test]
    fn context_section_worker_usage_keeps_anchor() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            provider: None,
            model: None,
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 84_000,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(84_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 8_000,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: None,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(
            rendered.contains("84k/200k (42%)"),
            "worker usage must not replace the anchor: {rendered}"
        );
    }

    #[test]
    fn context_section_set_context_tokens_clears_anchor() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            provider: None,
            model: None,
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 84_000,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(84_000),
        });
        sidebar.update(SidebarMessage::SetContextTokens { tokens: None });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("200k"), "body: {rendered}");
        assert!(!rendered.contains('%'), "body: {rendered}");
    }
}
