use std::io::{self, Write};
use std::time::{Duration, Instant};

use std::collections::HashMap;

use futures_util::StreamExt;
use futures_util::stream::{BoxStream, SelectAll};
use ratatui::prelude::*;
use termina::{
    EventReader, EventStream, PlatformTerminal, Terminal,
    escape::csi::{Csi, Keyboard, Window},
    event::Event as TerminalEvent,
};

use shuvarie_core::{Config, Connections, ThemeSet};

use crate::tui::event::Event;

use self::app::{App, AppEffect, AppMessage};
use self::tabs::TabId;
use crate::spawn::CoreHandle;
use crate::spawn::CoreSpawner;
use shuvarie_core::StartupSession;
use tokio::sync::mpsc::Receiver;

pub mod add_decision_provider;
pub mod add_provider;
mod app;
mod assisted_by;
/// The `/attach` picker: a centred fuzzy finder over a directory, whose pick
/// lands in the composer's pending attachment strip.
mod attach;
mod auth;
/// The `/codemode` overlay: whether the scripting tool is offered, how it
/// presents the roster, and the limits a script runs under.
mod codemode;
mod command_menu;
mod commands;
mod components;
mod confirm_quit;
mod context;
mod escape;
mod event;
/// The worktree reads and writes behind the unstaged-files guard (`git
/// status`, `git add`).
mod git_status;
/// Filesystem watch on the workspace's git `HEAD` file: the sidebar branch
/// label refreshes on checkouts made outside the TUI.
mod git_watch;
/// The slash-command guardrail prompt: a trigger-prefixed submit that matched
/// no command is held for confirmation instead of being sent as prose.
mod guard;
mod history_search;
/// Opt-in raw input tracing (`SHUVARIE_TRACE_INPUT`), for diagnosing file
/// drops.
mod input_trace;
mod list;
mod model_picker;
mod notify;
/// Classify a paste payload as ordinary text or a dropped file path.
mod paste_path;
/// The `/peak-table` popup: the active model's peak/off-peak windows, in the
/// schedule's timezone, UTC, or the user's local time.
mod peak_table;
mod permission;
mod popup;
mod question;
mod registry;
mod scene;
mod screensaver;
mod search;
mod session;
mod session_picker;
mod sidebar;
mod slash;
mod spinner;
/// The tab strip: one agent per tab, each with its own core task and workspace.
mod tabs;
mod theme;
mod theme_picker;
mod title;
mod todo;
pub mod trust;
/// The unstaged-files guard prompt: a new turn submitted from a dirty worktree
/// is held for stage/send-anyway/abort.
mod unstaged;
mod utils;
mod variant;
mod viewer;
mod warning;
mod welcome;
mod workspace;

pub enum TuiResponse {
    SessionSaved { session_id: uuid::Uuid },
}

/// Every core the TUI drives: one per tab, plus the spawner that makes more.
///
/// The cores are held apart from the [`App`] because they are not model state:
/// each one is a running task with a command channel and an event stream, and
/// the loop multiplexes those streams. The App holds a sender per tab (to address
/// one agent) and the ids that name them.
struct Cores {
    spawner: CoreSpawner,
    /// Events from every tab's core, each tagged with the tab it came from.
    /// `SelectAll` retires a stream by itself once its core stops, so a closed
    /// tab leaves nothing behind here.
    events: SelectAll<BoxStream<'static, (TabId, shuvarie_core::Event)>>,
    /// Workspace watch wakeups, tagged the same way: a tab's sidebar refreshes
    /// its own branch label when its `.git/HEAD` moves. Unlike the event
    /// streams, these have no natural end, so closing a tab retires its stream
    /// through that tab's stop signal (see [`Cores::close`]).
    branches: SelectAll<BoxStream<'static, TabId>>,
    /// What stops each tab's core: its command sender and its task. The event
    /// receiver went into `events`, and the branch watch into `branches`.
    live: HashMap<TabId, LiveCore>,
}

