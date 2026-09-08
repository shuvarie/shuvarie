# AGENTS.md

Shuvarie (シュヴァリエ, "chevalier") is a terminal-based AI agent coding tool built in Rust. This file orients AI agents (and humans) to the codebase, conventions, and intended architecture so that changes stay consistent.

## Workspace crates

- `./` (binary `shuvarie`) — TUI rendering, event loop, input handling, channel wiring. **No business logic lives here.** Renders state produced by `shuvarie-core`.
- `./crates/core/` (`shuvarie-core`) — App state, config + connections loading, the async core task that orchestrates LLM and database work, the domain `Session`/`Event`/`Command` logic, and the agent tools (read/write/edit files, `apply_patch`, run commands, list dirs, grep, glob, `lsp`, `webfetch`, `question`, `todo`) with deterministic permission rules. Persists through `shuvarie-db`; owns the `shuvarie_lsp::LspManager` and the worker-agent roster.
- `./vendor/selune/` (`selune`, git submodule) — uniform source of provider + model info (Catwalk format): wire types, a hosted catalog client, offline embedded configs. A connection's transport is a `selune::ProviderType` (kebab-case); `shuvarie-core::catalog` derives pricing/context/key requirements from the matching catalog entry (id stored in the connection's `catalog` field); `find_model` matches catalog ids loosely — exact, vendor-tail (`vendor/model`), dated snapshot alias in either direction (`claude-sonnet-4-5` ↔ `claude-sonnet-4-5-20250929`), or a unique tag-stripped variant (`glm-5.3-flash` ↔ `glm-5.3-flash:cloud`; ambiguous bases like `gpt-oss` match nothing).
- `./vendor/frameplay/` (`frameplay-lib`, git submodule) — wall-clock frame repeater powering the spinner animations (`src/tui/spinner.rs`); the render loop wakes at each frame boundary via `Frameplay::time_to_next_frame`.
- `./crates/llm/` (`shuvarie-llm`) — thin wrapper over `rig` (Rig 0.42.x — see the `rig` skill): provider clients, model listing, streaming multi-turn agent chat (`stream`, `run_worker`), `StreamItem`/message types, `WorkerAgent`, and the context-management `ContextHook`. No pricing, no TUI concerns.
- `./crates/db/` (`shuvarie-db`) — persistence layer: Toasty models, the `Store` wrapper over Turso embedded SQLite, embedded schema migrations, and the `migrate` bin. No TUI concerns.
- `./crates/highlight/` (`shuvarie-highlight`) — markdown + code highlighting for the chat pane; emits unwrapped ratatui `Line`s (wrapping stays with the TUI `Paragraph`).
- `./crates/lsp/` (`shuvarie-lsp`) — LSP client lifecycle: built-in per-language server registry + config, server spawn via `async-lsp`, and the `LspManager` (start/stop/restart/analyze, diagnostics collection). Stays KDL-free.

When adding a feature: put domain logic in `shuvarie-core`, provider/model data + pricing in `selune` (via `shuvarie-core::catalog`), LLM/SDK glue in `shuvarie-llm`, storage/ORM glue in `shuvarie-db`, markdown/code highlighting in `shuvarie-highlight`, LSP client lifecycle in `shuvarie-lsp`, and only rendering + input dispatch in the root binary.

## Architecture

### The Elm Architecture (TEA) — hierarchical

The TUI follows `map_event → message → update → return`. The root `App` composes submodels (session screen, chat pane, overlays), each with its own message enum, `map_event`, `update`, and `view`; the parent dispatches events and forwards grouped messages.

- `map_event` — pure (`&self` only), no side effects; terminal events dispatch overlay-aware first, then route-aware.
- `update` — mutates the model and may return an effect; **all side effects happen here** (send core commands via `ctx.send(...)`, `UpdateCtx` carries `Connections` + the `Command` sender).
- `view` — draws state, `&self`, no side effects, may compose multiple widgets (a model need not implement `ratatui::Widget`).

New components follow the same shape (message enum + `new` + `view` + `update` when needed), composed in `App` with a grouped `AppMessage` variant. Core events (`shuvarie_core::Event`) are mapped to `AppMessage` variants in `App::map_event`.

