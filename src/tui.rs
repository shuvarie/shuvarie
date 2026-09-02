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
mod welcome;

pub async fn run_tui(
    config: Config,
    cmd_tx: Sender<Command>,
    event_rx: Receiver<CoreEvent>,
) -> io::Result<Option<uuid::Uuid>> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let event_stream = EventStream::new(reader, |_| true);

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    let connections = Connections::load().map_err(|e| io::Error::other(e.to_string()))?;
    let app = App::new(connections, cmd_tx);

    init_terminal(rat.backend_mut().terminal_mut())?;

    let frame_budget = frame_budget(config.ui.frame_rate);

    let session_id = render_tui(app, &mut rat, event_rx, event_stream, frame_budget).await;

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut());
    let session_id = session_id?;
    deinit?;
    Ok(session_id)
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
async fn render_tui<B>(
    mut app: App,
    rat: &mut ratatui::Terminal<B>,
    mut event_rx: Receiver<CoreEvent>,
    mut event_stream: EventStream,
    frame_budget: Option<Duration>,
) -> io::Result<Option<uuid::Uuid>>
where
    B: Backend,
    io::Error: From<<B as Backend>::Error>,
{
    let mut last_draw: Instant;
    // A pending frame deadline armed when an event arrived too soon after
    // the last draw. `None` means no frame is pending.
    let mut frame_deadline: Option<tokio::time::Instant> = None;
    // Drives spinner animation when any in-progress indicator is active.
    let mut spinner_tick = tokio::time::interval(Duration::from_millis(100));
    spinner_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    'render_loop: loop {
        // Draw frame.
        rat.draw(|frame| app.view(frame, frame.area()))?;
        last_draw = Instant::now();

        'event_listening: loop {
            let changed = tokio::select! {
                biased; // cheap branches first
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
                // Spinner tick: redraw when an in-progress indicator is active.
                _ = spinner_tick.tick(), if app.has_active_spinner() => {
                    app.mark_spinners_dirty();
                    true
                }
                // Terminal event — draws under the same frame budget as core
                // events; when idle the budget has already elapsed, so keys
                // still render immediately.
                ev = event_stream.next() => {
                    let Some(ev_result) = ev else { break 'render_loop Ok(app.session.session_id); };
                    let msg = app.map_event(Event::Terminal(ev_result?));
                    apply_msg(&mut app, msg)
                }
                // Core event — drain all already-queued core events as one batch.
                ev = event_rx.recv() => {
                    let Some(ev) = ev else { break 'render_loop Ok(app.session.session_id); };
                    let msg = app.map_event(Event::Core(ev));
                    let mut changed = apply_msg(&mut app, msg);
                    while let Ok(ev) = event_rx.try_recv() {
                        let msg = app.map_event(Event::Core(ev));
                        changed |= apply_msg(&mut app, msg);
                    }
                    changed
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

/// Apply a mapped message, recording a quit request on `AppEffect::Quit`.
/// Returns `true` if there was a message to apply.
fn apply_msg(app: &mut App, msg: Option<AppMessage>) -> bool {
    if let Some(msg) = msg {
        if let Some(AppEffect::Quit) = app.update(msg) {
            app.mark_quit();
        }
        true
    } else {
        false
    }
}

fn init_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}{}{}",
        escape::ENTER_ALTERNATE_SCREEN,
        escape::ENABLE_MOUSE,
        escape::ENABLE_SGR_MOUSE,
        escape::ENABLE_KITTY_KEYBOARD
    )?;
    terminal.flush()?;
    Ok(())
}

fn deinit_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}{}{}",
        escape::DISABLE_KITTY_KEYBOARD,
        escape::DISABLE_SGR_MOUSE,
        escape::DISABLE_MOUSE,
        escape::EXIT_ALTERNATE_SCREEN
    )?;
    terminal.flush()?;
    Ok(())
}
