use std::time::{Duration, SystemTime};

use crate::tui::theme;
use ratatui::prelude::*;
use shuvarie_core::peaks::{PeakPhase, PeakSchedule};
use shuvarie_llm::TokenUsage;

use crate::tui::utils::num::{CachedScaledNumber, fmt_cost};
use crate::tui::utils::time::fmt_relative;

/// The sidebar Context panel: session token usage, estimated cost, and the
/// active model's context window (from the Selune catalog, mapped through the
/// connection's `catalog` field and model id). The window line (below the
/// token totals) shows the context occupancy — the latest main-request
/// footprint (from `Event::UsageUpdate`, workers excluded; also restored from
/// a loaded session's persisted request usage) against the window — and falls
/// back to the bare window size while no footprint is known (fresh session, or
/// a provider that has not reported usage yet). Directly below it,
/// the latest main-request read tokens (`R…`, `shuvarie_llm::read_tokens`)
/// and cache-hit percentage (`CH…%`, `cached_input_tokens` over the read
/// tokens) show how the last exchange was served; each half is hidden while
/// unavailable. Display strings come from `utils::num` so token and cost
/// formatting lives in one place.
///
/// Below the cost, a catalog peak/off-peak schedule (provider-wide, or the
/// model's own override) adds a live countdown to the end of the current
/// phase (`Peak ends in 2h`, `Off-Peak ends in 1d 4h`). The schedule is
/// static, so the sidebar holds the phase and the time left in it, refreshed
/// by the render loop's tick. No schedule, no line. The phase label is bold
/// and tinted by what it costs — peak red, off-peak green — while the
/// countdown itself stays dim with the rest of the panel.
pub struct ContextDisplay {
    input_tokens: CachedScaledNumber,
    output_tokens: CachedScaledNumber,
    reasoning_tokens: CachedScaledNumber,
    cached_tokens: CachedScaledNumber,
    cost: f64,
    context_length: Option<CachedScaledNumber>,
    context_tokens: Option<CachedScaledNumber>,
    /// Prompt tokens the latest main request read (`read_tokens` of its
    /// usage); `None` until a main request reports usage (live or restored
    /// from the session's persisted request usage).
    read_tokens: Option<CachedScaledNumber>,
    /// Cache-hit percentage of that request (rounded, capped at 100); only
    /// when the provider reported cached tokens.
    cached_pct: Option<u64>,
    /// The peak/off-peak schedule governing the active model, when the
    /// catalog declares one.
    peaks: Option<PeakSchedule>,
    /// Where that schedule stood at the last refresh: the phase, and the time
    /// left in it. Refreshed by [`Self::set_peaks`] and by the render loop's
    /// tick so `view` renders cached state only.
    peak: Option<(PeakPhase, Duration)>,
}

impl ContextDisplay {
    pub fn new() -> Self {
        Self {
            input_tokens: Default::default(),
            output_tokens: Default::default(),
            reasoning_tokens: Default::default(),
            cached_tokens: Default::default(),
            cost: 0.0,
            context_length: None,
            context_tokens: None,
            read_tokens: None,
            cached_pct: None,
            peaks: None,
            peak: None,
        }
    }

    /// Install (or clear, when `None`) the peak/off-peak schedule that governs
    /// the active model, and refresh the live phase it puts the model in.
    pub fn set_peaks(&mut self, peaks: Option<PeakSchedule>) {
        self.peaks = peaks;
        self.refresh_peak();
    }

    /// Whether a peak/off-peak schedule governs the active model: the render
    /// loop wakes for the countdown only while one does.
    pub fn has_peak_schedule(&self) -> bool {
        self.peaks.is_some()
    }

    /// Recompute the peak phase and the time left in it. The catalog schedule
    /// is static, so this follows the wall clock: the render loop ticks while
    /// a schedule is installed.
    pub fn refresh_peak(&mut self) {
        self.refresh_peak_at(SystemTime::now());
    }