Startup loads config + connections and the working-directory store (`.shuvarie/data.db`); the TUI opens on the Session route — the first message lazily creates the session row; with no providers configured a Welcome overlay shows.

### Async runtime: Tokio

`main` is `#[tokio::main]`. The TUI loop runs on the main thread using termina's `EventStream`, `tokio::select!`-ing between terminal events and core events (one draw per frame budget; `[ui].frame_rate` default 60). All LLM/database work runs on the spawned **core task** (`shuvarie_core::run`); communication is mpsc: TUI → core `Command`s, core → TUI `Event`s (wrapped as `Event::Core`). Keep blocking work off the TUI thread (offload to the core task or `spawn_blocking`).

### Storage: hybrid

- **Config** (`shuvarie-core::config`) — layered chain `$cwd/shuvarie.kdl` → `$cwd/.shuvarie/config.kdl` → global `~/.config/shuvarie/config.kdl`, merged per top-level section (except `lsp.servers`, key-by-key); KDL via kdl-serde. UI/agent/context/lsp/skills/retry preferences live here; no secrets.
- **Connections** (`connections.kdl`, same module) — providers keyed by `id` plus the active provider/model; hand-rolled KDL in `connections_kdl`. `kind` is the rig transport (a `selune::ProviderType` in kebab-case, e.g. `openai`, `ollama`); the optional `catalog` child carries the Selune catalog id used for pricing/context/key-requirement lookups (legacy configs put the catalog id in `kind` — still resolved). Secrets live here — never in the database or `config.kdl`.
- **Database** (`.shuvarie/data.db`) — Turso embedded SQLite via the Toasty ORM: sessions (UUID v7), messages (positioned reasoning segments + interrupted flag), tool calls (file-change JSON + original/new content for undo), undo log, message embeddings. Schema is managed with toasty migrations embedded in the binary — regenerate with `cargo run -p shuvarie-db --bin migrate -- migration generate --name <change>` (see the `toasty` skill).
- **Undo / redo / replay** — undo moves the last user+assistant turn into `undo_log` and reverts file changes; redo restores it; replay = undo + re-send; `/continue` re-streams after an interrupted turn (prompt from `shuvarie_core::session::CONTINUE_PROMPT`, available only when the last message is an incomplete assistant reply). Interrupted turns are re-streamed automatically by the retry/overflow-continue paths. `run_shell` effects are never reverted.
- **Prompt steering** — a prompt submitted while the agent loop is busy (streaming or connection-retry wait) is queued in the core task's run loop (`steered` in `core_task.rs`) instead of erroring; the queue's front is dispatched as the next user turn at the next completed action boundary of the active stream (after the tool batch settles, or after a thinking/text segment — the stream task cuts the turn, persisting it interrupted, and reports `StreamOutcome::Preempted`; a shared `SteerSignal` gates the cut so `CancelStream` never double-persists), when the turn finishes, or when it is cancelled. A dispatched prompt only preempts the stream it was queued during (signal disarmed at dispatch); session-level transitions (new/load/delete) wipe the queue; `Alt+Up` / `Alt+Shift+Up` recall the newest entry into the input (overwrite vs. stacked prepend). The TUI mirrors the queue via `Event::PromptSteered` / `TurnStarted { steered }` / `SteeredRecalled` / `SteeredCleared` and renders queued prompts in the chat pane.
- **Context management** — three layers keep long turns from blowing up input tokens: tool-output caps (`truncate.rs`), a history-budget `ContextHook` (rig `AgentHook` in `shuvarie-llm`) that trims old tool results to recovery-hint stubs, and overflow-triggered LLM compaction (`compaction.rs`) that auto-continues after summarizing older messages — the cut keeps the most recent `keep_recent_tokens` tokens verbatim (floored at 4 messages), repeated compactions resume from the previous summary (folded in as prior context instead of re-summarizing it), and the serialized head carries tool activity plus ground-truth read/modified file lists. Config under `[context]`.
- **Sidebar context display** — every completed LLM request (main stream + workers) emits `StreamItem::Usage` → additive `Event::UsageUpdate`; each turn end emits authoritative `Event::UsageSnapshot` (session cumulative totals) that *replaces* client-side totals (so cancelled/retried turns self-heal). The context window comes from the Selune catalog (`catalog_context_length` in `src/tui/app.rs`); the window line shows the latest main-request context footprint (`UsageUpdate.context_tokens`, from `shuvarie_llm::context_footprint`; workers excluded since they run separate conversations) as % of it — bare window size while no footprint is known (fresh/loaded session, during compaction).
- **Search** — `Ctrl+R` is hybrid: instant BM25 FTS over `messages`, then a debounced semantic search over `message_embeddings` (cosine distance), merged by reciprocal rank fusion.

