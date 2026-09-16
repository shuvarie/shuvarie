# AGENTS.md

Shuvarie (シュヴァリエ, "chevalier") is a terminal-based AI agent coding tool built in Rust. This file orients AI agents (and humans) to the codebase, conventions, and intended architecture so that changes stay consistent.

## Workspace crates

- `./` (binary `shuvarie`) — TUI rendering, event loop, input handling, channel wiring. **No business logic lives here.** Renders state produced by `shuvarie-core`.
- `./crates/core/` (`shuvarie-core`) — app state, the async core task that orchestrates LLM and database work, domain `Session`/`Event`/`Command` logic, and the agent tools (read/write/edit/delete files, `apply_patch`, run commands, list dirs, grep, glob, `skill`, `lsp`, `webfetch`, `question`, `todo`) with permission rules. Persists through `shuvarie-db`; owns the `shuvarie_lsp::LspManager` and the worker-agent roster. Re-exports `shuvarie-config` types at its crate root.
- `./crates/config/` (`shuvarie-config`) — the `config.kdl`/`connections.kdl`/`trusts.kdl` model + hand-rolled KDL codecs, path resolution (`config_dir`, the layered load chain), and the profile-split app file/dir name constants. Depends only on `kdl`/`dirs`/`thiserror`; secrets live only in `connections.kdl`; no LLM/TUI/db concerns.
- `./crates/llm/` (`shuvarie-llm`) — thin wrapper over `rig` (Rig 0.42.x — see the `rig` skill): provider clients, model listing, streaming multi-turn agent chat (`stream`, `run_worker`), `StreamItem`/message types, `WorkerAgent`, and the context-management `ContextHook`. The `FileChangeHook` surfaces each tool result the moment its call completes (`with_early_finish`; rig would otherwise buffer the whole batch behind the slowest call) and `provider.rs::map_agent_stream` drops the later buffered duplicates via `FileChangeHook::surfaced_early`. No pricing, no TUI concerns.
- `./crates/db/` (`shuvarie-db`) — persistence layer: Toasty models, the `Store` wrapper over Turso embedded SQLite, embedded schema migrations, and the `migrate` bin. No TUI concerns.
- `./crates/highlight/` (`shuvarie-highlight`) — markdown + code highlighting for the chat pane; emits unwrapped ratatui `Line`s (wrapping stays with the TUI `Paragraph`), reports safe top-level block boundaries (`render_pass`) so streaming re-renders only the open tail, renders tables with box-drawing borders (capped cell widths), and preserves code indentation (leading whitespace → NBSPs, tabs → 4 columns).
- `./crates/lsp/` (`shuvarie-lsp`) — LSP client lifecycle: built-in per-language server registry + config, server spawn via `async-lsp`, and the `LspManager` (start/stop/restart/analyze, diagnostics collection). Stays KDL-free.
- `./vendor/selune/` (`selune`, git submodule) — uniform source of provider + model info (Catwalk format): wire types, a hosted catalog client, offline embedded configs. A connection's transport is a `selune::ProviderType` (kebab-case); `shuvarie-core::catalog` derives pricing/context/key requirements from the matching catalog entry (id stored in the connection's `catalog` field); `find_model` matches catalog ids loosely — exact, vendor-tail (`vendor/model`), dated snapshot alias in either direction, or a unique tag-stripped variant (ambiguous bases like `gpt-oss` match nothing).
- `./vendor/frameplay/` (`frameplay-lib`, git submodule) — wall-clock frame repeater powering the spinner animations; the render loop wakes at each frame boundary via `Frameplay::time_to_next_frame`.

**Feature placement**: domain logic → `shuvarie-core`; provider/model data + pricing → `selune` (via `shuvarie-core::catalog`); LLM/SDK glue → `shuvarie-llm`; config-file formats/paths → `shuvarie-config`; storage/ORM glue → `shuvarie-db`; markdown/code highlighting → `shuvarie-highlight`; LSP client lifecycle → `shuvarie-lsp`; only rendering + input dispatch → the root binary.

## Architecture

### The Elm Architecture (TEA)

The TUI follows `map_event → message → update → return`. The root `App` composes submodels (session screen, chat pane, overlays), each with its own message enum, `map_event`, `update`, and `view`; the parent dispatches events and forwards grouped messages.

- `map_event` — pure (`&self` only), no side effects; terminal events dispatch overlay-aware first, then route-aware.
- `update` — mutates the model and may return an effect; **all side effects happen here** (send core commands via `ctx.send(...)`; `UpdateCtx` carries `Connections` + the `Command` sender).
- `view` — draws state, `&self`, no side effects; may compose multiple widgets (a model need not implement `ratatui::Widget`).

