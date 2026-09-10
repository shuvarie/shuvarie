use std::cell::{Cell, RefCell};

use ratatui::layout::{Alignment, Rect};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::{LspStatus, ServerStatus, SidebarPref, Skill, SkillWarning};
use shuvarie_llm::TokenUsage;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::components::VersionBar;
use crate::tui::sidebar::context::ContextDisplay;
use crate::tui::utils::{ctrl, text::truncate_spans};

use super::theme;

mod context;

/// Viewport width below which the sidebar auto-collapses when the pref is
/// `auto`. At 80+ columns today's layout is unchanged.
pub const COLLAPSE_BELOW_COLS: u16 = 80;

pub struct Sidebar {
    pub version_bar: VersionBar,
    context: ContextDisplay,
    pub lsp_servers: Vec<LspStatus>,
    pub lsp_enabled: bool,
    pub skills: Vec<Skill>,
    pub skill_warnings: Vec<SkillWarning>,
    pub todos_done: usize,
    pub todos_total: usize,
    /// Default expansion from `[ui] sidebar`.
    pref: SidebarPref,
    /// Manual Ctrl+W override; wins over `pref` until the config changes it.
    manual: Option<bool>,
    /// Last known viewport width, so a toggle can flip the effective state.
    width: u16,
    dirty: Cell<bool>,
    lines_cache: RefCell<Vec<Line<'static>>>,
}