### LLM providers

All provider access goes through `rig` in `shuvarie-llm` — OpenAI-compatible, Anthropic, Gemini, Ollama, Ollama Cloud. The root binary never calls `rig` directly.

### Rendering stack

`ratatui` 0.30.x with the `termina` backend (not Crossterm) — see the `ratatui` skill. Alt-screen and the Kitty-keyboard/mouse-tracking protocols are managed manually via CSI escapes in `src/tui/escape.rs`.

### UI design — borderless, muted

The visual style lives in `src/tui/theme.rs` (true-color `Color::Rgb`, medieval/heraldic palette). Import colors and use the helpers (`section_block`, `overlay_block`, `help_line`, `active_marker`) — never inline `Block::bordered()`, raw `Color` constants (`Color::Red`…), or `Stylize` shorthand colors.

- **Selection** is baked into the `ListItem` (`▶` prefix + `ACCENT_BG` row style via `list::render_list_item`); lists render statelessly and scroll offsets are computed in `update` via `list::scroll_offset_for` — do not introduce `ListState`/`TableState`/`ScrollbarState`.
- The chat history pane (`session/chat.rs` + `session/virtualizer.rs`) is the one stateful exception: turn-based virtualization with `RefCell` render caches, painting only the visible viewport plus overscan.
- Format tokens/cost via the `utils/num.rs` helpers; render help rows via `theme::help_line` — never flat unstyled strings.

**Permission rules** (`crates/core/src/permissions.rs`) — there is no interactive permission popup: tool calls either run or fail deterministically. Reads are permitted anywhere except under hidden paths (any `.` component; workspace-root `.agents`/`.shuvarie` are exempt), writes only inside the working directory and never under hidden paths; `run_shell`/`webfetch` are not path-gated.

## Conventions

- **Edition 2024**, resolver v3, workspace-inherited package metadata (`version.workspace`, etc.).
- **Errors**: `color-eyre` in the binary; library crates use typed errors (thiserror or a manual enum) rather than `eyre`.
- **Comments**: do not add comments unless explicitly requested (see project-wide convention).
- **Formatting/lint**: `cargo fmt` (defaults; `rustfmt.toml` is intentionally empty) and `cargo clippy --all-targets` must pass.
- **Commits**: follow the `Assisted-By:` trailer convention in `CONTRIBUTING.md` for AI-assisted commits. Do not commit unless explicitly asked.
- **Skills**: `.agents/skills/` contains `ratatui`, `tokio`, `turso-db`, `toasty`, and `rig` references — consult them when working on TUI rendering, async, storage, ORM, or LLM provider/agent code. The app loads these skills (`shuvarie-core::skills`) into the sidebar and agent preamble, following the Agent Skills standard: recursive discovery, validated frontmatter, XML preamble block, `/skill:<name> [args]` invocation, and `disable-model-invocation`.
- **kdl-serde quirk**: kdl-serde drops entries serializing to `Bool(false)`/`Null`, so default-on flags are stored inverted (`disabled #true` = off, omitted = on) and non-Option nested fields need `deserialize_with = "kdlserde::de_default"`.
- **Reuse**: for logic used in multiple places, prefer shared utils/modules.
- **Modules**: when creating a mod directory, keep the `<mod_name>.rs` in the parent directory rather than creating a `mod.rs`.

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Tips

### AGENTS.md

When changes are made, it's a good practice to check AGENTS.md if there is stale guidance and update it.

Keep AGENTS.md concise. Only add important points that every AI agent should know before working on this project.

### In development notice

Since the project is not released yet. It's safe to update the `0000` migration directly.