    /// Recompute the peak phase and the time left in it as of `now`.
    pub(crate) fn refresh_peak_at(&mut self, now: SystemTime) {
        self.peak = self
            .peaks
            .as_ref()
            .and_then(|schedule| schedule.status(now))
            .map(|status| (status.phase(), status.remaining(now)));
    }

    pub fn set_context_length(&mut self, context_length: Option<u64>) {
        self.context_length = context_length.map(|n| n.into());
    }

    /// Add one request's usage to the running totals. `context_tokens` is
    /// the request's context footprint; `None` (worker requests, or a zero
    /// footprint) leaves the anchor — and the latest-request read/cache-hit
    /// metrics — untouched.
    pub fn add_usage(&mut self, usage: &TokenUsage, cost: f64, context_tokens: Option<u64>) {
        self.input_tokens = self
            .input_tokens
            .saturating_add(usage.input_tokens.unwrap_or(0));
        self.output_tokens = self
            .output_tokens
            .saturating_add(usage.output_tokens.unwrap_or(0));
        self.reasoning_tokens = self
            .reasoning_tokens
            .saturating_add(usage.reasoning_tokens.unwrap_or(0));
        self.cached_tokens = self
            .cached_tokens
            .saturating_add(usage.cached_input_tokens.unwrap_or(0));
        self.cost += cost;
        if context_tokens.is_some_and(|t| t > 0) {
            self.context_tokens = context_tokens.map(|n| n.into());
            self.set_request_metrics(usage);
        }
    }

    pub fn set_usage(&mut self, usage: &TokenUsage, cost: f64) {
        self.input_tokens = usage.input_tokens.unwrap_or(0).into();
        self.output_tokens = usage.output_tokens.unwrap_or(0).into();
        self.reasoning_tokens = usage.reasoning_tokens.unwrap_or(0).into();
        self.cached_tokens = usage.cached_input_tokens.unwrap_or(0).into();
        self.cost = cost;
    }

    /// Seed (or clear, when `None`) the latest main-request context block:
    /// the occupancy anchor plus the read/cache-hit metrics, all derived from
    /// one request's usage. Used to restore the metrics from a loaded
    /// session's persisted request usage; a zero footprint (or `None`) clears
    /// instead.
    ///
    /// A footprint larger than the window cannot describe one request — rows
    /// written before the reply row carried a per-request payload stored the
    /// run's aggregate, which a multi-call turn sums past the window — so it
    /// is treated as unknown rather than pinning the occupancy at the
    /// ceiling. The window is unknown before the model is resolved; without
    /// it the value is taken as-is.
    pub fn set_request(&mut self, usage: Option<&TokenUsage>) {
        let window = self.context_length.as_ref().map(|length| length.num());
        let usage = usage.filter(|u| {
            let footprint = shuvarie_llm::context_footprint(u);
            footprint > 0 && window.is_none_or(|window| footprint <= window)
        });
        match usage {
            Some(usage) => {
                self.context_tokens = Some(shuvarie_llm::context_footprint(usage).into());
                self.set_request_metrics(usage);
            }
            None => {
                self.context_tokens = None;
                self.read_tokens = None;
                self.cached_pct = None;
            }
        }
    }

    /// Refresh the latest-request read/cache-hit metrics from one request's
    /// usage; the anchor is owned by the caller (`add_usage` keeps the event's
    /// precomputed footprint, `set_request` derives it from the usage).
    fn set_request_metrics(&mut self, usage: &TokenUsage) {
        let read = shuvarie_llm::read_tokens(usage);
        self.read_tokens = (read > 0).then(|| read.into());
        self.cached_pct = match usage.cached_input_tokens {
            Some(cached) if cached > 0 && read > 0 => {
                Some((cached as f64 / read as f64 * 100.0).round().min(100.0) as u64)
            }
            _ => None,
        };
    }