New components follow the same shape (message enum + `new` + `view` + `update` when needed), composed in `App` with a grouped `AppMessage` variant. Core events (`shuvarie_core::Event`) are mapped to `AppMessage` variants in `App::map_event`.

### Async runtime

`main` is `#[tokio::main]`. The TUI loop runs on the main thread using termina's `EventStream`, `tokio::select!`-ing between terminal events and core events (one draw per frame budget; `[ui].frame_rate` default 60). All LLM/database work runs on the spawned **core task** (`shuvarie_core::run`); communication is mpsc — TUI → core `Command`s, core → TUI `Event`s (wrapped as `Event::Core`). Keep blocking work off the TUI thread (offload to the core task or `spawn_blocking`).

### Startup

After the `-d`/`--dir` flag (default: the launch cwd) has switched the process working directory: workspace-trust scan (see Trust & permissions) → config + connections load → the working-directory store opens (`.shuvarie/data.db` for release, `.shuvarie-dev/data.db` for debug) → the TUI opens on the Session route. The first message lazily creates the session row; with no providers configured a Welcome overlay shows.

### LLM providers

All provider access goes through `rig` in `shuvarie-llm` — OpenAI-compatible, Anthropic, Gemini, Ollama, Ollama Cloud. The root binary never calls `rig` directly.

## Storage

### Build-profile name split

Every app-owned file/dir name differs by profile: release keeps `shuvarie.kdl`, `.shuvarie`, `~/.config/shuvarie`; debug builds use the `shuvarie-dev` variants, so dev and release state never mix. Canonical constants live in `shuvarie-config` (`shuvarie_config::WORKSPACE_DIR_NAME` — re-exported by `shuvarie-db` — plus the other profile-split consts in its `src/lib.rs`); never hardcode the names.

### Config (`config.kdl`)

- Layered chain `$cwd/shuvarie.kdl` → `$cwd/.shuvarie/config.kdl` → global `~/.config/shuvarie/config.kdl`, merged per top-level section (except `lsp.servers`/`registries`, key-by-key). Hand-rolled KDL in `config_kdl.rs` (same shape as `connections_kdl`, shared span/error helpers in `kdl_util`).
- Default-on flags are stored inverted (`disabled #true` = off, omitted = on) and values equal to their section default are omitted from the saved file.
- Holds UI/agent/context/lsp/skills/retry/shell/registries/tools/permissions preferences — **no secrets**.
- `permissions { … }` gates tool calls (see Trust & permissions; absent everywhere = the built-in defaults).
- `shell.path` selects the executable `run_shell` uses (absolute/relative path or a `PATH` name); when it is not found at startup the platform default is used and a `ShellWarning` popup is shown.
- `registries { selune { … } }` gates the built-in Selune registry (unknown names are preserved for future user registries): `disabled #true` hides it from the provider/model selectors and never fetches the hosted catalog; `remote-first #true` seeds the selector popups on the hosted registry and enables the bounded startup fetch (the offline-first default skips it — popups fetch on demand via Ctrl+O). Metadata lookups (pricing/context/key rules) always consult the embedded catalog.
- `tools { web-search { … } }` (at most one node) registers the `web_search` agent tool, offered only when the section exists and `enabled #true`: `type` is `ollama` (POST JSON, structured `results[]` output) or `to_markdown` (fetch the endpoint, convert the body to markdown); `headers` values resolve `$ENV`/`${ENV}` at tool build time; `params type="body-json"|"query"` maps the `query` argument to the remote parameter name via `as="…"` (omitted `params` defaults: ollama → body-json `query`, to_markdown → query `q`).
- `scenes` and `themes` define named scenes/themes (see Scenes and UI design & theming for shape and loading).
- The `LspConfigRepr` KDL mirror converts to `shuvarie_lsp::LspConfig` via `shuvarie_core::lsp_manager` (keeps `shuvarie-lsp` config-format-free).

### Connections (`connections.kdl`)

- Providers keyed by `id` plus the active provider/model/variant; hand-rolled KDL in `connections_kdl`; files the app writes start with an API-key warning header comment (`connections_kdl::FILE_HEADER`).
- `kind` is the rig transport (a `selune::ProviderType` in kebab-case); the optional `catalog` child carries the Selune catalog id for pricing/context/key-requirement lookups (legacy configs put the id in `kind` — still resolved).
- The active `variant` is the model's reasoning-effort pick (the catalog entry's `reasoning_options`): Ctrl+T cycles it (`Command::CycleVariant` → `catalog::next_variant`, wrapping past the last variant back to the unset default; `SetActiveModel` resets it), `/variant [name]` opens a selector or picks directly (`Command::SelectVariant`, validated TUI-side against `catalog::model_variants`). Rendered as `provider:model:variant` on the status row; persisted but not yet plumbed into requests.
- Secrets live here — never in the database or `config.kdl`. Connectability checks (`catalog::is_connectable` / `has_connected_providers`) consult the live catalog, so they live in `shuvarie-core::catalog`, not the config crate.