struct LiveCore {
    cmd_tx: tokio::sync::mpsc::Sender<shuvarie_core::Command>,
    join: tokio::task::JoinHandle<()>,
    /// Ends this tab's branch-watch stream. A dropped sender resolves the
    /// receiver with an error, which the stream reads as "stop" — so dropping
    /// this releases the filesystem watch behind it.
    stop_watch: Option<tokio::sync::oneshot::Sender<()>>,
}

impl Cores {
    fn new(spawner: CoreSpawner, tab: TabId, core: CoreHandle) -> Self {
        let mut cores = Self {
            spawner,
            events: SelectAll::new(),
            branches: SelectAll::new(),
            live: HashMap::new(),
        };
        cores.add(tab, core);
        cores
    }

    /// Takes over a freshly spawned core for `tab`.
    fn add(&mut self, tab: TabId, core: CoreHandle) {
        let parts = core.into_parts();
        let (stop_watch, stopped) = tokio::sync::oneshot::channel();
        self.events.push(core_events(tab, parts.event_rx));
        self.branches.push(branch_wakes(
            tab,
            git_watch::BranchWatch::start(&parts.root),
            stopped,
        ));
        self.live.insert(
            tab,
            LiveCore {
                cmd_tx: parts.cmd_tx,
                join: parts.join,
                stop_watch: Some(stop_watch),
            },
        );
    }

    /// Stops one tab's core and waits for it, which is where its session lock is
    /// released and its LSP and MCP managers stop.
    ///
    /// The App has already dropped its own sender for the tab by the time this
    /// runs; the sender dropped here is the last one.
    async fn close(&mut self, tab: TabId) -> Result<(), String> {
        let Some(live) = self.live.remove(&tab) else {
            return Ok(());
        };
        let LiveCore {
            cmd_tx,
            join,
            stop_watch,
        } = live;
        // Cut a live turn first. Dropping the sender ends the core's select loop,
        // but a turn already streaming runs in its own task: without the cancel
        // it would keep the model talking (and billing) with nowhere to deliver.
        // Unlike a quit, the process carries on, so this one is worth the send.
        let _ = cmd_tx.try_send(shuvarie_core::Command::CancelStream);
        drop(cmd_tx);
        // End the tab's branch stream, which drops the `.git/HEAD` watch it
        // holds. `BranchWatch::next` parks instead of resolving its channel, so
        // this signal is the only end the stream has; `unfold` drops its state
        // (watch included) the moment the stream yields `None`.
        drop(stop_watch);
        join.await.map_err(|e| format!("core task failed: {e}"))
    }

    /// Stops every core, once the App (and so its senders) is gone.
    async fn shutdown_all(self) {
        let Self { live, .. } = self;
        let mut joins = Vec::with_capacity(live.len());
        for (_, core) in live {
            drop(core.cmd_tx);
            joins.push(core.join);
        }
        // One task failing must not keep the others from being waited on, so
        // every handle is awaited and only then are the failures dropped: by
        // this point the process is exiting and there is no one left to tell.
        futures_util::future::join_all(joins).await;
    }
}

/// One tab's core events as a stream, tagged with the tab.
///
/// The stream ends when the core does (its event sender drops with the task),
/// which is what lets `SelectAll` retire it without the loop tracking it.
fn core_events(
    tab: TabId,
    event_rx: Receiver<shuvarie_core::Event>,
) -> BoxStream<'static, (TabId, shuvarie_core::Event)> {
    futures_util::stream::unfold((tab, event_rx), |(tab, mut event_rx)| async move {
        let event = event_rx.recv().await?;
        Some(((tab, event), (tab, event_rx)))
    })
    .boxed()
}