pub enum SidebarMessage {
    UpdateConfig {
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
    /// Seeds (or clears, when `None`) the latest main-request context block:
    /// the occupancy anchor the window percentage is taken against, plus the
    /// read tokens and cache-hit percentage. Restored from the loaded
    /// session's persisted request usage; also used to drop stale metrics.
    SetContextRequest {
        usage: Option<TokenUsage>,
    },
    UpdateLsp {
        servers: Vec<LspStatus>,
    },
    UpdateSkills {
        skills: Vec<Skill>,
        warnings: Vec<SkillWarning>,
    },
    SetTodos {
        done: usize,
        total: usize,
    },
    /// Applies the `[ui] sidebar` pref and clears any manual override.
    SetPref {
        pref: SidebarPref,
    },
    /// Tracks the viewport width so a toggle can flip the effective state.
    SetWidth {
        cols: u16,
    },
    /// Manual collapse/expand override (Ctrl+W): flips the current effective
    /// state and sticks until `SetPref` arrives again.
    Toggle,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            version_bar: VersionBar::new(HorizontalAlignment::Left),
            context: ContextDisplay::new(),
            lsp_servers: Vec::new(),
            lsp_enabled: true,
            skills: Vec::new(),
            skill_warnings: Vec::new(),
            todos_done: 0,
            todos_total: 0,
            pref: SidebarPref::Auto,
            manual: None,
            width: 0,
            dirty: Cell::new(true),
            lines_cache: RefCell::new(Vec::new()),
        }
    }

    pub fn update(&mut self, msg: SidebarMessage) {
        self.dirty.set(true);
        match msg {
            SidebarMessage::UpdateConfig { context_length } => {
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
            SidebarMessage::SetContextRequest { usage } => {
                self.context.set_request(usage.as_ref());
            }
            SidebarMessage::UpdateLsp { servers } => {
                self.lsp_servers = servers;
            }
            SidebarMessage::UpdateSkills { skills, warnings } => {
                self.skills = skills;
                self.skill_warnings = warnings;
            }
            SidebarMessage::SetTodos { done, total } => {
                self.todos_done = done;
                self.todos_total = total;
            }
            SidebarMessage::SetPref { pref } => {
                self.pref = pref;
                self.manual = None;
            }
            SidebarMessage::SetWidth { cols } => {
                self.width = cols;
            }
            SidebarMessage::Toggle => {
                self.manual = Some(!self.collapsed_at(self.width));
            }
        }
    }

    /// Whether the sidebar renders collapsed at the given viewport width: the
    /// manual override wins, then the config pref, then the width rule.
    pub fn collapsed_at(&self, width: u16) -> bool {
        self.manual.unwrap_or(match self.pref {
            SidebarPref::Auto => width < COLLAPSE_BELOW_COLS,
            SidebarPref::Expanded => false,
            SidebarPref::Collapsed => true,
        })
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SidebarMessage> {
        if ctrl(key)
            && key.code == KeyCode::Char('w')
            && key.kind == termina::event::KeyEventKind::Press
        {
            return Some(SidebarMessage::Toggle);
        }
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
    pub(crate) fn rendered_lines(&self) -> Vec<Line<'static>> {
        if self.dirty.get() {
            *self.lines_cache.borrow_mut() = self.build_lines();
            self.dirty.set(false);
        }
        self.lines_cache.borrow().clone()
    }

    /// The one-line stand-in for the collapsed sidebar, shown between the
    /// status row and the key hint: context info (token totals, window
    /// fraction, read tokens, cache hit, cost) and LSP server states — no
    /// skills, no todos. Truncated to `max_width` display columns.
    pub fn collapsed_line(&self, max_width: usize) -> Line<'static> {
        let mut spans = self.context.compact_spans();
        if self.lsp_enabled && !self.lsp_servers.is_empty() {
            spans.push(Span::raw("  │  ").fg(theme::TEXT_MUTED));
            for (i, s) in self.lsp_servers.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("  ").fg(theme::TEXT_MUTED));
                }
                match server_marker(s.status) {
                    Some((marker, color)) => spans.push(Span::raw(marker.to_string()).fg(color)),
                    None => spans.push(super::spinner::spinner()),
                }
                spans.push(Span::raw(" ").fg(theme::TEXT_MUTED));
                spans.push(Span::raw(s.name.clone()).fg(theme::TEXT));
                if s.diagnostics > 0 {
                    spans.push(Span::raw(format!(" ⚑{}", s.diagnostics)).fg(theme::WARNING));
                }
            }
        }
        Line::from(truncate_spans(spans, max_width))
    }

    fn build_lines(&self) -> Vec<Line<'static>> {
        let mut lines: Vec<Line> = Vec::new();

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
                let mut row = vec![
                    Span::raw("  ").fg(theme::TEXT_MUTED),
                    match server_marker(s.status) {
                        Some((marker, color)) => Span::raw(marker.to_string()).fg(color),
                        None => super::spinner::spinner(),
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
                let mut spans = vec![Span::styled(
                    format!("  {}", skill.name),
                    if skill.disable_model_invocation {
                        Style::new().fg(theme::TEXT_MUTED)
                    } else {
                        Style::new().fg(theme::TEXT).bold()
                    },
                )];
                if skill.global {
                    spans.push(Span::styled(
                        " (global)",
                        Style::new().fg(theme::TEXT_MUTED),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
        for warning in &self.skill_warnings {
            let label = warning
                .path
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| warning.path.display().to_string());
            lines.push(
                Line::from(format!("  ! {label}: {}", warning.message))
                    .fg(theme::WARNING)
                    .italic(),
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

/// Status marker for a settled server; `None` while animating (the spinner
/// span stands in).
fn server_marker(status: ServerStatus) -> Option<(&'static str, Color)> {
    match status {
        ServerStatus::Running => Some(("✓", theme::SUCCESS)),
        ServerStatus::Starting | ServerStatus::Stopping => None,
        ServerStatus::Stopped => Some(("○", theme::TEXT_MUTED)),
        ServerStatus::Failed => Some(("✗", theme::ERROR)),
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
    fn context_section_shows_read_and_cache_hit() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 20_200,
                input_tokens: 600,
                output_tokens: 200,
                cached_input_tokens: 19_400,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(20_200),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("R20k CH97%"), "body: {rendered}");
    }

    #[test]
    fn context_section_read_line_between_window_and_cost() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 20_200,
                input_tokens: 600,
                output_tokens: 200,
                cached_input_tokens: 19_400,
                ..Default::default()
            },
            cost: 0.5,
            context_tokens: Some(20_200),
        });
        let lines = sidebar.rendered_lines();
        let pos = |needle: &str| {
            lines
                .iter()
                .position(|l| text(std::slice::from_ref(l)).contains(needle))
        };
        let window = pos("20.2k/200k (10%)");
        let read = pos("R20k CH97%");
        let cost = pos("Cost $0.50");
        assert!(window.is_some(), "body: {}", text(&lines));
        assert!(read.is_some(), "body: {}", text(&lines));
        assert!(cost.is_some(), "body: {}", text(&lines));
        assert!(read > window && cost > read, "order: {}", text(&lines));
    }

    #[test]
    fn context_section_hides_cache_hit_without_cached_tokens() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 20_200,
                input_tokens: 20_000,
                output_tokens: 200,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(20_200),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("R20k"), "body: {rendered}");
        assert!(!rendered.contains("CH"), "body: {rendered}");
    }

    #[test]
    fn context_section_hides_read_and_cache_when_worker_only() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 10_100,
                output_tokens: 200,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: None,
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(!rendered.contains("R10.1k"), "body: {rendered}");
        assert!(!rendered.contains("CH"), "body: {rendered}");
    }

    #[test]
    fn context_section_cache_hit_caps_at_100_percent() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 20_200,
                input_tokens: 20_000,
                output_tokens: 200,
                cached_input_tokens: 21_000,
                ..Default::default()
            },
            cost: 0.0,
            context_tokens: Some(20_200),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("R20k CH100%"), "body: {rendered}");
    }

    #[test]
    fn context_section_window_line_below_token_line() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
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
        assert!(rendered.contains("↑10k"), "body: {rendered}");
        assert!(rendered.contains("$0.20"), "body: {rendered}");
        assert!(!rendered.contains("↑50k"), "body: {rendered}");
        assert!(
            rendered.contains("R50k"),
            "snapshot keeps the latest-request read tokens: {rendered}"
        );
    }

    #[test]
    fn context_section_worker_usage_keeps_anchor() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
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
        assert!(
            rendered.contains("R84k"),
            "worker usage must not replace the read tokens: {rendered}"
        );
    }

    #[test]
    fn context_section_set_context_request_clears_anchor() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
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
        sidebar.update(SidebarMessage::SetContextRequest { usage: None });
        let rendered = text(&sidebar.rendered_lines());
        assert!(rendered.contains("200k"), "body: {rendered}");
        assert!(!rendered.contains('%'), "body: {rendered}");
        assert!(!rendered.contains("R84k"), "body: {rendered}");
    }

    #[test]
    fn context_section_set_context_request_seeds_anchor_and_cache_metrics() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::SetUsage {
            usage: TokenUsage::default(),
            cost: 0.0,
        });
        sidebar.update(SidebarMessage::SetContextRequest {
            usage: Some(TokenUsage {
                input_tokens: 500,
                output_tokens: 200,
                total_tokens: 20_200,
                cached_input_tokens: 19_400,
                ..Default::default()
            }),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(
            rendered.contains("20.2k/200k (10%)"),
            "restored request seeds the anchor: {rendered}"
        );
        assert!(rendered.contains("R20k"), "body: {rendered}");
        assert!(rendered.contains("CH97%"), "body: {rendered}");

        sidebar.update(SidebarMessage::SetContextRequest { usage: None });
        let rendered = text(&sidebar.rendered_lines());
        assert!(
            !rendered.contains("R20k") && !rendered.contains("CH97%"),
            "clearing drops the restored metrics: {rendered}"
        );
    }

    #[test]
    fn context_section_set_context_request_zero_footprint_is_a_clear() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
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
        sidebar.update(SidebarMessage::SetContextRequest {
            usage: Some(TokenUsage::default()),
        });
        let rendered = text(&sidebar.rendered_lines());
        assert!(!rendered.contains('%'), "body: {rendered}");
        assert!(!rendered.contains("R84k"), "body: {rendered}");
    }

    fn lsp_status(name: &str, status: ServerStatus, diagnostics: usize) -> LspStatus {
        LspStatus {
            name: name.to_string(),
            language: name.to_string(),
            status,
            pid: None,
            diagnostics,
            error: None,
        }
    }

    #[test]
    fn collapsed_follows_width_by_default() {
        let sidebar = Sidebar::new();
        assert!(sidebar.collapsed_at(79), "below the threshold collapses");
        assert!(!sidebar.collapsed_at(80), "at the threshold stays expanded");
    }

    #[test]
    fn collapsed_pref_expands_everywhere() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetPref {
            pref: SidebarPref::Expanded,
        });
        assert!(!sidebar.collapsed_at(0));
        assert!(!sidebar.collapsed_at(40));
    }

    #[test]
    fn collapsed_pref_collapses_everywhere() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetPref {
            pref: SidebarPref::Collapsed,
        });
        assert!(sidebar.collapsed_at(0));
        assert!(sidebar.collapsed_at(200));
    }

    #[test]
    fn toggle_overrides_until_pref_reset() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetWidth { cols: 200 });
        sidebar.update(SidebarMessage::Toggle);
        assert!(sidebar.collapsed_at(200), "expanded + toggle collapses");
        sidebar.update(SidebarMessage::Toggle);
        assert!(!sidebar.collapsed_at(200), "toggle back expands");
        sidebar.update(SidebarMessage::SetPref {
            pref: SidebarPref::Auto,
        });
        assert!(!sidebar.collapsed_at(200), "pref reset clears the override");
    }

    #[test]
    fn toggle_on_narrow_screen_expands() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::SetWidth { cols: 40 });
        sidebar.update(SidebarMessage::Toggle);
        assert!(!sidebar.collapsed_at(40));
    }

    #[test]
    fn map_event_toggles_on_ctrl_w() {
        let sidebar = Sidebar::new();
        let key = KeyEvent::new(KeyCode::Char('w'), termina::event::Modifiers::CONTROL);
        assert!(matches!(
            sidebar.map_event(&key),
            Some(SidebarMessage::Toggle)
        ));
        let release = KeyEvent {
            kind: termina::event::KeyEventKind::Release,
            ..KeyEvent::new(KeyCode::Char('w'), termina::event::Modifiers::CONTROL)
        };
        assert!(sidebar.map_event(&release).is_none());
        let plain = KeyEvent::new(KeyCode::Char('w'), termina::event::Modifiers::empty());
        assert!(sidebar.map_event(&plain).is_none());
    }

    #[test]
    fn collapsed_line_shows_context_and_lsp_not_skills() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                input_tokens: 10_100,
                output_tokens: 12_300,
                ..Default::default()
            },
            cost: 0.125,
            context_tokens: Some(84_000),
        });
        sidebar.update(SidebarMessage::SetTodos { done: 2, total: 5 });
        sidebar.update(SidebarMessage::UpdateSkills {
            skills: vec![Skill {
                name: "ratatui".into(),
                path: "/skills/ratatui".into(),
                description: String::new(),
                tags: Vec::new(),
                category: None,
                global: false,
                disable_model_invocation: false,
            }],
            warnings: Vec::new(),
        });
        sidebar.update(SidebarMessage::UpdateLsp {
            servers: vec![
                lsp_status("rust-analyzer", ServerStatus::Running, 3),
                lsp_status("gopls", ServerStatus::Stopped, 0),
            ],
        });
        let line = sidebar.collapsed_line(200);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(rendered.contains("↑10.1k ↓12.3k"), "body: {rendered}");
        assert!(rendered.contains("84k/200k (42%)"), "body: {rendered}");
        assert!(rendered.contains("R10.1k"), "body: {rendered}");
        assert!(!rendered.contains("CH"), "body: {rendered}");
        assert!(rendered.contains("$0.12"), "body: {rendered}");
        assert!(rendered.contains("✓ rust-analyzer ⚑3"), "body: {rendered}");
        assert!(rendered.contains("○ gopls"), "body: {rendered}");
        assert!(!rendered.contains("Skills"), "body: {rendered}");
        assert!(!rendered.contains("Todos"), "body: {rendered}");
    }

    #[test]
    fn collapsed_line_shows_read_and_cache_hit_between_window_and_cost() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateConfig {
            context_length: Some(200_000),
        });
        sidebar.update(SidebarMessage::UpdateUsage {
            usage: TokenUsage {
                total_tokens: 20_200,
                input_tokens: 600,
                output_tokens: 200,
                cached_input_tokens: 19_400,
                ..Default::default()
            },
            cost: 0.125,
            context_tokens: Some(84_000),
        });
        let line = sidebar.collapsed_line(200);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(
            rendered.contains("84k/200k (42%) R20k CH97% $0.12"),
            "body: {rendered}"
        );
    }

    #[test]
    fn collapsed_line_omits_lsp_when_disabled_or_empty() {
        let mut sidebar = Sidebar::new();
        sidebar.lsp_enabled = false;
        sidebar.lsp_servers = vec![lsp_status("rust-analyzer", ServerStatus::Running, 0)];
        let line = sidebar.collapsed_line(200);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!rendered.contains("rust-analyzer"), "body: {rendered}");

        let sidebar = Sidebar::new();
        let line = sidebar.collapsed_line(200);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(!rendered.contains("│"), "body: {rendered}");
    }

    #[test]
    fn collapsed_line_truncates_to_width() {
        let mut sidebar = Sidebar::new();
        sidebar.update(SidebarMessage::UpdateLsp {
            servers: vec![
                lsp_status("rust-analyzer-with-a-long-name", ServerStatus::Running, 0),
                lsp_status(
                    "gopls-and-another-long-server-name",
                    ServerStatus::Running,
                    0,
                ),
            ],
        });
        let line = sidebar.collapsed_line(30);
        let rendered: String = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert!(rendered.chars().count() == 30, "body: {rendered}");
        assert!(rendered.ends_with('…'), "body: {rendered}");
    }
}