    pub fn view(&self, lines: &mut Vec<Line<'static>>) {
        lines.push(Line::from("Context").fg(theme::accent()).bold());
        let window = self.context_length.as_ref().filter(|c| c.num() > 0);
        lines.push(
            Line::from(format!("  ↑{} ↓{}", self.input_tokens, self.output_tokens,))
                .fg(theme::text_dim()),
        );
        if let Some(ctx) = window {
            let line = match self.context_tokens {
                Some(ref tokens) => {
                    let pct = (tokens.num() as f64 / ctx.num() as f64 * 100.0)
                        .round()
                        .min(100.0);
                    format!("  {}/{} ({pct:.0}%)", tokens, ctx)
                }
                None => format!("  {}", ctx),
            };
            lines.push(Line::from(line).fg(theme::text_dim()));
        }
        let mut request = Vec::new();
        if let Some(ref read) = self.read_tokens {
            request.push(format!("R{}", read));
        }
        if let Some(pct) = self.cached_pct {
            request.push(format!("CH{pct}%"));
        }
        if !request.is_empty() {
            lines.push(Line::from(format!("  {}", request.join(" "))).fg(theme::text_dim()));
        }
        if self.reasoning_tokens.num() > 0 {
            lines.push(
                Line::from(format!("  Think {}", self.reasoning_tokens)).fg(theme::text_dim()),
            );
        }
        if self.cached_tokens.num() > 0 {
            lines.push(Line::from(format!("  Cache {}", self.cached_tokens)).fg(theme::text_dim()));
        }
        lines.push(Line::from(format!("  Cost {}", fmt_cost(self.cost))).fg(theme::text_dim()));
        if let Some((phase, remaining)) = self.peak {
            lines.push(Line::from(peak_spans("  ", phase, remaining)));
        }
        lines.push(Line::from(""));
    }

    /// Dim spans of the essentials — token totals, the context-window
    /// fraction, the latest request's read tokens and cache hit, and the
    /// cost plus the peak countdown — for the collapsed-sidebar status line.
    pub(crate) fn compact_spans(&self) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        spans.push(
            Span::raw(format!("↑{} ↓{}", self.input_tokens, self.output_tokens))
                .fg(theme::text_dim()),
        );
        if let Some(window) = self.context_length.as_ref().filter(|c| c.num() > 0) {
            let window_text = match self.context_tokens {
                Some(ref tokens) => {
                    let pct = (tokens.num() as f64 / window.num() as f64 * 100.0)
                        .round()
                        .min(100.0);
                    format!(" {}/{} ({pct:.0}%)", tokens, window)
                }
                None => format!(" {}", window),
            };
            spans.push(Span::raw(window_text).fg(theme::text_dim()));
        }
        if let Some(ref read) = self.read_tokens {
            spans.push(Span::raw(format!(" R{}", read)).fg(theme::text_dim()));
        }
        if let Some(pct) = self.cached_pct {
            spans.push(Span::raw(format!(" CH{pct}%")).fg(theme::text_dim()));
        }
        spans.push(Span::raw(format!(" {}", fmt_cost(self.cost))).fg(theme::text_dim()));
        if let Some((phase, remaining)) = self.peak {
            spans.extend(peak_spans(" ", phase, remaining));
        }
        spans
    }
}

/// The countdown spans: `prefix`, the phase label, and the wall-clock time
/// left in it. The label is bold and tinted by what the phase means for the
/// bill — red at peak rates, green off-peak — so the state reads at a glance;
/// the countdown stays dim with the rest of the Context panel.
fn peak_spans(prefix: &str, phase: PeakPhase, remaining: Duration) -> Vec<Span<'static>> {
    vec![
        Span::raw(format!("{prefix}{}", phase_label(phase)))
            .fg(phase_color(phase))
            .bold(),
        Span::raw(format!(" ends in {}", fmt_relative(remaining))).fg(theme::text_dim()),
    ]
}

/// The label a countdown carries for its phase.
fn phase_label(phase: PeakPhase) -> &'static str {
    match phase {
        PeakPhase::Peak => "Peak",
        PeakPhase::OffPeak => "Off-Peak",
    }
}

/// The phase label's color: the error tone at peak rates, the success tone
/// off-peak.
fn phase_color(phase: PeakPhase) -> Color {
    match phase {
        PeakPhase::Peak => theme::error(),
        PeakPhase::OffPeak => theme::success(),
    }
}
