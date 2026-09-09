use ratatui::prelude::*;
use shuvarie_highlight::theme;
use shuvarie_llm::TokenUsage;

use crate::tui::utils::num::{fmt_cost, fmt_tokens};

/// The sidebar Context panel: session token usage, estimated cost, and the
/// active model's context window (from the Selune catalog, mapped through the
/// connection's `catalog` field and model id). The window line (below the
/// token totals) shows the context occupancy — the latest main-request
/// footprint
/// (`Event::UsageUpdate.context_tokens`, workers excluded) against the
/// window — and falls back to the bare window size while no footprint is
/// known (fresh or loaded session, during compaction). Display strings come
/// from `utils::num` so token and cost formatting lives in one place.
pub struct ContextDisplay {
    input_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    cached_tokens: u64,
    cost: f64,
    context_length: Option<u64>,
    context_tokens: Option<u64>,
}

impl ContextDisplay {
    pub fn new() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            cached_tokens: 0,
            cost: 0.0,
            context_length: None,
            context_tokens: None,
        }
    }

    pub fn set_context_length(&mut self, context_length: Option<u64>) {
        self.context_length = context_length;
    }

    /// Add one request's usage to the running totals. `context_tokens` is
    /// the request's context footprint; `None` (worker requests, or a zero
    /// footprint) leaves the anchor untouched.
    pub fn add_usage(&mut self, usage: &TokenUsage, cost: f64, context_tokens: Option<u64>) {
        self.input_tokens = self.input_tokens.saturating_add(usage.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(usage.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(usage.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(usage.cached_input_tokens);
        self.cost += cost;
        if let Some(tokens) = context_tokens.filter(|&t| t > 0) {
            self.context_tokens = Some(tokens);
        }
    }

    pub fn set_usage(&mut self, usage: &TokenUsage, cost: f64) {
        self.input_tokens = usage.input_tokens;
        self.output_tokens = usage.output_tokens;
        self.reasoning_tokens = usage.reasoning_tokens;
        self.cached_tokens = usage.cached_input_tokens;
        self.cost = cost;
    }

    /// Replace (or clear, when `None`) the context-occupancy anchor.
    pub fn set_context_tokens(&mut self, tokens: Option<u64>) {
        self.context_tokens = tokens.filter(|&t| t > 0);
    }

    pub fn view(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from("Context").fg(theme::ACCENT).bold());
        let window = self.context_length.filter(|&c| c > 0);
        lines.push(
            Line::from(format!(
                "  ↑{} ↓{}",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens),
            ))
            .fg(theme::TEXT_DIM),
        );
        if let Some(ctx) = window {
            let line = match self.context_tokens {
                Some(tokens) => {
                    let pct = (tokens as f64 / ctx as f64 * 100.0).round().min(100.0);
                    format!("  {}/{} ({pct:.0}%)", fmt_tokens(tokens), fmt_tokens(ctx))
                }
                None => format!("  {}", fmt_tokens(ctx)),
            };
            lines.push(Line::from(line).fg(theme::TEXT_DIM));
        }
        if self.reasoning_tokens > 0 {
            lines.push(
                Line::from(format!("  Think {}", fmt_tokens(self.reasoning_tokens)))
                    .fg(theme::TEXT_DIM),
            );
        }
        if self.cached_tokens > 0 {
            lines.push(
                Line::from(format!("  Cache {}", fmt_tokens(self.cached_tokens)))
                    .fg(theme::TEXT_DIM),
            );
        }
        lines.push(Line::from(format!("  Cost {}", fmt_cost(self.cost))).fg(theme::TEXT_DIM));
        lines.push(Line::from(""));
    }

    /// Dim spans of the essentials — token totals, the context-window
    /// fraction, and the cost — for the collapsed-sidebar status line.
    pub(crate) fn compact_spans(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        spans.push(
            Span::raw(format!(
                "↑{} ↓{}",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens),
            ))
            .fg(theme::TEXT_DIM),
        );
        if let Some(window) = self.context_length.filter(|&c| c > 0) {
            let window_text = match self.context_tokens {
                Some(tokens) => {
                    let pct = (tokens as f64 / window as f64 * 100.0).round().min(100.0);
                    format!(" {}/{} ({pct:.0}%)", fmt_tokens(tokens), fmt_tokens(window))
                }
                None => format!(" {}", fmt_tokens(window)),
            };
            spans.push(Span::raw(window_text).fg(theme::TEXT_DIM));
        }
        spans.push(Span::raw(format!(" {}", fmt_cost(self.cost))).fg(theme::TEXT_DIM));
        spans
    }
}