/// One tab's workspace watch as a stream of "this tab's branch may have
/// changed" wakeups, ending when the tab is closed.
///
/// `BranchWatch::next` never resolves to a closed channel: it parks instead, so
/// the watch cannot signal its own end. The `stop` signal is that end — dropped
/// by [`Cores::close`], which makes this stream yield `None` so `SelectAll`
/// retires it and the watch is dropped with it. Without it every closed tab
/// would leave an inotify subscription on a directory nothing draws, for the
/// rest of the run.
fn branch_wakes(
    tab: TabId,
    watch: git_watch::BranchWatch,
    stop: tokio::sync::oneshot::Receiver<()>,
) -> BoxStream<'static, TabId> {
    futures_util::stream::unfold(
        (tab, watch, stop),
        |(tab, mut watch, mut stop)| async move {
            tokio::select! {
                // The tab closed (or the signal was dropped): end the stream.
                _ = &mut stop => None,
                // Cancelling this future is safe: `BranchWatch::next` only
                // awaits its receiver, so a wakeup is either delivered or still
                // queued.
                _ = watch.next() => Some((tab, (tab, watch, stop))),
            }
        },
    )
    .boxed()
}
pub async fn run_tui(
    config: Config,
    theme_set: ThemeSet,
    spawner: CoreSpawner,
    core: CoreHandle,
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
    // The sidebar shows the workspace of the core this tab drives, not the one
    // the process was launched from: a tab spawned elsewhere has its own.
    let workspace = workspace::WorkspaceInfo::detect_at(core.root());
    let app = App::new(
        ui,
        codemode::CodemodePrefs::from_config(&config.codemode),
        theme,
        theme_choices,
        config.registries.clone(),
        connections,
        core.cmd_tx.clone(),
        initial_cols,
        workspace,
    );
    // One tab so far. Every later tab is spawned by a `SpawnTab` effect and
    // registered by the loop (see `Cores::add`); the watch on this tab's
    // workspace `HEAD` goes in with it.
    let cores = Cores::new(spawner, app.first_tab(), core);

    init_terminal(rat.backend_mut().terminal_mut())?;

    // Decide how modified keys reach the TUI before the render loop starts
    // consuming events: the probe shares the reader, so any keystrokes typed
    // during the wait stay buffered and are delivered by the stream later.
    let modify_other_keys = probe_keyboard_protocol(rat.backend_mut().terminal_mut(), &reader)?;

    let (session_id, cores) = render_tui(app, &mut rat, cores, event_stream, frame_budget).await?;

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut(), modify_other_keys);
    deinit?;
    // Every tab's scroll was persisted on the way out, and the App took its
    // command senders with it, so each core's select loop has already broken;
    // waiting on them releases the session locks and stops LSP and MCP before the
    // terminal is handed back.
    cores.shutdown_all().await;

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
    mut cores: Cores,
    mut event_stream: EventStream,
    frame_budget: Option<Duration>,
) -> io::Result<(Option<uuid::Uuid>, Cores)>
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
    // Drives the sidebar's peak/off-peak countdown: the line follows the wall
    // clock, so it wakes on its own interval while a catalog schedule governs
    // the active model. `None` otherwise. Armed by [`arm_peak_wake`], which
    // keeps a pending deadline instead of re-deriving it on every draw.
    let mut peak_wake: Option<Instant> = None;
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
        // frame boundary keeps every spinner at its own frame rate; the
        // fullscreen scenes animate off the same clock, and the loop takes
        // whichever comes first.
        spinner_wake = match (
            spinner::next_wake(app.active_spinners()),
            app.animation_wake(),
        ) {
            (Some(spinner), Some(scene)) => Some(spinner.min(scene)),
            (spinner, scene) => spinner.or(scene),
        }
        .map(|until| tokio::time::Instant::now() + until);
        peak_wake = arm_peak_wake(Instant::now(), app.peak_wake(), peak_wake);
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
            // Effects the App cannot carry out itself because they need the
            // cores: spawning a tab, or stopping one.
            let mut deferred: Vec<AppEffect> = Vec::new();
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
                        &mut deferred,
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
                        &mut deferred,
                    )
                }
                // Peak/off-peak countdown wake: the sidebar's "Peak ends in …"
                // line is derived from the wall clock, so it is refreshed on
                // its own interval while a schedule is installed.
                _ = async {
                    if let Some(deadline) = peak_wake {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                }, if peak_wake.is_some() => {
                    peak_wake = None;
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        Some(AppMessage::PeakTick),
                        &mut deferred,
                    )
                }
                // Terminal event — draws under the same frame budget as core
                // events; when idle the budget has already elapsed, so keys
                // still render immediately.
                ev = event_stream.next() => {
                    let Some(ev_result) = ev else { break 'render_loop app.session.session_id; };
                    let term_ev = ev_result?;
                    input_trace::record(&term_ev);
                    let msg = app.map_event(Event::Terminal(term_ev));
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        msg,
                        &mut deferred,
                    )
                }
                // Core event — drain all already-queued core events as one batch.
                // From any tab, already-queued ones drained as one batch, and
                // tagged with the tab whose core produced it — so a background
                // agent's stream keeps filling its own chat pane.
                ev = cores.events.next() => {
                    let Some((tab, event)) = ev else {
                        // Every core is gone: nothing is left to drive.
                        break 'render_loop app.session.session_id;
                    };
                    let msg = app.map_event(Event::Core { tab, event });
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        msg,
                        &mut deferred,
                    )
                }
                // Git branch watcher — one tab's `.git/HEAD` was touched (a
                // checkout or branch switch elsewhere), so that tab's sidebar
                // re-reads its own branch label.
                tab = cores.branches.next(), if !cores.branches.is_empty() => {
                    let Some(tab) = tab else { break 'render_loop app.session.session_id };
                    apply_msg(
                        &mut app,
                        rat.backend_mut().terminal_mut(),
                        Some(AppMessage::Tab {
                            tab,
                            msg: Box::new(AppMessage::BranchChanged),
                        }),
                        &mut deferred,
                    )
                }
            };

            if app.quit_requested() {
                break 'render_loop app.session.session_id;
            }

            for effect in deferred.drain(..) {
                apply_loop_effect(
                    &mut app,
                    &mut cores,
                    rat.backend_mut().terminal_mut(),
                    effect,
                )
                .await;
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
    Ok((session_id, cores))
}

