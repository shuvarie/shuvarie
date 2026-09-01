use std::sync::OnceLock;

use frameplay::{FrameTimeReference, Frameplay, FrameplayOptions};
use ratatui::prelude::*;

use super::theme;

const FRAMES: [&str; 6] = ["⠹", "⠼", "⠶", "⠧", "⠏", "⠛"];

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

/// The current spinner frame, advanced by wall-clock time so all spinners stay
/// in sync without mutable state.
pub fn spinner_frame() -> &'static str {
    frameplay().get_frame()
}

/// A styled spinner span in the accent color.
pub fn spinner() -> Span<'static> {
    Span::raw(spinner_frame()).fg(theme::ACCENT)
}
