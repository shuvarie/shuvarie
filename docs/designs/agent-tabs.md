# Agent tabs

A tab is **a separate agent**: its own core task, its own workspace, its own conversation,
its own composer. Switching tabs switches which agent you are talking to, not which view you are
looking at. Nothing is suspended — a background tab's turn keeps streaming, its tools keep running,
and its chat fills in while you are working somewhere else.

That is only possible because the core stopped reading the process for itself. Before tabs, every
"where am I" question in the core was answered by `std::env::current_dir()`: the workspace root, the
permission rules' base, the shell's cwd, the context files, the local database. One process could
therefore host exactly one workspace. The first half of this feature is threading that root through
as an argument; the second half is the tab model on top of it.

- `src/spawn.rs`: the spawner. One core task per workspace.
- `src/tui/tabs.rs`: the tab model and the strip.
- `src/tui/app.rs`: staging (the swap), the effects that reach the spawner, and the guards.
- `src/tui.rs`: the render loop's `Cores`, which drives every core and every branch watch.
- `crates/core/src/permissions.rs`: `Permissions::workspace_root`, the root the core runs on.

## The workspace root is task state

`Permissions` is built from a config *and a root* — it always was; the root was simply discarded
after the path rules were compiled against it. It now keeps it:

```rust
pub struct Permissions {
    // …
    workspace_root: PathBuf,
}
```

Everything that used to ask the process follows from that one field:

| Question | Before | Now |
| --- | --- | --- |
| Base for a tool's relative path | `std::env::current_dir()` | `Access::resolve_read` / `resolve_write` (`permissions.rs`) |
| `run_shell`'s default cwd | `current_dir()` | `Access::workspace_root()` (`tools/run_shell.rs`) |
| `lsp analyze`'s containment root | `current_dir()` | the same (`tools/lsp.rs`) |
| Context files | `context::load_from_cwd` | `context::load_workspace(root, ..)` (`context.rs`) |
| Local database | `Store::default_path()` (relative) | `Store::for_workspace(root, global_store)` (`crates/db/src/store.rs`) |
| The core's own `workspace_root` | `current_dir()` | `permissions.workspace_root()` (`core_task.rs`) |

`core_task::run` needed no new parameter: it already receives the rules, so the root arrives with
them. `shuvarie_core::run` is unchanged in signature and behaviour for every existing caller.

The store deserves a note, because it is the one place where "per tab" and "shared" genuinely
conflict. `Store::for_workspace` follows `db { global-store }`: off means that workspace's own
`<root>/.shuvarie/data.db`, on means the shared database in the data dir with directory tracking
pinned to `root`. The distinction matters because opening the *global* store purges sessions no
workspace claims (`purge_orphans`), so two tabs opening it for two different workspaces must each
pin their own tracking — which is exactly what `open_global_in(dir, workspace)` already did for the
process, now called per tab.

