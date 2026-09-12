use std::io::{self, Write};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use ratatui::prelude::*;
use termina::{EventStream, PlatformTerminal, Terminal};
use tokio::sync::mpsc::{Receiver, Sender};

use shuvarie_core::{Command, Config, Connections, Event as CoreEvent};

use crate::tui::event::Event;

use self::app::{App, AppEffect, AppMessage};

pub mod add_provider;
mod app;
mod command_menu;
mod commands;
mod components;
mod confirm_quit;
mod context;
mod escape;
mod event;
mod history_search;
mod list;
mod model_picker;
mod question;
mod search;
mod session;
mod session_picker;
mod sidebar;
mod slash;
mod spinner;
mod theme;
mod utils;
mod warning;
mod welcome;
mod workspace;

pub enum TuiResponse {
    SessionSaved { session_id: uuid::Uuid },
}

pub async fn run_tui(
    config: Config,
    cmd_tx: Sender<Command>,
    event_rx: Receiver<CoreEvent>,
) -> io::Result<Option<TuiResponse>> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let event_stream = EventStream::new(reader, |_| true);

    let initial_cols = term.get_dimensions()?.cols;
    let frame_budget = frame_budget(config.ui.frame_rate);
    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    let connections = Connections::load().map_err(|e| io::Error::other(e.to_string()))?;
    let workspace = workspace::WorkspaceInfo::detect();
    let app = App::new(config.ui, connections, cmd_tx, initial_cols, workspace);

    init_terminal(rat.backend_mut().terminal_mut())?;

    let session_id = render_tui(app, &mut rat, event_rx, event_stream, frame_budget).await;

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut());
    let session_id = session_id?;
    deinit?;

    if let Some(session_id) = session_id {
        return Ok(Some(TuiResponse::SessionSaved { session_id }));
    }

    Ok(None)
}

/// Minimum interval between frames. `None` disables the cap (one draw per
/// event, the original behavior).
fn frame_budget(frame_rate: u32) -> Option<Duration> {
    if frame_rate == 0 {
        None
    } else {
        Some(Duration::from_secs_f64(1.0 / frame_rate as f64))
    }
}