/// Runs an effect that needs the cores.
///
/// Neither arm can fail the loop: a tab that will not open is a message to the
/// user, and a core that will not stop is already on its way out (the process is
/// holding no other reference to it).
async fn apply_loop_effect(
    app: &mut App,
    cores: &mut Cores,
    terminal: &mut PlatformTerminal,
    effect: AppEffect,
) {
    // The two arms below report back through `apply_msg`, which can itself defer
    // an effect; nothing either arm reports asks for a core, so the sink is
    // discarded rather than drained.
    let mut sink = Vec::new();
    match effect {
        AppEffect::SpawnTab { path } => {
            // No path means "the same workspace as the tab I am looking at".
            let root = path.unwrap_or_else(|| app.active_root());
            match cores.spawner.spawn(&root, StartupSession::None).await {
                Ok(core) => {
                    let cmd_tx = core.cmd_tx.clone();
                    let root = core.root().to_path_buf();
                    // The tab has to exist before its core's first events are
                    // dispatched, or they would be dropped for naming a tab
                    // nothing knows. Registering the stream afterwards is safe:
                    // the events are already queued in the channel.
                    apply_msg(
                        app,
                        terminal,
                        Some(AppMessage::TabOpened { root, cmd_tx }),
                        &mut sink,
                    );
                    match app.take_opened_tab() {
                        Some(id) => {
                            cores.add(id, core);
                        }
                        // The App refused the tab it just asked for; a core with
                        // no tab behind it has nothing to drive.
                        None => {
                            let _ = core.shutdown().await;
                        }
                    }
                }
                Err(error) => {
                    apply_msg(
                        app,
                        terminal,
                        Some(AppMessage::TabOpenFailed { root, error }),
                        &mut sink,
                    );
                }
            }
        }
        AppEffect::CloseTab { id } => {
            // Dropping the App's sender for the tab first: the core's select loop
            // only sees its channel close once every clone is gone, and the App
            // hands the screen to a neighbour as part of closing.
            apply_msg(app, terminal, Some(AppMessage::TabClosed { id }), &mut sink);
            let _ = cores.close(id).await;
        }
        _ => {}
    }
}

