use std::io::{self, Write};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use ratatui::prelude::*;
use termina::{
    EventReader, EventStream, PlatformTerminal, Terminal,
    escape::csi::{Csi, Keyboard, Window},
    event::Event as TerminalEvent,
};
use tokio::sync::mpsc::{Receiver, Sender};

use shuvarie_core::{Command, Config, Connections, Event as CoreEvent, ThemeSet};

use crate::tui::event::Event;

use self::app::{App, AppEffect, AppMessage};

pub mod add_decision_provider;
pub mod add_provider;
mod app;
mod assisted_by;
mod auth;
mod command_menu;
mod commands;
mod components;
mod confirm_quit;
mod context;
mod escape;
mod event;
/// Filesystem watch on the workspace's git `HEAD` file: the sidebar branch
/// label refreshes on checkouts made outside the TUI.
mod git_watch;
mod history_search;
mod list;
mod model_picker;
mod permission;
mod popup;
mod question;
mod registry;
mod scene;
mod search;
mod session;
mod session_picker;
mod sidebar;
mod slash;
mod spinner;
mod theme;
mod theme_picker;
mod title;
mod todo;
pub mod trust;
mod utils;
mod variant;
mod viewer;
mod warning;
mod welcome;
mod workspace;

pub enum TuiResponse {
    SessionSaved { session_id: uuid::Uuid },
}