### Database (`.shuvarie/data.db`)

Turso embedded SQLite via the Toasty ORM. Stores open with turso's multiprocess WAL (`experimental_multiprocess_wal`), so several app instances share one store.

- `sessions` — UUID v7; `leaf_id` points at the active branch's tip: every append persists the new tip, and forks set it. `0` (`shuvarie_db::EMPTY_LEAF`) marks a deliberately cleared (empty) path — an undo/fork before the first prompt — which reloads as an empty chat; `NULL` is only legacy unset rows and fresh sessions, which load as the newest message; also the persisted scene and the chat pane's scroll position (sticky-bottom flag + content anchor, written by `Store::set_scroll` on session leave — a raw SQL update so `updated_at` is untouched; `set_active_leaf` is likewise raw-SQL).
- `messages` — the session **tree**: `parent_id` links each node to its predecessor (`None` at root prompts; a fork is just a node with several children); the active path runs leaf → root. Positioned reasoning + text segments — text runs carry the tool-call position they streamed at, so a reload rebuilds the interleave. Interrupted flag; per-message usage plus the turn's last main-request usage (`request_json`; an assistant message joins the turn's multi-request text runs with a paragraph break — `TurnText` in `shuvarie-llm` — so reloads keep the streaming paragraph breaks).
- `tool_calls` — file-change JSON + original/new content, kept for diff rendering only — nothing ever reverts files; a `killed` flag marks calls cut off mid-run (they persist so reloads keep their blocks).
- `message_embeddings` — semantic search; regenerated after imports.
- `session_locks` (raw table, no toasty model) — the core task locks the active session under a UUID client id with a 10 s heartbeat and a 30 s stale-beat takeover (released on session transitions and quit); `LoadSession`/`DeleteSession` of a foreign-locked session emit `Event::SessionLocked` and the picker renders those rows dimmed.

Schema is managed with toasty migrations embedded in the binary — regenerate with `cargo run -p shuvarie-db --bin migrate -- migration generate --name <change>` (see the `toasty` skill); new migrations are timestamp-prefixed (`YYYYMMDD_HHMMSS_<name>.sql`) while the init migration keeps its all-zero `0000` prefix.

## Trust & permissions

### Workspace trust

Before the config chain loads, the startup flow scans the workspace root for trust candidates: the root context file (`shuvarie_config::CONTEXT_FILE_CANDIDATES`), `.shuvarie/context/*`, a non-empty `.agents/skills`, and workspace config files + non-empty `scene.d`/`themes.d` dirs at the workspace root or inside `<WORKSPACE_DIR_NAME>` (skipped when `--config` named the file).

- Undecided workspaces prompt with a one-screen checklist (`src/tui/trust.rs`, own terminal + alt-screen before the TUI starts): Space toggles, Enter records+applies, Esc rejects for this session only, q quits.
- A recorded workspace applies its grants silently, **except** when the scan finds candidates in categories neither granted nor asked before — those prompt once more ("new files" checklist; `Grant` merges via `TrustGrants::union`; Esc keeps the record and re-asks next startup). Records carry the offered categories in an `asked { … }` block (`WorkspaceTrust.asked`, `TrustFile::mark_asked`); a fully rejected record (empty grants) never re-asks.
- Decisions live in `config_dir()/trusts.kdl` (`shuvarie_config::TrustFile`, codec in `trusts_kdl.rs`): a top-level `trust { … }` is the default for unrecorded workspaces; each `path "<dir>" { trust { all | contexts skills configs } }` record answers by canonicalized path match (empty body = rejected); `/-` comment-outs are the manual revocation mechanism.
- Only granted categories load: `Config::load_trusted` skips workspace config layers without `configs`, `Skills::load` skips `.agents/skills` without `skills`, and `context.rs` skips the workspace-root file and `.shuvarie/context/*` without `contexts` — ancestor dirs and the global config dir stay user-owned and always load. The grants travel into the core task (`CoreCtx.trust`) so `/reload` and per-turn context re-reads keep honoring them.

### Permission rules

Tool calls are gated by the configurable `permissions` config section (`crates/core/src/permissions.rs`). Built-in default when absent: `ask-all`; `paths { allow except-hidden=#true "." }`; `shell-patterns { allow-all }` — layers never drop these: each scope's rules concatenate most-specific-layer-first and the built-in rules always sit at the deepest end. `permissions` merges key-wise: verbs come from the highest-priority layer that sets them, rule lists stack highest-priority-first.