/// Arms the peak/off-peak countdown's wake deadline.
///
/// The deadline is anchored when it is first armed and then kept across draws
/// until it fires. Re-deriving `now + PEAK_REFRESH` after every draw would let
/// any faster wake push it back forever: the busy spinner redraws every ~70 ms
/// while a turn runs (and the open session picker every 2 s), so the countdown
/// would never be due and the "Peak ends in …" line would freeze for the whole
/// turn. A withdrawn schedule — the model switched to one with no peak windows
/// — disarms it.
fn arm_peak_wake(
    now: Instant,
    interval: Option<Duration>,
    armed: Option<Instant>,
) -> Option<Instant> {
    match (interval, armed) {
        (Some(_), Some(armed)) => Some(armed),
        (Some(interval), None) => Some(now + interval),
        (None, _) => None,
    }
}

/// Apply a mapped message, recording a quit request on `AppEffect::Quit`,
/// writing `AppEffect::CopyToClipboard` as OSC 52 (the render loop owns the
/// terminal), and launching the browser for `AppEffect::OpenBrowser`. The
/// queued notifications are drained afterwards.
/// Applies a mapped message and returns whether the app changed.
///
/// Effects the App can carry out are handled here, because the loop owns the
/// terminal they write to. The ones that need the cores — spawning or stopping a
/// tab — are pushed onto `deferred` for the loop to run once the select has
/// resolved (see [`apply_loop_effect`]).
fn apply_msg(
    app: &mut App,
    terminal: &mut PlatformTerminal,
    msg: Option<AppMessage>,
    deferred: &mut Vec<AppEffect>,
) -> bool {
    if let Some(msg) = msg {
        match app.update(msg) {
            Some(AppEffect::Quit) => app.mark_quit(),
            Some(AppEffect::CopyToClipboard(text)) => {
                let _ = write!(terminal, "{}", escape::set_clipboard(&text));
                let _ = terminal.flush();
            }
            Some(AppEffect::OpenBrowser(url)) => auth::open_in_browser(&url),
            Some(effect @ (AppEffect::SpawnTab { .. } | AppEffect::CloseTab { .. })) => {
                deferred.push(effect)
            }
            // Notifications never travel through the return slot; they are
            // drained below.
            Some(AppEffect::Notify { .. }) | None => {}
        }
        for effect in app.take_pending_effects() {
            if let AppEffect::Notify { body, sounds } = effect {
                if let Some(body) = body {
                    let _ = write!(terminal, "{}", escape::notify(&body));
                    let _ = terminal.flush();
                }
                // An empty list is a notice with no sound to play; the first
                // candidate that exists is the one heard.
                notify::play_sound(&sounds);
            }
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

    /// The countdown must not be starved by the faster wakes that redraw the
    /// same loop: while a turn runs the spinner is due every ~70 ms, so a
    /// deadline re-derived on each draw (`now + PEAK_REFRESH`) would never
    /// elapse and the line would stay frozen for the whole turn.
    #[test]
    fn peak_wake_deadline_is_anchored_while_a_schedule_is_installed() {
        let interval = Duration::from_secs(15);
        let start = Instant::now();

        let armed = arm_peak_wake(start, Some(interval), None);
        assert_eq!(armed, Some(start + interval));

        // A spinner frame 70 ms later redraws and re-arms: the pending
        // deadline survives instead of sliding forward.
        let spinner_frame = start + Duration::from_millis(70);
        assert_eq!(
            arm_peak_wake(spinner_frame, Some(interval), armed),
            armed,
            "a faster wake must not push the countdown's deadline back"
        );

        // Once the deadline fires the loop clears it and the next draw arms a
        // fresh interval from the draw.
        let fired = start + interval;
        assert_eq!(
            arm_peak_wake(fired, Some(interval), None),
            Some(fired + interval)
        );

        // A model with no peak windows withdraws the schedule: no wake at all,
        // even if a deadline was already armed.
        assert_eq!(arm_peak_wake(fired, None, armed), None);
    }

    #[test]
    fn notify_escape_carries_the_body_and_drops_control_characters() {
        assert_eq!(
            escape::notify("Shuvarie — Fix the login bug"),
            "\x1b]9;Shuvarie — Fix the login bug\x1b\\"
        );
        // A BEL or ESC would end the sequence early (or smuggle another one
        // in) and a line break would split the single-line toast, so control
        // characters go and line breaks become spaces.
        assert_eq!(
            escape::notify("a\x07b\x1bc\nde\rfg\thi"),
            "\x1b]9;abc de fg hi\x1b\\"
        );
        // ConEmu's `9;4`/`9;9` share OSC 9, so a payload that starts with a
        // digit is made unambiguous rather than parsed as progress or cwd.
        assert_eq!(escape::notify("4;50%"), "\x1b]9; 4;50%\x1b\\");
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

    /// Closing a tab has to end its branch stream, because that is what drops the
    /// `.git/HEAD` watch the stream owns: `BranchWatch::next` parks rather than
    /// resolving a closed channel, so nothing else would ever end it.
    ///
    /// The watch here is on a directory that is not a repository, so it produces
    /// no wakeups at all — which is the shape of the leak: a tab whose watch never
    /// fires still holds an inotify subscription until the run ends.
    #[tokio::test]
    async fn a_closed_tabs_branch_stream_ends_and_releases_its_watch() {
        let dir = tempfile::TempDir::new().unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let mut wakes = branch_wakes(
            TabId::new(7),
            git_watch::BranchWatch::start(dir.path()),
            stopped,
        );

        // While the tab lives the stream stays open and silent: an idle watch
        // wakes for nothing, and yielding `None` here would retire a live tab's.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), wakes.next())
                .await
                .is_err(),
            "an idle watch must not end the stream"
        );

        // Dropping the signal is what `Cores::close` does.
        drop(stop);

        assert!(
            tokio::time::timeout(Duration::from_millis(50), wakes.next())
                .await
                .expect("the stream ends once the tab closes")
                .is_none(),
            "a closed tab's stream yields `None`, so `SelectAll` retires it"
        );
    }

    /// The stream still reports wakeups while the tab is open — the stop signal
    /// must not swallow the wakes it was added beside.
    #[tokio::test]
    async fn an_open_tabs_branch_stream_still_reports_wakeups() {
        use super::workspace::testing::seed_git_repo;

        let dir = tempfile::TempDir::new().unwrap();
        seed_git_repo(dir.path(), "main");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let mut wakes = branch_wakes(
            TabId::new(3),
            git_watch::BranchWatch::start(dir.path()),
            stopped,
        );

        // Let the platform watch register before the write (FSEvents starts its
        // stream asynchronously).
        tokio::time::sleep(Duration::from_millis(250)).await;
        std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/other\n")
            .expect("switch branch");

        // A checkout elsewhere rewrites HEAD; the watcher wakes the stream.
        let woken = tokio::time::timeout(Duration::from_secs(10), wakes.next()).await;
        assert_eq!(
            woken.expect("a rewritten HEAD wakes the stream"),
            Some(TabId::new(3))
        );

        // The stream outlives the wakeup, still owned by its tab. One write can
        // raise more than one notify event, so the next yield may be another
        // wakeup (or nothing at all) — what must never happen is the end of the
        // stream, which is the stop signal's job alone.
        assert!(!stop.is_closed(), "the tab is still open");
        match tokio::time::timeout(Duration::from_millis(50), wakes.next()).await {
            // Nothing further to report: the stream is still waiting.
            Err(_) => {}
            // A second event for the same write, still tagged correctly.
            Ok(Some(tab)) => assert_eq!(tab, TabId::new(3)),
            Ok(None) => panic!("only the stop signal may end a live tab's stream"),
        }
    }
}