pub async fn run_tui(
    config: Config,
    theme_set: ThemeSet,
    cmd_tx: Sender<Command>,
    event_rx: Receiver<CoreEvent>,
) -> io::Result<Option<TuiResponse>> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let detected = self::theme::detect_variant(&mut term, &reader);
    let theme = theme_set.resolve(config.ui.theme.as_deref(), detected);
    let theme_choices = theme_set.choices(detected);
    let event_stream = EventStream::new(reader.clone(), |_| true);

    let window = term.get_dimensions()?;
    let initial_cols = window.cols;
    let derived_cell_size = match (window.pixel_width, window.pixel_height) {
        (Some(pixel_width), Some(pixel_height)) => {
            let width = pixel_width / window.cols.max(1);
            let height = pixel_height / window.rows.max(1);
            (width > 0 && height > 0).then_some((width, height))
        }
        _ => None,
    };
    let frame_budget = frame_budget(config.ui.frame_rate);
    let mut ui = config.ui;
    if ui.image.cell_size.is_none()
        && let Some(cell_size) = derived_cell_size
    {
        ui.image.cell_size = Some(cell_size);
    }
    // Graphical protocols need honest cell metrics to place images: when
    // the window ioctl report had no pixel dimensions, ask the terminal
    // directly (`CSI 16 t`) — kitty, Ghostty, foot, iTerm2, WezTerm and
    // Konsole all answer, and they are exactly the terminals whose graphic
    // protocol the auto-detection below selects.
    // Auto (`protocol` unset) detects from the terminal environment and
    // writes the choice back into the prefs — the App maps the resolved
    // value into both the chat pane's and the viewer's render path, so the
    // detection governs rendering, not only the probe below.
    let effective_image_protocol = *ui
        .image
        .protocol
        .get_or_insert_with(shuvarie_core::ImageProtocol::detect);
    if ui.image.cell_size.is_none()
        && !matches!(
            effective_image_protocol,
            shuvarie_core::ImageProtocol::Halfblocks
        )
    {
        ui.image.cell_size = probe_cell_size(&mut term, &reader)?;
    }
    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    let connections = Connections::load().map_err(|e| io::Error::other(e.to_string()))?;
    let workspace = workspace::WorkspaceInfo::detect();
    // Watch the workspace's `HEAD` file for the sidebar's branch label; the
    // watch lives as long as the render loop (`branch_watch` is consumed by
    // `render_tui`).
    let branch_watch = self::git_watch::BranchWatch::start(&workspace.path);
    let app = App::new(
        ui,
        theme,
        theme_choices,
        config.registries.clone(),
        connections,
        cmd_tx,
        initial_cols,
        workspace,
    );

    init_terminal(rat.backend_mut().terminal_mut())?;

    // Decide how modified keys reach the TUI before the render loop starts
    // consuming events: the probe shares the reader, so any keystrokes typed
    // during the wait stay buffered and are delivered by the stream later.
    let modify_other_keys = probe_keyboard_protocol(rat.backend_mut().terminal_mut(), &reader)?;

    let session_id = render_tui(
        app,
        &mut rat,
        event_rx,
        event_stream,
        frame_budget,
        branch_watch,
    )
    .await;

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut(), modify_other_keys);
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
    mut branch_watch: git_watch::BranchWatch,
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
    // Session picker list refresh: while the picker is open the list is
    // re-fetched periodically so lock states stay live.
    let mut picker_wake;
    const PICKER_REFRESH: Duration = Duration::from_secs(2);

    let session_id = 'render_loop: loop {
        sync_window_title(
            rat.backend_mut().terminal_mut(),
            &mut last_title,
            &app.window_title(),
        )?;

        // Draw frame, then hand the painted area to the overlays: resize
        // events only arrive on SIGWINCH, so a popup opened before the first
        // one would otherwise scroll its list against a zero-height viewport.
        let frame = rat.draw(|frame| app.view(frame, frame.area()))?;
        app.set_viewport(frame.area);
        last_draw = Instant::now();

        // Spinners advance on wall-clock time, so waking at each earliest
        // frame boundary keeps every spinner at its own frame rate.
        spinner_wake = spinner::next_wake(app.active_spinners())
            .map(|until| tokio::time::Instant::now() + until);
        picker_wake = app
            .session_picker
            .open
            .then(|| tokio::time::Instant::now() + PICKER_REFRESH);

        // Media transmissions (kitty) and placed graphics (sixel/iTerm2 in
        // the fullscreen viewer) reach the terminal between frames, never
        // through the cell diff. The pass also fills the render entries
        // paint reads from (at this frame's width): paint stays pure. A
        // draw that delivered a payload is immediately followed by one
        // empty-diff draw: the terminal then re-renders its grid with the
        // now-transmitted graphic in place.
        if app.after_frame(frame.area, rat.backend_mut().terminal_mut())? {
            continue;
        }

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
                _ = async {
                    if let Some(deadline) = picker_wake {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if picker_wake.is_some() => {
                    picker_wake = Some(tokio::time::Instant::now() + PICKER_REFRESH);
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        Some(AppMessage::PickerRefresh),
                    )
                }
                // Terminal event — draws under the same frame budget as core
                // events; when idle the budget has already elapsed, so keys
                // still render immediately.
                ev = event_stream.next() => {
                    let Some(ev_result) = ev else { break 'render_loop app.session.session_id; };
                    let msg = app.map_event(Event::Terminal(ev_result?));
                    apply_msg(&mut app, rat.backend_mut().terminal_mut(), msg)
                }
                // Core event — drain all already-queued core events as one batch.
                ev = event_rx.recv() => {
                    let Some(ev) = ev else { break 'render_loop app.session.session_id; };
                    let msg = app.map_event(Event::Core(ev));
                    apply_msg(&mut app, rat.backend_mut().terminal_mut(), msg)
                }
                // Git branch watcher — the workspace's `.git/HEAD` file was
                // touched (a checkout or branch switch elsewhere); the
                // sidebar re-reads the branch label.
                _ = branch_watch.next() => {
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        Some(AppMessage::BranchChanged),
                    )
                }
            };

            if app.quit_requested() {
                break 'render_loop app.session.session_id;
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
    };

    // Empty the terminal's kitty image cache: images the session displayed
    // outlive the process otherwise.
    app.write_kitty_shutdown_deletes(rat.backend_mut().terminal_mut())?;
    Ok(session_id)
}