- **Matching** — path rules match canonicalized paths in declaration order, first match wins; the fallback is the scope's bare `-all` verb, then the top-level verb, then ask. Rule nodes take several path/pattern arguments sharing the verb+properties (`ask "rm" "sudo"`); the serializer re-groups consecutive same-key rules into one node.
- **Properties** — `exact` matches the path alone; `except-hidden` skips the rule when the path below it has a hidden component (workspace-root `.agents` and the app dir — `.shuvarie`, `.shuvarie-dev` in debug — stay exempt; global skill dirs are always readable); `allow` rules take `mode="ro"|"rw"` (default `rw` — `ro` matches only reads, never decides writes).
- **Shell rules** — path rules never gate `run_shell`; only `shell-patterns` decide commands. `raw` patterns match flanked by non-alphanumeric characters with whitespace runs collapsed; `regex` matches as written (compiled at startup — a bad regex fails startup). Every `deny` shell rule is also watched against a running command's captured output and kills it on match.
- **Denials** — any denial cuts the whole turn like a user cancel (`DenyCut`, consumed by the stream task): the denied call's own result is persisted as failed with the denial reason, remaining running calls as killed.
- **Asks** — `Ask` pauses the tool until the user answers the TUI overlay (`Event::PermissionRequested` / `Command::PermissionDecide` with a `PermissionAnswer`): allow, deny (Esc = deny = turn cut), or allow-for-this-session (`s` — remembered in the `SessionGrants` set inside `PermissionGate`, keyed by the exact canonical path or the whitespace-collapsed command; rules still decide first, so a grant never overrides a `deny`). `webfetch`, `skill`, and bash-mode `!` commands are ungated.

## Session lifecycle

### Tree, active path, forking

- `Session::from_stored` builds the full tree (`Session.nodes`, used by the popup) and walks `leaf_id` → root into the **active path** (`Session.messages` + the path-keyed maps the TUI renders from); usage totals are path-only. A cleared `EMPTY_LEAF` walks an empty path, and the in-memory leaf is the walked tip.
- `history_for_send` walks tip → root and stops after the newest summary node, so a summary replaces everything before it.
- `Command::ForkSession { node, summarize }` re-roots the path: every turn node forks *before* itself (its parent becomes the tip; its content is recalled into the input) — summary/system nodes are markers that walk to themselves, and `node: None` walks to the last user prompt (`/undo`). While a turn runs, the fork first cuts it (`cut_running_stream` — the same persist-interrupted path as `CancelStream`) and wipes the steered queue.
- With `summarize`, the fork tip's ancestor chain is summarized by an LLM (`compaction::summarize` + `serialize_head`) into a new summary node (`CompactionStarted`/`Finished` keep the TUI busy indicator armed); the forked-away node is reparented under it.
- `Command::DeleteBranch` removes an off-path subtree (tool calls + embeddings + messages; the core refuses when it would contain the active leaf; busy-guarded while a turn runs).
- `Event::Forked { session, prompt }` reloads the TUI view — the chat rebuild keeps the live viewport (sticky stays pinned; the content anchor carries over and clamps to the bottom when the anchored turn was forked away; prompt recall).
- The `/tree` popup (`src/tui/session/tree.rs`, `Command::OpenTree` → `Event::SessionTree`, never busy-guarded — read-only) walks the tree (`↑↓`), forks (Enter), toggles summarize-before-node (`s`), and deletes branches (`d`, two-press confirm); while a turn runs it is view-only (walk + Esc).

### Undo / replay

- `/undo` = fork before the last user prompt, cutting a running turn first; no file changes are ever reverted (`run_shell` effects untouched).
- `/replay` = that fork + re-send (refused while a turn runs).
- Interrupted turns are re-streamed automatically by the retry/overflow-continue paths, which delete the interrupted assistant tip, walk the leaf back, and emit `Event::Forked { prompt: None }` before re-streaming.

### Import / export

- `shuvarie_db::session_file::SessionFile` is the versioned JSON snapshot of one session (format field 1): the session row (timestamps in epoch ms), the full message tree, the tool calls, and the scroll position (the anchor is a dense path index, so it survives the import unchanged); embeddings are excluded — they regenerate after an import.
- Export: `shuvarie --export-session [FILE]` (with `-s` exports that session, else the most recent; no value names the file `<session_id>-<YYYYMMDD_HHMMSS>.json` in the cwd) and the `/export [path]` slash command (`Command::ExportSession` → `Event::SessionExported`).
- Import: `shuvarie --import-session FILE` → `Store::import_session` — the session id is kept when free (else a fresh UUID v7 is minted), message/tool-call ids are regenerated and remapped (parents first by `seq`, which is topological), leaf/scroll/timestamps carry over, and the row is always a `Main` session. Both CLI commands run right after the store opens (before trust/config) and exit; a mid-import failure deletes the partial rows.

### Interrupted turns

