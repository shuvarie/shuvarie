use std::io::{self, Write};

use futures_util::StreamExt;
use ratatui::prelude::*;
use termina::{EventStream, PlatformTerminal, Terminal as _};
use tokio::sync::mpsc::{Receiver, Sender};

use shuvarie_core::{Command, Connections, Event as CoreEvent};

use crate::tui::event::Event;

use self::app::{App, AppEffect};

pub mod add_provider;
mod app;
mod approval;
mod command_menu;
mod components;
mod confirm_quit;
mod context;
mod escape;
mod event;
mod history_search;
mod home;
mod list;
mod logo;
mod model_picker;
mod search;
mod session;
mod session_picker;
mod sidebar;
mod theme;
mod utils;
mod welcome;

pub async fn run_tui(cmd_tx: Sender<Command>, event_rx: Receiver<CoreEvent>) -> io::Result<()> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let event_stream = EventStream::new(reader, |_| true);

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    let connections = Connections::load().map_err(|e| io::Error::other(e.to_string()))?;
    let app = App::new(connections, cmd_tx);

    init_terminal(rat.backend_mut().terminal_mut())?;

    let res: io::Result<()> = render_tui(app, &mut rat, event_rx, event_stream).await;

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut());
    res.and(deinit)
}

/// Helper function for rendering TUI and handling errors
async fn render_tui<B>(
    mut app: App,
    rat: &mut ratatui::Terminal<B>,
    mut event_rx: Receiver<CoreEvent>,
    mut event_stream: EventStream,
) -> io::Result<()>
where
    B: Backend,
    io::Error: From<<B as Backend>::Error>,
{
    'render_loop: loop {
        // Draw frame
        rat.draw(|frame| app.view(frame, frame.area()))?;

        'event_listening: loop {
            // Listen to event and map message
            let msg = tokio::select! {
                // Terminal event
                ev = event_stream.next() => {
                    let Some(ev_result) = ev else { break 'render_loop Ok(()); };
                    app.map_event(Event::Terminal(ev_result?))
                }
                // Shuvarie core event
                ev = event_rx.recv() => {
                    let Some(ev) = ev else { break 'render_loop Ok(()); };
                    app.map_event(Event::Core(ev))
                }
            };

            // Handle message
            if let Some(msg) = msg {
                // Update model and catch return
                if let Some(ret) = app.update(msg) {
                    // Match return
                    match ret {
                        AppEffect::Quit => break 'render_loop Ok(()),
                    }
                }
                // Model updated. Rendering the next frame is required.
                break 'event_listening;
            }
            // Nothing changed. Keep listening.
        }
    }
}

fn init_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}",
        escape::ENTER_ALTERNATE_SCREEN,
        escape::ENABLE_KITTY_KEYBOARD
    )?;
    terminal.flush()?;
    Ok(())
}

fn deinit_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(
        terminal,
        "{}{}",
        escape::DISABLE_KITTY_KEYBOARD,
        escape::EXIT_ALTERNATE_SCREEN
    )?;
    terminal.flush()?;
    Ok(())
}