/// Apply a mapped message, recording a quit request on `AppEffect::Quit`,
/// writing `AppEffect::CopyToClipboard` as OSC 52 (the render loop owns the
/// terminal), and launching the browser for `AppEffect::OpenBrowser`.
fn apply_msg(app: &mut App, terminal: &mut PlatformTerminal, msg: Option<AppMessage>) -> bool {
    if let Some(msg) = msg {
        match app.update(msg) {
            Some(AppEffect::Quit) => app.mark_quit(),
            Some(AppEffect::CopyToClipboard(text)) => {
                let _ = write!(terminal, "{}", escape::set_clipboard(&text));
                let _ = terminal.flush();
            }
            Some(AppEffect::OpenBrowser(url)) => auth::open_in_browser(&url),
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

fn deinit_terminal(
    terminal: &mut PlatformTerminal,
    reset_modify_other_keys: bool,
) -> io::Result<()> {
    write!(
        terminal,
        "{}{}{}{}{}{}{}",
        escape::DISABLE_KITTY_KEYBOARD,
        if reset_modify_other_keys {
            escape::RESET_MODIFY_OTHER_KEYS
        } else {
            ""
        },
        escape::DISABLE_SGR_MOUSE,
        escape::DISABLE_MOUSE,
        escape::DISABLE_BRACKETED_PASTE,
        escape::EXIT_ALTERNATE_SCREEN,
        escape::pop_window_title()
    )?;
    terminal.flush()?;
    Ok(())
}

/// Time to wait for a kitty keyboard protocol query answer before treating the
/// terminal as not speaking the protocol.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

/// Decide how modified keys reach the TUI.
///
/// `init_terminal` pushes kitty keyboard protocol flags, which real
/// kitty-protocol terminals honor: they answer the `CSI ? u` query below and
/// send modified keys as CSI-u sequences (e.g. Shift+Enter as `\x1b[13;2u`),
/// which termina parses into modifier-carrying key events.
///
/// tmux does not implement the kitty protocol toward panes: it silently
/// ignores both the flag push and the query, and without help it strips the
/// modifiers from keys that have no legacy encoding — Shift+Enter arrives as
/// plain `\r`, indistinguishable from Enter. A silent terminal therefore gets
/// an xterm `modifyOtherKeys` request (`CSI > 4 ; Pv m`); tmux tracks it and
/// starts forwarding modified keys as CSI-u when its `extended-keys` server
/// option is on and `extended-keys-format` is `csi-u` (tmux 3.5+; both default
/// to `off`/`xterm`). The level comes from `modify_other_keys_request`: level 2
/// when the modified keys will arrive as CSI-u, so chords like Ctrl+M stop
/// colliding with their legacy control bytes without Ctrl+C losing its `\x03`.
///
/// Returns `true` when the kitty protocol is available and `false` when the
/// modifyOtherKeys fallback was requested, so `deinit_terminal` can reset it.
fn probe_keyboard_protocol(term: &mut PlatformTerminal, reader: &EventReader) -> io::Result<bool> {
    write!(term, "{}", escape::QUERY_KITTY_FLAGS)?;
    term.flush()?;
    if let Ok(true) = reader.poll(Some(PROBE_TIMEOUT), kitty_flags_report) {
        // Consume the report so it never surfaces as a stray CSI event;
        // keystrokes typed during the wait stay buffered for the stream.
        let _ = reader.read(kitty_flags_report)?;
        return Ok(true);
    }
    write!(term, "{}", modify_other_keys_request(tmux_forwards_csi_u()))?;
    term.flush()?;
    Ok(false)
}

/// The `modifyOtherKeys` level to request from a terminal that stayed silent
/// during the kitty probe.
///
/// Level 2 is worth asking for only where modified keys come back as CSI-u:
/// tmux with `extended-keys on` and `extended-keys-format csi-u` then forwards
/// every modified chord that way, so Ctrl+M arrives as `CSI 109;5u` rather than
/// the CR that Enter also sends — level 1 deliberately leaves chords with a
/// legacy control byte alone, which is why the Ctrl+M command-menu binding
/// stayed dead under tmux. `tmux_forwards_csi_u` is what establishes that.
///
/// Everywhere else level 1 stands: extended keys arrive in the xterm
/// `CSI 27 ; mod ; code ~` form that termina's parser drops, so level 2 would
/// turn working legacy chords (Ctrl+C, Ctrl+J, ...) into sequences nothing can
/// decode — a quit binding that stops quitting, to fix one binding that still
/// works as Enter.
fn modify_other_keys_request(csi_u_extended_keys: bool) -> Csi {
    if csi_u_extended_keys {
        escape::REQUEST_MODIFY_OTHER_KEYS_LEVEL2
    } else {
        escape::REQUEST_MODIFY_OTHER_KEYS_LEVEL1
    }
}

/// Whether tmux will encode the extended keys we are about to ask for as CSI-u
/// — the one encoding termina's parser understands.
///
/// tmux's `extended-keys-format` is a *server* option a pane cannot change: the
/// format is the user's choice, and it defaults to `xterm` (alongside
/// `extended-keys off`). Only `csi-u` output is worth asking level 2 for. At
/// `xterm` the extended keys arrive as `CSI 27 ; mod ; code ~`, which termina
/// drops, so level 2 would turn Ctrl+C into an undecodable sequence where level
/// 1 leaves its `\x03` alone — a quit binding that stops quitting, to fix one
/// binding that still reaches Enter.
///
/// `extended-keys` itself is deliberately not consulted: with it `off` tmux
/// ignores the level request entirely (measured: keys byte-identical to no
/// request), and with it `on` or `always` the format above decides how keys
/// arrive. Requiring it to be on would only strand users who enable extended
/// keys mid-session.
///
/// An option tmux does not know reads as `None` — versions predating
/// `extended-keys-format` (tmux 3.5) fold modifyOtherKeys levels 1 and 2 into
/// one extended mode, so the lower level is equivalent there rather than worse.
fn tmux_forwards_csi_u() -> bool {
    in_tmux() && tmux_format_is_csi_u(tmux_option("extended-keys-format").as_deref())
}

/// Whether tmux's `extended-keys-format` says modified keys come back as CSI-u.
fn tmux_format_is_csi_u(format: Option<&str>) -> bool {
    format == Some("csi-u")
}

/// Read one tmux server option (`tmux show-options -sv <name>`), without the
/// trailing newline. `None` when tmux cannot be run or does not know the
/// option. The child inherits `TMUX`, so it addresses the server this pane
/// belongs to.
fn tmux_option(name: &str) -> Option<String> {
    let out = std::process::Command::new("tmux")
        .args(["show-options", "-sv", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8(out.stdout).ok()?;
    Some(value.trim().to_owned())
}

/// Whether the TUI runs inside a tmux pane (`$TMUX` is set).
fn in_tmux() -> bool {
    std::env::var_os("TMUX").is_some()
}

/// Matches the kitty keyboard flags report (`CSI ? flags u`). Non-matching
/// events are retained by the reader for later consumers.
fn kitty_flags_report(event: &TerminalEvent) -> bool {
    matches!(
        event,
        TerminalEvent::Csi(Csi::Keyboard(Keyboard::ReportFlags(_)))
    )
}

/// Ask the terminal for its cell size in pixels (`CSI 16 t`) and return the
/// report — the image renderers' font metrics. `None` when the terminal
/// stays silent (or reports unusable values): rendering then keeps the
/// 8×16 fallback. Answered by the kitty-graphics-family terminals, exactly
/// the ones the auto-detected protocols select.
fn probe_cell_size(
    term: &mut PlatformTerminal,
    reader: &EventReader,
) -> io::Result<Option<(u16, u16)>> {
    write!(term, "{}", escape::QUERY_CELL_SIZE_PX)?;
    term.flush()?;
    if let Ok(true) = reader.poll(Some(PROBE_TIMEOUT), cell_size_report) {
        // Consume the report so it never surfaces as a stray CSI event.
        return Ok(match reader.read(cell_size_report)? {
            TerminalEvent::Csi(Csi::Window(window)) => match window.as_ref() {
                Window::ReportCellSizePixelsResponse {
                    width: Some(width),
                    height: Some(height),
                } if *width > 0
                    && *height > 0
                    && *width <= u16::MAX as i64
                    && *height <= u16::MAX as i64 =>
                {
                    Some((*width as u16, *height as u16))
                }
                _ => None,
            },
            _ => None,
        });
    }
    Ok(None)
}

/// Matches the cell-size report (`CSI 6 ; height ; width t`). Non-matching
/// events are retained by the reader for later consumers.
fn cell_size_report(event: &TerminalEvent) -> bool {
    matches!(
        event,
        TerminalEvent::Csi(Csi::Window(window))
            if matches!(
                **window,
                Window::ReportCellSizePixelsResponse { .. }
            )
    )
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

    #[test]
    fn keyboard_protocol_probe_sequences() {
        assert_eq!(escape::QUERY_KITTY_FLAGS.to_string(), "\x1b[?u");
        // The modifyOtherKeys request is built from termina's typed
        // key-modifier resource command; the bytes must match xterm's
        // `CSI > 4 ; Pv m` exactly for tmux to act on it.
        assert_eq!(
            escape::REQUEST_MODIFY_OTHER_KEYS_LEVEL1.to_string(),
            "\x1b[>4;1m"
        );
        assert_eq!(
            escape::REQUEST_MODIFY_OTHER_KEYS_LEVEL2.to_string(),
            "\x1b[>4;2m"
        );
        assert_eq!(escape::RESET_MODIFY_OTHER_KEYS, "\x1b[>4n");
        assert_eq!(escape::QUERY_CELL_SIZE_PX.to_string(), "\x1b[16t");
    }

    #[test]
    fn tmux_asks_for_modify_other_keys_level_two() {
        // Level 2 is what makes tmux send Ctrl+M as CSI-u instead of CR, so it
        // is only requested once the keys are known to come back that way.
        assert_eq!(modify_other_keys_request(true).to_string(), "\x1b[>4;2m");
        // Everything else keeps level 1: extended keys use the xterm format
        // termina drops, so level 2 would only break Ctrl+C for no gain.
        assert_eq!(modify_other_keys_request(false).to_string(), "\x1b[>4;1m");
    }

    #[test]
    fn tmux_level_two_needs_the_csi_u_format() {
        // The option value that makes tmux send Ctrl+M as CSI-u instead of CR.
        assert!(tmux_format_is_csi_u(Some("csi-u")));
        // `xterm` (the default format) encodes extended keys as
        // `CSI 27 ; mod ; code ~`, which termina drops.
        assert!(!tmux_format_is_csi_u(Some("xterm")));
        // tmux older than the option, or an answer we could not read: the
        // format is unknown, so the level stays 1.
        assert!(!tmux_format_is_csi_u(None));
    }

    #[test]
    fn tmux_level_two_ctrl_m_parses_as_a_control_chord() {
        use termina::event::{KeyCode, Modifiers};

        // What tmux 3.5+ sends for Ctrl+M once `CSI > 4;2m` is requested; the
        // legacy CR it sends at level 1 carries no CONTROL modifier.
        let mut parser = termina::Parser::default();
        parser.parse(b"\x1b[109;5u", false);
        match parser.pop() {
            Some(TerminalEvent::Key(key)) => {
                assert_eq!(key.code, KeyCode::Char('m'));
                assert!(key.modifiers.contains(Modifiers::CONTROL));
            }
            other => panic!("expected a Ctrl+M key event, got {other:?}"),
        }
    }

    #[test]
    fn cell_size_report_matches_only_the_cell_size_response() {
        let report = TerminalEvent::Csi(Csi::Window(Box::new(
            Window::ReportCellSizePixelsResponse {
                width: Some(9),
                height: Some(18),
            },
        )));
        assert!(cell_size_report(&report));
        let other = TerminalEvent::Csi(Csi::Window(Box::new(Window::ReportWindowState)));
        assert!(!cell_size_report(&other));
    }
}