- A cancel (Escape/quit) aborts the stream task; `persist_interrupted_turn` persists the partial text/reasoning as interrupted plus the tool calls that had started but not finished as killed records (`TurnState.pending_tools`), and `Event::StreamCancelled` follows.
- The TUI commits the in-flight turn with still-running tool blocks finalized as killed (`ToolMessage::Kill` in `session/blocks/tool.rs`: ⏹ WARNING marker, `Took` duration from the wall clock, streamed output kept) — never dropped, never failed.
- A `run_shell` call killed by its timeout is also a killed block (decided by the `timeout Ns:` status line in the output, live and on reload) rather than failed; text whose first line is not a status line is all body.

## Agent runtime

### Prompt steering

- A prompt submitted while the agent loop is busy (streaming or connection-retry wait) is queued in the core task's run loop (`steered` in `core_task.rs`) instead of erroring; the queue's front is dispatched as the next user turn at the next completed action boundary of the active stream — as soon as the in-flight tool batch settles (immediately, not on the next stream item) or when a thinking/text segment completes; mid-segment deltas never cut.
- The stream task cuts the turn (persisting it interrupted) and reports `StreamOutcome::Preempted`; a shared `SteerSignal` gates the cut so `CancelStream` never double-persists. The signal re-arms after each dispatch while the queue is non-empty (multi-prompt queues drain at successive action boundaries) and fully disarms once the queue empties.
- Session-level transitions (new/load/delete) and forks wipe the queue; `Alt+Up` / `Alt+Shift+Up` recall the newest entry into the input (overwrite vs. stacked prepend). The TUI mirrors the queue via `Event::PromptSteered` / `TurnStarted { steered }` / `SteeredRecalled` / `SteeredCleared` and renders queued prompts in the chat pane.

### Scenes

A named bundle applied at request build: system prompt, injected wrapper prompts, and the tool roster.

- **Config shape** — `scenes { default "Plan"; scene name="Plan" { description, subagents, system-prompts, thinking, tools } }` with `system-prompts { prelude, interlude, before-each, after-each }`, `tools { enable-all|ask-all|disable-all; tool "a" "b" { disabled #true; ask #true } }` (one entry per name; `disabled #false` re-enables under `disable-all`), and `subagents { disabled #true; <worker> { system-prompts, tools, disabled } }` keyed by built-in worker name.
- **Loading** — two levels (`Config::load_scenes` → `SceneSet`):
  - Global level — global `config.kdl` scenes + global `<config_dir>/scene.d/*.kdl`, always loaded.
  - Local level — workspace configs + workspace-root `./scene.d/*.kdl` + `<WORKSPACE_DIR_NAME>/scene.d/*.kdl`, `configs`-trust-gated; `--config` uses the named file as the single local layer with its adjacent `scene.d`.
  - Drop-in files that fail to read or parse are skipped with a warning instead of failing the load.
  - Within a level a scene name must be unique — conflicts report via `SceneSet::warnings` → `Event::ScenesLoaded` → the TUI warning popup, and neither copy loads; across levels the local level overrides the global one field-wise per scene name, and `default` comes from the highest-priority source that sets it.
  - `Config::scene_sources` carries the per-layer scenes for conflict detection; `Config::scenes` keeps the plain chain merge for save fidelity.
- **Resolution** — `shuvarie_core::scenes::Scene::resolve` reads the persisted `sessions.scene` (raw-SQL `Store::set_scene` keeps `updated_at` untouched; new sessions store `scenes.default`), falling back to the built-in Default when unset or unresolvable (the built-in Default scene is code in `shuvarie_core::scenes`, never serialized).
- **Per request** — the scene `prelude` replaces `AGENT_PREAMBLE` (skills/web-search/context sections still append); `interlude` injects as a system message at the top of the outgoing history once a switch happened; `before-each`/`after-each` hooks wrap every prior user prompt / assistant reply plus the pending prompt (`inject_history` — the history never replays tool interactions, so turns are exactly [user, assistant]).
- **Tool gating** — a stricter overlay on the permissions engine: `disabled` tools are dropped from the roster by the `tools::scene_tool` builders, `ask-all`/per-tool `ask` turns an `allow` verdict into an ask (`Access::for_tool`; deny stays deny), and ungated tools (`lsp`, `webfetch`, `skill`, `question`, `todo`, `web_search`) get an ask-first call wrapper. Worker overrides: per-worker `prelude` replaces that worker's preamble (an `interlude` appends), per-worker `tools` gate the worker's set, a disabled worker leaves the roster, `subagents { disabled #true }` empties it.
- **`/scene [name]`** — dispatches `Command::SwitchScene`:
  - Opens the switcher popup; a bare-name argument switches directly. Picker rows carry an identity (`SceneListEntry::id`, `None` = built-in), so a configured scene named "Default" stays distinct and switchable, while an unconfigured `/scene Default` falls back to the built-in.
  - Refused while a turn is busy, and mid-session (after the first message) for scenes without an interlude — the built-in included; before the first message any scene may be picked (a session-less pre-pick records the scene the session will start under).
  - Persistence reports `Event::SceneChanged`; switch failures surface via `Event::SceneError`; `thinking #false` is reserved (schema only — no per-provider thinking plumbing yet).