/// Helper function for rendering TUI and handling errors.
///
/// All events flow through one scheduler: each wake applies the event (core
/// events are drained as a batch) and then either draws — when the frame
/// budget has elapsed — or arms a deadline at `last_draw + budget` so further
/// events coalesce into the pending frame. Terminal input outranks core
/// events, and input latency is bounded by one frame budget.
async fn render_tui(
    mut app: App,
    rat: &mut ratatui::Terminal<TerminaBackend<PlatformTerminal>>,
    mut event_rx: Receiver<CoreEvent>,
    mut event_stream: EventStream,
    frame_budget: Option<Duration>,
) -> io::Result<Option<uuid::Uuid>>
where
    io::Error: From<<TerminaBackend<PlatformTerminal> as Backend>::Error>,
{
    let mut last_draw: Instant;
    // A pending frame deadline armed when an event arrived too soon after
    // the last draw. `None` means no frame is pending.
    let mut frame_deadline: Option<tokio::time::Instant> = None;
    // Last tab title written to the terminal. Starts empty so the first
    // iteration writes the initial title.
    let mut last_title = String::new();
    // Drives spinner animation: wakes at the earliest next frame change
    // across the spinners currently animating. `None` when none are. Armed
    // after each draw, before it is ever read.
    let mut spinner_wake;

    'render_loop: loop {
        sync_window_title(
            rat.backend_mut().terminal_mut(),
            &mut last_title,
            &app.window_title(),
        )?;

        // Draw frame.
        rat.draw(|frame| app.view(frame, frame.area()))?;
        last_draw = Instant::now();

        // Spinners advance on wall-clock time, so waking at each earliest
        // frame boundary keeps every spinner at its own frame rate.
        spinner_wake = spinner::next_wake(app.active_spinners())
            .map(|until| tokio::time::Instant::now() + until);

        'event_listening: loop {
            let changed = tokio::select! {
                // Flush the armed frame timer: a coalesced frame is due.
                // The `if` guard only disables polling — the future
                // expression is still evaluated, so handle `None` here.
                _ = async {
                    if let Some(deadline) = frame_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if frame_deadline.is_some() => {
                    // Deadline elapsed — draw the coalesced frame.
                    frame_deadline = None;
                    break 'event_listening;
                }
                // Spinner wake: redraw when the next in-progress frame is
                // due. The `if` guard only disables polling — the future
                // expression is still evaluated, so handle `None` here.
                _ = async {
                    if let Some(deadline) = spinner_wake {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if spinner_wake.is_some() => {
                    // Consumed; re-armed from the current state after the
                    // next draw. The wake becomes a message so the animated
                    // models refresh through their `update` paths.
                    spinner_wake = None;
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        Some(AppMessage::SpinnerUpdate),
                    )
                }
                // Terminal event — draws under the same frame budget as core
                // events; when idle the budget has already elapsed, so keys
                // still render immediately.
                ev = event_stream.next() => {
                    let Some(ev_result) = ev else { break 'render_loop Ok(app.session.session_id); };
                    let msg = app.map_event(Event::Terminal(ev_result?));
                    apply_msg(&mut app, rat.backend_mut().terminal_mut(), msg)
                }
                // Core event — drain all already-queued core events as one batch.
                ev = event_rx.recv() => {
                    let Some(ev) = ev else { break 'render_loop Ok(app.session.session_id); };
                    let msg = app.map_event(Event::Core(ev));
                    apply_msg(&mut app, rat.backend_mut().terminal_mut(), msg)
                }
            };

            if app.quit_requested() {
                break 'render_loop Ok(app.session.session_id);
            }

            if !changed {
                // Nothing changed — keep listening without redrawing.
                continue;
            }

            // One draw decision for every event kind.
            match frame_budget {
                None => {
                    // No cap — draw every state change.
                    frame_deadline = None;
                    break 'event_listening;
                }
                Some(budget) => {
                    if last_draw.elapsed() >= budget {
                        // Budget satisfied — draw now.
                        frame_deadline = None;
                        break 'event_listening;
                    }
                    // Arm a deadline at the earliest frame the budget allows
                    // and keep coalescing events until it fires. The deadline
                    // anchors on the last draw, so re-arming during a burst
                    // never pushes it later.
                    frame_deadline = Some(tokio::time::Instant::from_std(last_draw + budget));
                }
            }
        }
    }
}

/// Apply a mapped message, recording a quit request on `AppEffect::Quit` and
/// writing `AppEffect::CopyToClipboard` as OSC 52 (the render loop owns the
/// terminal). Returns `true` if there was a message to apply.
fn apply_msg(app: &mut App, terminal: &mut PlatformTerminal, msg: Option<AppMessage>) -> bool {
    if let Some(msg) = msg {
        match app.update(msg) {
            Some(AppEffect::Quit) => app.mark_quit(),
            Some(AppEffect::CopyToClipboard(text)) => {
                let _ = write!(terminal, "{}", escape::set_clipboard(&text));
                let _ = terminal.flush();
            }
            None => {}
        }
        true
    } else {
        false
    }
}

/// Write the OSC 2 tab title escape when the desired title differs from the
/// last written one. The render loop owns the terminal, so app-driven title
/// changes are applied here rather than from `update`.
fn sync_window_title<W: io::Write>(
    terminal: &mut W,
    last: &mut String,
    title: &str,
) -> io::Result<()> {
    if *last != title {
        write!(terminal, "{}", escape::set_window_title(title))?;
        terminal.flush()?;
        last.clear();
        last.push_str(title);
    }
    Ok(())
}

fn init_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}{}{}{}{}",
        escape::ENTER_ALTERNATE_SCREEN,
        escape::ENABLE_MOUSE,
        escape::ENABLE_SGR_MOUSE,
        escape::ENABLE_KITTY_KEYBOARD,
        escape::ENABLE_BRACKETED_PASTE,
        escape::push_window_title()
    )?;
    terminal.flush()?;
    Ok(())
}

fn deinit_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}{}{}{}{}",
        escape::DISABLE_KITTY_KEYBOARD,
        escape::DISABLE_SGR_MOUSE,
        escape::DISABLE_MOUSE,
        escape::DISABLE_BRACKETED_PASTE,
        escape::EXIT_ALTERNATE_SCREEN,
        escape::pop_window_title()
    )?;
    terminal.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_window_title_writes_only_on_change() {
        let mut out = Vec::new();
        let mut last = String::new();

        sync_window_title(&mut out, &mut last, "Shuvarie").unwrap();
        assert!(!out.is_empty());
        assert_eq!(last, "Shuvarie");

        let unchanged = out.len();
        sync_window_title(&mut out, &mut last, "Shuvarie").unwrap();
        assert_eq!(out.len(), unchanged);

        sync_window_title(&mut out, &mut last, "Shuvarie — fix the bug").unwrap();
        assert!(out.len() > unchanged);
        assert_eq!(last, "Shuvarie — fix the bug");
    }
}
