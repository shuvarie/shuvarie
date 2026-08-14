use std::io::{self, Write};

use futures_util::StreamExt;
use ratatui::prelude::*;
use termina::{EventStream, PlatformTerminal, Terminal as _};
use tokio::sync::mpsc::{Receiver, Sender};

use shuvarie_core::{Command, Config, Event};

use self::app::{App, AppReturn};

pub mod add_provider;
mod app;
mod command_menu;
mod confirm_quit;
mod context;
mod escape;
mod home;
mod list;
mod logo;
mod model_picker;
mod search;
mod session;
mod sidebar;
mod theme;
mod utils;
mod welcome;
mod widgets;

pub async fn run_tui(cmd_tx: Sender<Command>, mut event_rx: Receiver<Event>) -> io::Result<()> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let mut event_stream = EventStream::new(reader, |_| true);

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    init_terminal(rat.backend_mut().terminal_mut())?;

    let config = Config::load().map_err(|e| io::Error::other(e.to_string()))?;
    let mut app = App::new(config, cmd_tx);

    let res: io::Result<()> = 'render_loop: loop {
        // Draw frame
        rat.draw(|frame| app.view(frame, frame.area()))?;

        'event_listening: loop {
            // Listen to event and map message
            let msg = tokio::select! {
                // Terminal event
                ev = event_stream.next() => {
                    let Some(Ok(ev)) = ev else { break 'render_loop Ok(()); };
                    App::map_event(&ev, &app)
                }
                // Shuvarie core event
                ev = event_rx.recv() => {
                    let Some(ev) = ev else { break 'render_loop Ok(()); };
                    Some(App::map_core_event(ev))
                }
            };

            // Handle message
            if let Some(msg) = msg {
                // Update model and catch return
                if let Some(ret) = app.update(msg) {
                    // Match return
                    match ret {
                        AppReturn::Quit => break 'render_loop Ok(()),
                    }
                }
                // Model updated. Rendering the next frame is required.
                break 'event_listening;
            }
            // Nothing changed. Keep listening.
        }
    };

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut());
    res.and(deinit)
}

fn init_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(terminal, "{}", escape::ENTER_ALTERNATE_SCREEN)?;
    terminal.flush()?;
    Ok(())
}

fn deinit_terminal(terminal: &mut PlatformTerminal) -> io::Result<()> {
    write!(terminal, "{}", escape::EXIT_ALTERNATE_SCREEN)?;
    terminal.flush()?;
    Ok(())
}
