use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::{LspStatus, Skill};
use shuvarie_llm::TodoItem;
use shuvarie_llm::TokenUsage;
use termina::event::KeyEvent;

use crate::tui::{components::VersionBar, utils::locale::ToDecSepNum};

use super::theme;

pub struct Sidebar {
    pub version_bar: VersionBar,
    pub tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub context_length: Option<u64>,
    pub lsp_servers: Vec<LspStatus>,
    pub lsp_enabled: bool,
    pub skills: Vec<Skill>,
    pub todos: Vec<TodoItem>,
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
    },
    SetUsage {
        usage: TokenUsage,
        cost: f64,
    },
    UpdateLsp {
        servers: Vec<LspStatus>,
    },
    UpdateSkills {
        skills: Vec<Skill>,
    },
    UpdateTodos(Vec<TodoItem>),
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            version_bar: VersionBar::new(HorizontalAlignment::Left),
            tokens: 0,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: 0,
            cost: 0.0,
            provider: None,
            model: None,
            context_length: None,
            lsp_servers: Vec::new(),
            lsp_enabled: true,
            skills: Vec::new(),
            todos: Vec::new(),
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
                self.context_length = context_length;
            }
            SidebarMessage::UpdateUsage { usage, cost } => {
                self.tokens = self.tokens.saturating_add(usage.total_tokens);
                self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
                self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
                self.reasoning_tokens =
                    self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
                self.cached_tokens = self.cached_tokens.saturating_add(usage.cached_input_tokens);
                self.cost += cost;
            }
            SidebarMessage::SetUsage { usage, cost } => {
                self.tokens = usage.total_tokens;
                self.input_tokens = usage.input_tokens;
                self.output_tokens = usage.output_tokens;
                self.reasoning_tokens = usage.reasoning_tokens;
                self.cached_tokens = usage.cached_input_tokens;
                self.cost = cost;
            }
            SidebarMessage::UpdateLsp { servers } => {
                self.lsp_servers = servers;
            }
            SidebarMessage::UpdateSkills { skills } => {
                self.skills = skills;
            }
            SidebarMessage::UpdateTodos(todos) => {
                self.todos = todos;
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

        lines.push(Line::from("Context").fg(theme::ACCENT).bold());
        let tokens_line = match self.context_length {
            Some(ctx) if ctx > 0 => {
                let pct = self.tokens as f64 / ctx as f64 * 100.0;
                format!("  Tokens: {} ({pct:.1}%)", self.tokens.to_dec_sep_num(','))
            }
            _ => format!("  Tokens: {}", self.tokens.to_dec_sep_num(',')),
        };
        lines.push(Line::from(tokens_line).fg(theme::TEXT_DIM));
        lines.push(
            Line::from(format!(
                "    ↑{} ↓{}",
                self.input_tokens.to_dec_sep_num(','),
                self.output_tokens.to_dec_sep_num(',')
            ))
            .fg(theme::TEXT_DIM),
        );
        if self.reasoning_tokens > 0 {
            lines.push(
                Line::from(format!(
                    "  Think:  {}",
                    self.reasoning_tokens.to_dec_sep_num(',')
                ))
                .fg(theme::TEXT_DIM),
            );
        }
        if self.cached_tokens > 0 {
            lines.push(
                Line::from(format!(
                    "  Cache:  {}",
                    self.cached_tokens.to_dec_sep_num(',')
                ))
                .fg(theme::TEXT_DIM),
            );
        }
        lines.push(Line::from(format!("  Cost: ${:.2}", self.cost)).fg(theme::TEXT_DIM));
        lines.push(Line::from(""));

        lines.push(Line::from("LSP").fg(theme::ACCENT).bold());
        if !self.lsp_enabled {
            lines.push(Line::from("  disabled").fg(theme::TEXT_MUTED));
        } else if self.lsp_servers.is_empty() {
            lines.push(Line::from("  no servers").fg(theme::TEXT_MUTED));
        } else {
            for s in &self.lsp_servers {
                let (marker, color) = match s.status {
                    shuvarie_core::ServerStatus::Running => ("✓", theme::SUCCESS),
                    shuvarie_core::ServerStatus::Starting => ("⟳", theme::WARNING),
                    shuvarie_core::ServerStatus::Stopping => ("⟳", theme::WARNING),
                    shuvarie_core::ServerStatus::Stopped => ("○", theme::TEXT_MUTED),
                    shuvarie_core::ServerStatus::Failed => ("✗", theme::ERROR),
                };
                let mut row = vec![
                    Span::raw("  ").fg(theme::TEXT_MUTED),
                    Span::raw(marker.to_string()).fg(color),
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
                if !skill.description.is_empty() {
                    lines
                        .push(Line::from(format!("    {}", skill.description)).fg(theme::TEXT_DIM));
                }
            }
        }
        lines.push(Line::from(""));

        lines.push(Line::from("Todo").fg(theme::ACCENT).bold());
        if self.todos.is_empty() {
            lines.push(Line::from("  none").fg(theme::TEXT_MUTED));
        } else {
            let done = self
                .todos
                .iter()
                .filter(|t| t.status == "completed")
                .count();
            let total = self.todos.len();
            lines.push(
                Line::from(format!("  {done}/{total} done")).fg(if done == total {
                    theme::SUCCESS
                } else {
                    theme::TEXT_DIM
                }),
            );
        }

        lines
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}
