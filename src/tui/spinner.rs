use std::sync::OnceLock;
use std::time::Duration;

use frameplay::{FrameTimeReference, Frameplay, FrameplayOptions};
use ratatui::prelude::*;

use super::theme;

const FRAMES: [&str; 6] = ["⠹", "⠼", "⠶", "⠧", "⠏", "⠛"];

const GENERATING_FRAMES: [&str; 18] = [
    "  ⢀⣾⣿⡿⠁  ",
    "  ⠐⣻⣿⣯⠄  ",
    "  ⠚⢛⣿⣥⡤  ",
    " ⠐⠛⠛⣫⣤⣤⠄ ",
    " ⠚⠛⠛⢁⣤⣤⡤ ",
    "⠐⠛⠛⠋ ⣠⣤⣤⠄",
    "⠚⠛⠛⠁ ⢀⣤⣤⡤",
    "⠻⠛⠋   ⣠⣤⣦",
    "⢿⠛⠁   ⢀⣤⣷",
    "⣿⠏     ⣰⣿",
    "⣿⡥     ⢚⣿",
    "⣷⣤⠄   ⠐⠛⢿",
    "⣦⣤⡤   ⠚⠛⠻",
    "⣠⣤⣤⠄ ⠐⠛⠛⠋",
    "⢀⣤⣤⡤ ⠚⠛⠛⠁",
    " ⣠⣤⣤⠔⠛⠛⠋ ",
    " ⢀⣤⣤⡾⠛⠛⠁ ",
    "  ⣠⣴⣿⠟⠋  ",
];

const WAIT_FRAMES: [&str; 17] = [
    "⣿⣿⣿⣿⣉⣉⣉⣉⣹",
    "⣏⣿⣿⣿⣏⣉⣉⣉⣹",
    "⣏⣹⣿⣿⣿⣉⣉⣉⣹",
    "⣏⣉⣿⣿⣿⣏⣉⣉⣹",
    "⣏⣉⣹⣿⣿⣿⣉⣉⣹",
    "⣏⣉⣉⣿⣿⣿⣏⣉⣹",
    "⣏⣉⣉⣹⣿⣿⣿⣉⣹",
    "⣏⣉⣉⣉⣿⣿⣿⣏⣹",
    "⣏⣉⣉⣉⣹⣿⣿⣿⣹",
    "⣏⣉⣉⣉⣉⣿⣿⣿⣿",
    "⣏⣉⣉⣉⣹⣿⣿⣿⣹",
    "⣏⣉⣉⣉⣿⣿⣿⣏⣹",
    "⣏⣉⣉⣹⣿⣿⣿⣉⣹",
    "⣏⣉⣹⣿⣿⣿⣉⣉⣹",
    "⣏⣉⣿⣿⣿⣏⣉⣉⣹",
    "⣏⣹⣿⣿⣿⣉⣉⣉⣹",
    "⣏⣿⣿⣿⣏⣉⣉⣉⣹",
];

const TOOL_FRAMES: [&str; 28] = [
    "         ",
    "⡀       ⢀",
    "⣄       ⣠",
    "⣦⡀     ⢀⣴",
    "⣷⣄     ⣠⣾",
    "⣿⣦⡀   ⢀⣴⣿",
    "⣿⣷⣄   ⣠⣾⣿",
    "⣿⣿⣦⡀ ⢀⣴⣿⣿",
    "⢿⣿⣷⣄ ⣠⣾⣿⡿",
    "⠻⣿⣿⣦⣀⣴⣿⣿⠟",
    "⠙⢿⣿⣷⣤⣾⣿⡿⠋",
    "⠈⠻⣿⣿⣶⣿⣿⠟⠁",
    " ⠙⢿⣿⣿⣿⡿⠋ ",
    "  ⢻⣿⣿⣿⡟  ",
    "  ⢸⣿⣿⣿⡇  ",
    "  ⣼⣿⣿⣿⣧  ",
    " ⣠⣾⣿⣿⣿⣷⣄ ",
    "⢀⣴⣿⣿⠿⣿⣿⣦⡀",
    "⣠⣾⣿⡿⠛⢿⣿⣷⣄",
    "⣴⣿⣿⠟⠉⠻⣿⣿⣦",
    "⣾⣿⡿⠋ ⠙⢿⣿⣷",
    "⣿⣿⠟⠁ ⠈⠻⣿⣿",
    "⣿⡿⠋   ⠙⢿⣿",
    "⣿⠟⠁   ⠈⠻⣿",
    "⡿⠋     ⠙⢿",
    "⠟⠁     ⠈⠻",
    "⠋       ⠙",
    "⠁       ⠈",
];

/// Which spinner a render-loop wake is scheduled for. Each kind animates at
/// its own frame rate, so the loop wakes at the earliest next frame change
/// across the kinds currently visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpinnerKind {
    /// The narrow inline spinner (block headers, sidebar, overlays).
    Inline,
    /// The wide status-row spinner shown while the model is generating.
    Generating,
    /// The wide status-row spinner shown while a tool is running.
    Tool,
    /// The wide status-row spinner shown while a tool is blocked waiting on
    /// the user (e.g. an open `question` prompt).
    Waiting,
}

fn spinner_options(frame_rate: u32) -> FrameplayOptions {
    FrameplayOptions {
        frame_time_reference: FrameTimeReference::StartTime,
        frame_rate,
    }
}

fn frameplay(kind: SpinnerKind) -> &'static Frameplay<&'static str> {
    static INLINE: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    static GENERATING: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    static TOOL: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    static WAITING: OnceLock<Frameplay<&'static str>> = OnceLock::new();

    match kind {
        SpinnerKind::Inline => INLINE.get_or_init(|| Frameplay::new(FRAMES, spinner_options(8))),
        SpinnerKind::Generating => {
            GENERATING.get_or_init(|| Frameplay::new(GENERATING_FRAMES, spinner_options(14)))
        }
        SpinnerKind::Tool => TOOL.get_or_init(|| Frameplay::new(TOOL_FRAMES, spinner_options(14))),
        SpinnerKind::Waiting => {
            WAITING.get_or_init(|| Frameplay::new(WAIT_FRAMES, spinner_options(14)))
        }
    }
}

/// Earliest duration until the next frame change across the active `kinds`.
/// `None` when no spinner is animating.
pub fn next_wake(kinds: impl IntoIterator<Item = SpinnerKind>) -> Option<Duration> {
    kinds
        .into_iter()
        .filter_map(|kind| frameplay(kind).time_to_next_frame())
        .min()
}

/// The current inline spinner frame, advanced by wall-clock time so all
/// spinners stay in sync without mutable state.
pub fn spinner_frame() -> &'static str {
    frameplay(SpinnerKind::Inline).get_frame()
}

/// A styled spinner span in the accent color.
pub fn spinner() -> Span<'static> {
    Span::raw(spinner_frame()).fg(theme::ACCENT)
}

/// The wide status-row spinner shown while the model is generating.
pub fn generating_spinner() -> Span<'static> {
    Span::raw(*frameplay(SpinnerKind::Generating).get_frame()).fg(theme::ACCENT)
}

/// The wide status-row spinner shown while a tool is running.
pub fn tool_spinner() -> Span<'static> {
    Span::raw(*frameplay(SpinnerKind::Tool).get_frame()).fg(theme::ACCENT)
}

/// The wide status-row spinner shown while a tool is blocked waiting on the
/// user (e.g. an open `question` prompt).
pub fn wait_spinner() -> Span<'static> {
    Span::raw(*frameplay(SpinnerKind::Waiting).get_frame()).fg(theme::ACCENT)
}
