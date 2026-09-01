use std::sync::OnceLock;

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

const TOOL_FRAMES: [&str; 12] = [
    "⣿⣿⣿⣿⣉⣉⣉⣿⣿",
    "⣏⣿⣿⣿⣏⣉⣉⣹⣿",
    "⣏⣹⣿⣿⣿⣉⣉⣉⣿",
    "⣏⣉⣿⣿⣿⣏⣉⣉⣹",
    "⣏⣉⣹⣿⣿⣿⣉⣉⣹",
    "⣏⣉⣉⣿⣿⣿⣏⣉⣹",
    "⣏⣉⣉⣹⣿⣿⣿⣉⣹",
    "⣿⣉⣉⣉⣿⣿⣿⣏⣹",
    "⣿⣏⣉⣉⣹⣿⣿⣿⣹",
    "⣿⣿⣉⣉⣉⣿⣿⣿⣿",
    "⣿⣿⣏⣉⣉⣹⣿⣿⣿",
    "⣿⣿⣿⣉⣉⣉⣿⣿⣿",
];

fn frameplay() -> &'static Frameplay<&'static str> {
    static FP: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    FP.get_or_init(|| {
        Frameplay::new(
            FRAMES,
            FrameplayOptions {
                frame_time_reference: FrameTimeReference::StartTime,
                frame_rate: 10,
            },
        )
    })
}

fn generating_frameplay() -> &'static Frameplay<&'static str> {
    static FP: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    FP.get_or_init(|| {
        Frameplay::new(
            GENERATING_FRAMES,
            FrameplayOptions {
                frame_time_reference: FrameTimeReference::StartTime,
                frame_rate: 14,
            },
        )
    })
}

fn tool_frameplay() -> &'static Frameplay<&'static str> {
    static FP: OnceLock<Frameplay<&'static str>> = OnceLock::new();
    FP.get_or_init(|| {
        Frameplay::new(
            TOOL_FRAMES,
            FrameplayOptions {
                frame_time_reference: FrameTimeReference::StartTime,
                frame_rate: 14,
            },
        )
    })
}

/// The current spinner frame, advanced by wall-clock time so all spinners stay
/// in sync without mutable state.
pub fn spinner_frame() -> &'static str {
    frameplay().get_frame()
}

/// A styled spinner span in the accent color.
pub fn spinner() -> Span<'static> {
    Span::raw(spinner_frame()).fg(theme::ACCENT)
}

/// The wide status-row spinner shown while the model is generating.
pub fn generating_spinner() -> Span<'static> {
    Span::raw(*generating_frameplay().get_frame()).fg(theme::ACCENT)
}

/// The wide status-row spinner shown while a tool is running.
pub fn tool_spinner() -> Span<'static> {
    Span::raw(*tool_frameplay().get_frame()).fg(theme::ACCENT)
}
