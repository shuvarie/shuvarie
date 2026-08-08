use std::io::{self, Write};

use ratatui::prelude::*;
use termina::{
    Event, PlatformTerminal, Terminal,
    event::{KeyCode, KeyEvent},
};

mod escape;

pub enum AppMessage {
    Quit,
}

pub enum AppReturn {
    Quit,
}

#[derive(Debug)]
pub struct App {}

impl App {
    pub fn new() -> Self {
        Self {}
    }

    pub fn handle_event(event: &Event) -> Option<AppMessage> {
        match &event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('q'),
                ..
            }) => Some(AppMessage::Quit),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: AppMessage) -> Option<AppReturn> {
        match msg {
            AppMessage::Quit => Some(AppReturn::Quit),
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget("Press q to quit", area);
    }
}

pub fn run_tui() -> io::Result<()> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    let mut app = App::new();
    // Initialize Termina terminal **after** the Ratatui terminal creation
    init_terminal(rat.backend_mut().terminal_mut())?;

    let res: io::Result<()> = {
        'render_loop: loop {
            rat.draw(|frame| app.view(frame, frame.area()))?;

            let term = rat.backend().terminal();
            'event_listening: loop {
                let event = term.read(|ev| !ev.is_escape())?;
                if let Some(msg) = App::handle_event(&event) {
                    if let Some(ret) = app.update(msg) {
                        // Handle return
                        match ret {
                            AppReturn::Quit => break 'render_loop Ok(()),
                        }
                    }
                    break 'event_listening;
                }
            }
        }
    };

    let deinit_res = deinit_terminal(rat.backend_mut().terminal_mut());
    res.and(deinit_res)
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