### Bash mode & prompt drafts

- **Bash mode** — a prompt starting with `!` runs the rest as a local shell command through the resolved `Shell` (same machinery as `run_shell`, no timeout, no persistence, never sent to the model). The input text renders in the accent color while the buffer starts with `!`; the run streams into a display-only floating popup above the input (`session/bash.rs::BashPopup`, Escape dismisses) fed by `Command::RunBash` → `Event::BashStarted`/`BashOutput`/`BashFinished` — the popup shows the full raw captured output verbatim (`ShellOutputTx::full`: no trimming, no `exit N:` prefix, display-capped only by a rolling 1 MiB window) — display-only, wiped on session transitions.
- **Prompt draft stacks** — the input area keeps `up_stack`/`down_stack` (TUI-local, `TextArea` in `src/tui/components.rs`): `Up`/`Ctrl+P` with the cursor on the first visual row and a non-empty up stack swaps in the previous sent prompt (pushing the current text onto the down stack); `Down`/`Ctrl+N` walks back when the cursor is on the last row (chat scrolling keeps the keys when a stack is empty). A plain submit records the typed text and clears the down stack (bash `!` and `/command` submits don't); Ctrl+C's clear, a displaced steer-recall draft, and a forked-away prompt (`Forked.prompt`) all stash into the up stack first.

### Context management

Three layers keep long turns from blowing up input tokens (config under `[context]`):

1. Tool-output caps (`truncate.rs`).
2. A history-budget `ContextHook` (rig `AgentHook` in `shuvarie-llm`) that trims old tool results to recovery-hint stubs.
3. Overflow-triggered LLM compaction (`compaction.rs`) that auto-continues after summarizing older messages — the cut keeps the most recent `keep_recent_tokens` tokens verbatim (floored at 4 messages); repeated compactions resume from the previous summary (folded in as prior context instead of re-summarizing it); the serialized head carries tool activity plus ground-truth read/modified file lists.

### Project context files

- The preamble loads the global `~/.config/shuvarie/AGENTS.md`, then the `AGENTS.md`/`CLAUDE.md` (an `AGENTS.override.md` replaces both) of every ancestor directory up to the filesystem root — one file per directory, outermost first — plus `.shuvarie/context/*` (`context.rs`; the workspace-root file and context dir only when the `contexts` trust category is granted).
- Re-read per request so edits apply without a restart; announced once per session via `Event::ContextLoaded`.

### Live shell output

`run_shell` streams output-tail chunks through the turn's shared shell channel, merged into the turn stream itself (`merge_shell_chunks` → `StreamItem::ShellOutput`; `select_all` polls the LLM/worker sub-streams first, so a call's `ToolStart` always precedes its chunks). The loop resolves each chunk to its own call by (worker, command) against the pending calls' args (`resolve_shell_call`) and `Event::ToolOutput` carries that `call_id`, so concurrent shells of one agent stream into their own blocks; identical concurrent commands stay ambiguous and fall back to the name+worker match.

## Rendering & UI

### Rendering stack

`ratatui` 0.30.x with the `termina` backend (not Crossterm) — see the `ratatui` skill. Alt-screen and the Kitty-keyboard/mouse-tracking protocols are managed manually via CSI escapes in `src/tui/escape.rs`.

### UI design — borderless, muted

The visual style lives in `src/tui/theme.rs` (true-color `Color::Rgb`, medieval/heraldic palette) backed by the resolved `ThemeColors` (default **Faerun** with a dark base and a light parchment variant).

- Palette access is through lowercase accessor functions (`theme::accent()`, `theme::diff_add_bg()`, …) over a process-global — never raw `Color::Rgb` literals. Import colors and use the helpers (`section_block`, `overlay_block`, `help_line`, `active_marker`) — never inline `Block::bordered()`, raw `Color` constants (`Color::Red`…), or `Stylize` shorthand colors. The `shuvarie-highlight` crate keeps its own (non-themed) syntax palette.
- Format tokens/cost via the `utils/num.rs` helpers; render help rows via `theme::help_line` — never flat unstyled strings.
- **Selection** is baked into the `ListItem` (`▶` prefix + `ACCENT_BG` row style via `list::render_list_item`); lists render statelessly and scroll offsets are computed in `update` via `list::scroll_offset_for` — do not introduce `ListState`/`TableState`/`ScrollbarState`.