`connections.kdl` is deliberately **not** per tab. The active provider and model are the user's, and
the sidebar showing a different model in a different tab than the one a turn actually ran on would
be a lie. Each core loads the same file, so the file stays the single authority — see
[Commands reach every core](#commands-reach-every-core).

## One screen slot, not one per tab

The obvious shape — `Vec<SessionScreen>`, one per tab — would have meant every message handler
choosing a screen before it could do anything: 110 `self.session` references, 48 command-send sites,
and the same again in the tests. The App instead keeps **one** screen slot, and the tab being shown is
*staged into it*:

```rust
pub struct App {
    pub session: SessionScreen,  // the screen on screen
    tabs: Vec<Tab>,              // the strip; the active tab's `screen` is None
    active: TabId,
    // …
}
```

The invariant is that exactly one tab holds `screen: None`, and it is the active one. Three
operations maintain it:

- `stage(position)` — move a tab's state into the working slots: its screen, its scene, its scene
  set, its scene default, and its core's sender into `ctx.cmd_tx`. Returns what was there before.
- `park(position, staged)` — the **switch** half. The tab being left takes its own state back; the
  working slots keep the incoming tab.
- `restore(position, staged)` — the **message** half. The tab takes back what it had while staged;
  the working slots take the outer state back.

Both halves are needed because there are two ways a tab becomes "current": the user switches to it,
or a message about it arrives. `apply_to_tab` is the second:

```rust
fn apply_to_tab(&mut self, tab: TabId, msg: AppMessage) -> Option<AppEffect> {
    if tab == self.active {
        return self.update(msg);            // the common case: no staging at all
    }
    let position = /* find the tab */;
    let staged = self.stage(position)?;     // a closed tab is dropped here
    self.staged_tab = Some(tab);
    let effect = self.update(msg);          // the ordinary path, for *that* tab
    self.staged_tab = None;
    self.restore(position, staged);
    effect
}
```

Staging is **transparent to the handlers**. A background tab's `TokenReceived` appends to *its* chat
pane; its `SessionLoaded` lands in *its* screen; the `SendMessage` its slash command produces goes to
*its* core, because `ctx.cmd_tx` is that tab's. No handler knows tabs exist, which is why the tab work
did not have to touch any of them.

What is *not* staged is equally deliberate: connections, the theme palette, the model picker, the
onboarding flow, the command menu. Those are the user's, not a tab's, and a picker opened in one tab
means the same thing in every tab.

### Guards

A few handlers reach app-wide surfaces that are not per tab — the session picker's list, the tree
popup, history search results — or answer a command whose answer is only meaningful where it was
asked for. `App::foreground()` is false while a tab is staged, and those handlers check it:

- `SessionsLoaded`, `SessionTree`, `SearchResults` — dropped; the picker shows the foreground tab's
  sessions, not whichever tab answered last.
- `SessionLocked`'s "re-fetch the picker" path — same.
- `SessionDeleted` — the *delete* is applied to the picker's list, but the "load the next session"
  fallback is suppressed: a background tab must not drive the foreground tab's session.

A background tab's `StreamDone` still fires its desktop notification. That is the point of a
background tab: you stepped away from it, not away from wanting to know.

## Per-tab events

`Event::Core` carries the tab (`event.rs`), and `AppMessage::Tab { tab, msg }` wraps a message about
one tab. `update` handles that variant first, before anything else looks at the message.

The classification is **by exception** (`core_event_is_global`): the fifteen events that are the
user's setup rather than a tab's agent are global — catalog and registry fetches, saved config,
OAuth, shell warnings, and the answers to interactive commands (`SessionsLoaded`, `SessionTree`,
`SearchResults`). Everything else defaults to belonging to its tab.

That default is the safe direction to be wrong. A global event attributed to a tab costs a no-op in
the wrong screen. A tab's event treated as global is a stream overwriting another tab's chat, which
is exactly the bug tabs exist to prevent.

## The render loop drives them all

The loop no longer owns one core; it owns a `Cores` (`tui.rs`):

```rust
struct Cores {
    spawner: CoreSpawner,
    events: SelectAll<BoxStream<'static, (TabId, shuvarie_core::Event)>>,
    branches: SelectAll<BoxStream<'static, TabId>>,
    live: HashMap<TabId, LiveCore>,   // what stops each one
}
```

Each core's receiver is unfolded into a stream tagged with its tab, and `SelectAll` retires a stream
by itself once its core stops — a closed tab leaves nothing in the select.

The `.git/HEAD` watches are one per tab and tagged the same way, so each sidebar refreshes its own
branch label — but they need help ending. An event stream ends when its core's sender drops; a
`BranchWatch::next` never does (`git_watch.rs` parks rather than resolving a closed channel). So each
tab carries a stop oneshot, and `Cores::close` drops it: the watch stream then yields `None`, which
`unfold` answers by replacing its state, dropping the `BranchWatch` and its inotify subscription with
it. Without that, every closed tab would leave a watcher on a directory nothing draws for the rest of
the run.

The `Terminal → Core → Branch` ordering of the old three-way select is unchanged; there are just more
streams in two of the three arms.

Two effects exist because the App cannot do the work itself, since spawning needs the spawner:

- `AppEffect::SpawnTab { path }` → the loop spawns, replies `TabOpened { root, cmd_tx }` (or
  `TabOpenFailed { root, error }`).
- `AppEffect::CloseTab { id }` → the loop stops the core and replies `TabClosed { id }`.

`apply_msg` pushes those onto a sink; `apply_loop_effect` runs them after the select resolves. The
replies are ordinary messages, so the model mutation stays inside `update`.

The ordering in `SpawnTab` is load-bearing in both directions:

1. **The tab is registered before its core's stream is**, so the core's first events (its skills,
   custom commands, scene set) already have a tab to land in rather than being dropped for naming
   one nothing knows. Registering the stream afterwards is safe: the events are queued in the
   channel, and the stream is only polled on the next select.
2. **The App's sender for a closing tab is dropped before the loop's**, because the core's select
   loop only ends when every sender is gone. `close_tab` hands the screen to a neighbour first, so
   the closing tab's screen — and the sender in it — is released before the loop is asked to stop
   the core.

`Cores::close` also sends `CancelStream` before dropping the sender. Dropping it ends the core's
select loop, but a turn already streaming runs in its own task: on a *quit* that task dies with the
process, so the old path did not need the cancel; on a *close* the process carries on, and without
the cancel the model would keep talking — and billing — with nowhere to deliver.

## Commands reach every core

Two kinds of command, two destinations:

- **Session-scoped** (`SendMessage`, `LoadSession`, `SetTitle`, `ForkSession`, …) go to the active
  tab's core through `ctx.send`, because they are about the conversation on screen.
- **Connection- and pref-scoped** (`SetActiveProvider`, `SetActiveModel`, `SelectVariant`,
  `CycleVariant`, `SetUiTheme`, `AddProvider`, `SetDecisionProvider`, `Reload`) go to *every* core
  through `send_all`. They are the user's, and a background tab that misses them is a background tab
  whose next turn runs on a model its sidebar no longer shows.

`save_scroll` follows the same split in reverse: every tab's scroll position is persisted to the core
that owns that session, not just the visible one, because a switch or a quit is the only chance.

## Opening a tab

`CoreSpawner` captures what is a property of the *process* — the `--config` file, the grants an
unrecorded workspace falls back to, `--transient` — and resolves what is a property of the
*workspace* per spawn: the canonicalized root, the trust grants, the config chain, the scene set, the
permission rules, and the store.

Trust is the interesting one. A tab **never prompts**: opening one from a key press must not yank the
terminal into a dialog. The grants are the workspace's recorded decision when it has one, else the
startup workspace's (`TrustFile::lookup_record`, never `resolve`). A workspace with its own record
gets its own answer; one without inherits the decision you already made.

`/newtab [path]` takes the directory to run in. Without a path it inherits the active tab's
workspace, which makes the command the way to get a second *conversation* in the same project. `~`
expands, and a relative path resolves against the startup directory — tabs never `chdir` the process,
since that would move the ground under every other tab.

Scenes are per tab, because they come from the workspace's own config chain: `/scene` in a tab
offers that project's scenes. The default a *new session* starts under is likewise the tab's.

## Two globals, on purpose

- **The Selune catalog.** `catalog::init` is idempotent per registry id, and the TUI's model pickers
  read the same catalog. Tabs are expected to agree on `registries`; a tab whose config disagrees
  changes lookups in every tab rather than only its own. Per-tab registries would mean a per-tab
  catalog, which is a separate piece of work.
- **`connections.kdl` and the `ui` prefs.** See above.

## The strip

One row, and only once there is something to choose between — a single-tab session paints exactly as
it did before tabs existed. An entry is `<number> <label>`, the label being the session title or, with
no session yet, the workspace directory's name; a busy tab carries `●`.

The active tab is the one entry with a **background**: `accent_bg`, the same fill the rest of the UI
uses for a selected row (`list.rs`), with accent text and padded either side so the highlight
surrounds the label rather than sitting under the letters alone. Inactive tabs stay on the bar's own
`surface` background, in `text_muted` (or `text_dim` when busy). The fill is what makes the current
tab readable from anywhere on the row instead of only by the colour of its letters — and it is why
the entries carry padding at all.

`bar_spans` is a pure function so the layout can be tested without a terminal, and
`the_painted_bar_fills_the_active_tab_only` reads the cells back through a `TestBackend` to prove the
fill reaches the screen rather than only the span. When the strip does not fit, the window is
**anchored on the active tab** and grows outward, right before left, and the dropped runs collapse to
`…`. A bar that dropped the tab you are looking at would be worse than no bar, and a bar anchored at
the start would do exactly that as soon as you moved past its width.

Keys: `Alt+Left` / `Alt+Right` cycle with wrap, `Alt+1` … `Alt+9` jump. All of them are gated on
`tabs.len() > 1`, so with a single tab every chord the composer had before tabs is still the
composer's — including `Alt+Left` / `Alt+Right`, which no longer have a tab to move to.

## Known limitations

- **No persistence across restarts.** Tabs are process state; `-c` / `-s` resume one session into the
  first tab. Restoring a set of tabs would need somewhere to record them.
- **Each tab is a full agent.** Its own LSP manager and MCP connections, started lazily — an idle tab
  is cheap, but a tab that has used `lsp` or an MCP server holds those resources until it is closed.
- **`--transient` and the global store are per-spawn.** Every tab opens its own store; with the global
  store that is one `open_global_in` per tab, each purging orphans for its own workspace. That is
  correct but not free.

## Testing

The claims above that matter are pinned by tests, most of them in `app.rs` and `spawn.rs`:

- `each_core_resolves_the_workspace_it_was_spawned_for` (`spawn.rs`) — two real cores in one process,
  two workspaces, each reporting its own custom-command roster. This is the load-bearing claim of the
  whole feature.
- `a_workspace_that_is_not_a_directory_is_rejected`, `an_unrecorded_workspace_inherits_the_startup_grants`.
- `each_tab_keeps_its_own_draft_and_session` — a half-written prompt and a session id survive two
  round trips.
- `only_the_active_tab_receives_session_commands`, `a_connection_command_reaches_every_tabs_core`.
- `a_background_tabs_event_lands_in_its_own_screen` — and the working slots come back intact,
  proven by a `Ping` still reaching the first tab's core afterwards.
- `the_active_tabs_event_applies_in_place`, `an_event_for_a_closed_tab_is_dropped`.
- `interactive_answers_from_a_background_tab_are_ignored`.
- `the_active_tabs_scene_set_fills_the_switcher` — surviving a round trip.
- `closing_a_tab_asks_the_loop_to_stop_the_tab_on_screen`, and that the strip is untouched until the
  loop answers.
- `the_tab_keys_are_free_when_a_single_tab_is_open`.
- The `tabs.rs` bar tests: the active tab surviving a bar narrower than the strip, the fill landing on
  the active entry alone (in the spans and, through a `TestBackend`, in the painted cells), and the
  label falling back to the workspace directory.