**Themes config** — `themes { theme name="Ayu" { bg "#0b0e14"; accent "#ffb454"; … } }`:

- Named themes are per-role palette overrides over `THEME_ROLES` (unknown roles error); a `theme` node may add `variant="light"` (several definitions per name, keyed by name + variant) and `mode "dark"|"light"` (default dark — which terminal appearance the definition paints for).
- Two-level load like scenes (`Config::load_themes` → `ThemeSet`: global `<config_dir>/themes.d` always + `configs`-trust-gated workspace `./themes.d` + `<WORKSPACE_DIR_NAME>/themes.d`), same conflict/skip-warning rules — a same-level conflict is per `(name, variant)` and labeled `name:variant`; `ThemeSet::warnings` opens the warning popup at startup.
- `ui { theme "name" }` or `"name:variant"` selects one at startup (`shuvarie_core::ThemeSet::resolve`): with no explicit variant, definitions are picked by the detected terminal mode (mode-matching definitions win, falling back to the base definition); user definitions shadow built-ins per `(name, variant)`; unknown names/variants warn and fall back to Faerun by mode. `run_tui` detects the terminal background at startup (`theme::detect_variant`: OSC 11 query via termina's `EventReader`, 250 ms timeout, relative-luminance split, dark on no answer) and `App::new` installs the resolved palette via `theme::init` — never re-resolved mid-run.

### Chat pane

The chat history pane (`session/chat.rs` + `session/virtualizer.rs`) is the one stateful exception: turn-based virtualization with `RefCell` render caches, painting only the visible viewport plus overscan.

- **Segments** (`session/segment.rs`) are chunked into `BodyChunk`s:
  - `Fixed` — always-resident lines.
  - `Sliced` — a `BodySource` projection (output rows, numbered file content, diff rows, or `Lines` — shared pre-rendered markdown) whose wrapped-row counts are precomputed and whose lines materialize only when painted, and only for the rows intersecting the viewport.
  - Long tool bodies / diffs / expanded reasoning / markdown replies render as sliced chunks (diff rows always, so the painter can resolve per-row styling from the source).
  - Diff rows tint their full row by kind (`DIFF_ADD_BG`/`DIFF_DEL_BG`, stronger `*_EMPH_BG` on the partially-edited runs — byte ranges persisted as `DiffLine.edits` by `compute_diff` via similar's inline diff).
  - Wrap counting and slicing rely on ratatui wrapping each source line independently — per-line `Paragraph::line_count` equals the line's contribution in a whole-body paragraph, so windowed paint is bit-identical (tests assert this).
- **Markdown cache** — per block (`session/md_cache.rs::MdCache`): `render_pass` records safe top-level boundaries, streaming appends re-render only the open region after the last one (committed lines live in immutable `Rc` runs), and per-line wrap counts are recomputed only on width change. Spinner ticks never invalidate caches: `TurnData::refresh_spinners` repaints only the spinner-bearing segments (`TurnCache::spinners` slots), tool bodies cache chunks keyed by `(width, env_rev, body_rev)`, and block estimates are cached per content revision.
- **Virtual selection** — mouse drags select text:
  - In the input (`InputBuffer` byte-anchored `sel_anchor`, head = cursor) and over the virtualized chat (`SelPos { turn, row, col }` in content space, anchored in `Chat::selection` so it survives scrolling; remapped on turn commit/steered pops).
  - Left-mouse routes by zone in `SessionScreen::handle_mouse` (input area → text area, bash popup → swallowed, else chat); highlight is a post-paint overlay tint (`theme::selection()`, full width on interior rows, column-bounded on boundary rows, snapped around wide glyphs).
  - Copy assembly (`Chat::selected_text`) groups wrapped rows by logical source row: fully covered rows emit clean logical text (`BodySource::row_copy_text` — gutterless `Numbered` code, marker-only diffs), boundary rows emit WYSIWYG visual fragments (`segment::visual_row_text` renders one wrapped row into a scratch buffer, `slice_visual` cuts by column).
  - Up within `CLICK_SLOP` is a click and toggles blocks; double/triple click select word/logical row.
  - Copy: Ctrl+C (input selection first, then chat; clears the input or requests quit when nothing is selected), Ctrl+X cuts in the input; mouse-up copy is opt-in via `[ui] copy-on-select` (default off). Transport is OSC 52 (`escape::set_clipboard`) — no clipboard dependency.

### Sidebar & status

- **Usage display**:
  - Every completed LLM request (main stream + workers) emits additive `Event::UsageUpdate`; each turn end emits authoritative `Event::UsageSnapshot` (session cumulative totals) that *replaces* client-side totals, so cancelled/retried turns self-heal.
  - The context window comes from the Selune catalog (`catalog_context_length` in `src/tui/app.rs`); the window line shows the latest main-request context footprint (`UsageUpdate.context_tokens`, from `shuvarie_llm::context_footprint` — workers excluded, they run separate conversations) as % of it, bare window size while no footprint is known.
  - Below it: the latest main request's read tokens (`R…`, `shuvarie_llm::read_tokens`) and cache-hit % (`CH…%`, `cached_input_tokens` over read tokens), each hidden while unavailable.
  - On session load / fork restores the block re-seeds from `Session.last_usage` (derived from the persisted `request_json`); `Reset`/`CompactionStarted` clear it.
- **Collapsible sidebar** — `[ui] sidebar auto|expanded|collapsed` (default `auto`) sets the default expansion; `Sidebar::collapsed_at` resolves a Ctrl+W manual override first, then the pref, then auto-collapse below 80 cols. When collapsed the sidebar column vanishes and a one-line context+LSP status line (`Sidebar::collapsed_line`, no skills/todos) renders between the status row and the key-hint footer.
- **Workspace path + branch** — the sidebar's first section is the cwd (home shortened to `~`, over-long intermediate components compressed to initials, current dir bright) plus the git branch (`⎇ <name>`, short hash when detached) via gitoxide (`gix`, minimal features) in `src/tui/workspace.rs`, detected once at startup in `run_tui` and installed through `SidebarMessage::SetWorkspace`. When collapsed, the same info renders right-aligned on the key-hint footer line (`Sidebar::workspace_line`).
- **Search** — `Ctrl+R` is hybrid: instant BM25 FTS over `messages`, then a debounced semantic search over `message_embeddings` (cosine distance), merged by reciprocal rank fusion.

## Conventions

- **Edition 2024**, resolver v3, workspace-inherited package metadata (`version.workspace`, etc.).
- **Errors**: `color-eyre` in the binary; library crates use typed errors (thiserror or a manual enum) rather than `eyre`.
- **Comments**: do not add comments unless explicitly requested.
- **Formatting/lint**: `cargo fmt` (defaults; `rustfmt.toml` is intentionally empty) and `cargo clippy --all-targets` must pass.
- **Commits**: follow the `Assisted-By:` trailer convention in `CONTRIBUTING.md` for AI-assisted commits. Do not commit unless explicitly asked.
- **Skills**:
  - `.agents/skills/` contains `ratatui`, `tokio`, `turso-db`, `toasty`, and `rig` references — consult them when working on TUI rendering, async, storage, ORM, or LLM provider/agent code.
  - The app loads them (`shuvarie-core::skills`) into the sidebar and agent preamble following the Agent Skills standard (recursive discovery, validated frontmatter, `/skill:<name> [args]` invocation, `disable-model-invocation`); `/reload` re-discovers without a restart.
  - The `skill` tool (`crates/core/src/tools/skill.rs`) returns the frontmatter-stripped SKILL.md (unknown names list the available ones) or reads further resource files via `path` (relative, canonicalized, escape-rejected, text-only); it joins the main + read-worker rosters only when the skill set is non-empty (scene-overridable like the other ungated tools).
- **Reuse**: for logic used in multiple places, prefer shared utils/modules.
- **Modules**: when creating a mod directory, keep the `<mod_name>.rs` in the parent directory rather than creating a `mod.rs`.

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

Nix flakes: `nix build .#` builds the binary and runs the test suite in checkPhase; `nix develop` provides the toolchain. The flake splices the `vendor/selune` submodule from its own `selune` input in `postUnpack`, because Nix drops submodule gitlinks from git trees.

## Issues and pull requests

Reviewing PRs:

- Do not run `gh pr checkout`, `git switch`, or otherwise move the worktree to the PR branch unless the user explicitly asks.
- Use `gh pr view`, `gh pr diff`, `gh api`, and local `git show`/`git diff` against fetched refs to inspect PR metadata, commits, and patches.
- If you need PR file contents, fetch/read them into temporary files or use `git show <ref>:<path>`.

Posting issue/PR comments:

- Write the comment to a temp file and post with `gh issue/pr comment --body-file` (never multi-line markdown via `--body`).
- Keep comments concise, technical, in the user's tone; end every AI-posted comment with the AI-generated disclaimer line specified by the originating prompt (e.g. `This comment is AI-generated`).

Closing issues via commit:

- Include `fixes #<number>` or `closes #<number>` in the message so merging auto-closes the issue. For multiple issues, repeat the keyword per issue (`closes #1, closes #2`); a shared keyword (`closes #1, #2`) only closes the first.

## Tips

- When changes are made, check AGENTS.md for stale guidance and update it.
- Keep AGENTS.md concise — only add important points that every AI agent should know before working on this project.