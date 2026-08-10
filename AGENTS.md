# AGENTS.md

Shuvarie (シュヴァリエ, "chevalier") is a terminal-based AI agent coding tool built in Rust. This file orients AI agents (and humans) to the codebase, conventions, and intended architecture so that changes stay consistent.

## Workspace crates

- `./` (binary `shuvarie`) — TUI rendering, event loop, input handling, channel wiring. **No business logic lives here.** Renders state produced by `shuvarie-core`.
- `./crates/core/` (`shuvarie-core`) — App state, config loading, storage layer (Turso + Toasty ORM), and the async core task that orchestrates LLM and database work. Owns the domain `Message`/`Update` logic. Depends on `shuvarie-llm` (config holds a `shuvarie_llm::Provider`).
- `./crates/llm/` (`shuvarie-llm`) — Thin wrapper over `rig`: `Provider` enum (8 variants) with metadata, `ProviderClient` builder, `ModelInfo`, model listing, streaming completion API, message types. No TUI concerns.

When adding a feature: put domain logic in `shuvarie-core`, LLM/SDK glue in `shuvarie-llm`, and only rendering + input dispatch in the root binary.

## Architecture

### The Elm Architecture (TEA) — hierarchical

The TUI follows `handle_event → message → update → return`:

- `handle_event` (pure, no side effects): maps a terminal `Event` (or an async channel message) to a message (e.g. `AppMessage`). Takes `&self`/`&App` only — never mutates.
- `update` (mutates the model): consumes a message and may produce a return (e.g. `AppReturn::Quit`) or an effect for the parent.
- `view(&self, frame: &mut Frame<'_>, area: Rect)`: draws current state. A TEA model is **not** required to implement `ratatui::Widget`; the dedicated `view` method takes a `Frame` + `Rect` and may compose multiple widgets, so it is not limited to a single widget's render contract. No side effects in `view`.

The root `App` composes **submodels**, each following the same TEA shape with its own message enum, `handle_event`, `update`, and `view`. Submodels own their state and logic; the parent dispatches events, forwards grouped messages, handles routing, and processes core events:

```
App (parent)
├── ModelSelectScreen   — ModelSelectMessage, handle_event, update(&UpdateCtx), view
├── CommandMenu         — CommandMenuMessage, handle_event, update → Option<CommandMenuEffect>, view
└── ChatScreen          — (placeholder; grows its own enum in M4)
```

**Module layout** (`src/tui/`):

| File | Role |
|---|---|
| `tui.rs` | Event loop: `EventStream` + `tokio::select!`, terminal init/deinit, calls `App::view` |
| `app.rs` | Parent `App`: `AppMessage` (grouped), `handle_event` dispatch, `update`, `view` (title bar + footer + content routing) |
| `context.rs` | `UpdateCtx` — shared `Config` + `Command` sender passed to submodel `update` calls |
| `model_select.rs` | `ModelSelectScreen` submodel + `ModelSelectMessage` + `AddProviderForm` |
| `command_menu.rs` | `CommandMenu` submodel + `CommandMenuMessage` + `CommandMenuEffect` |
| `chat.rs` | `ChatScreen` placeholder |
| `search.rs` | `Search` — nucleo fuzzy `filter_indices` helper |
| `widgets.rs` | `InputBuffer` — char buffer + cursor for forms/search |
| `theme.rs` | Color palette + style helpers (see UI design below) |
| `escape.rs` | CSI escape sequences for alt-screen enter/exit |

**Message grouping**: `AppMessage` wraps submodel messages — `ModelSelect(ModelSelectMessage)`, `CommandMenu(CommandMenuMessage)` — plus parent-only variants (`Quit`, `OpenModelSelect`, `OpenCommandMenu`, `Pong`, `ConfigSaved`, `ConfigError`). Core `Event`s are mapped to `AppMessage` via `App::map_core_event`, which wraps `ModelsLoaded`/`ModelsError` into `ModelSelect(...)` and keeps `ConfigSaved`/`ConfigError` at the parent level (the parent reloads config from disk on `ConfigSaved` so submodels see fresh provider data).

**Event dispatch flow** (`App::handle_event`, route-aware):

1. If `CommandMenu` is open → `CommandMenu::handle_event` → `CommandMenuMessage`.
2. Else if `ModelSelectScreen` add-form is open → `ModelSelectScreen::handle_add_form_event`.
3. Else if `ModelSelectScreen` search is active → `ModelSelectScreen::handle_search_event`.
4. Else `Ctrl+P` → `OpenCommandMenu` (parent-level global keybind).
5. Else route to the active screen's `handle_event` (`q`/`Tab` handled at parent for `Chat`).

**Update forwarding**: `App::update` matches `AppMessage` — parent-only variants are handled in-place; submodel variants are forwarded to `submodel.update(msg, &self.ctx)`. `CommandMenu::update` returns `Option<CommandMenuEffect>`; the parent matches effects (`OpenModelSelect`, `AddProvider`, `Quit`) to perform parent-level actions (route changes, opening the add form, quitting).

**`UpdateCtx`** (`src/tui/context.rs`): carries `config: Config` and `cmd_tx: Sender<Command>`. Passed by shared reference (`&UpdateCtx`) to submodel `update` calls so they can read config and send commands without owning them. The parent owns the `UpdateCtx` and reloads `config` from disk on `ConfigSaved`.

**Adding a feature**: add a message variant to the relevant submodel's enum (and an effect/return if the parent needs to act), an `update` arm, and `view` logic. Keep side effects out of `handle_event` and `view` — send commands via `ctx.send(...)` in `update` only. If a new submodel is needed, add a message enum, `handle_event`, `update(&UpdateCtx)`, and `view`, then compose it in `App` and add a grouped `AppMessage` variant.

### Async runtime: Tokio

`main` is `#[tokio::main]`. The TUI loop runs on the main thread using `termina`'s `EventStream` (the `event-stream` feature) so the loop can `tokio::select!` between terminal key events and core events without blocking. All LLM and database work runs on a spawned **core task** (`shuvarie_core::run`). Communication is via `tokio::sync::mpsc` channels:

- TUI → core: commands (e.g. `SendMessage`, `SelectModel`, `LoadHistory`) — `shuvarie_core::Command`.
- Core → TUI: events (e.g. `TokenReceived`, `StreamDone`, `ModelsLoaded`, `HistoryLoaded`) — `shuvarie_core::Event` — that are converted into `AppMessage` variants and fed into `update`.

Keep the TUI thread free of `await`s on blocking work; offload any blocking work to the core task (or `spawn_blocking`). The `select!` loop wakes on either a terminal event or a core event, so live updates (model lists, streaming tokens) render without requiring a keypress.

### Storage: hybrid

- **Config file** (`~/.config/shuvarie/config.toml`, via `toml` + `serde`): API keys, base URLs, the active provider/model, UI preferences. Keeps secrets out of the database.
- **Database** (Turso embedded SQLite + Toasty ORM): chat sessions and message history. Added in a later milestone; provider config stays in the TOML file regardless.

### LLM providers

All provider access goes through `rig` in `shuvarie-llm`. Supported providers, via `rig`'s built-in clients:

- **OpenAI-compatible** — OpenAI proper plus any OpenAI-compatible endpoint (OpenRouter, local `llama.cpp` server, etc.).
- **Anthropic** — Claude models.
- **Gemini** — Google models.
- **Ollama** — local models, no API key required (good for offline development).

`shuvarie-llm` exposes a `Provider` enum and a model-listing/streaming API; the root binary never calls `rig` directly.

### Rendering stack

`ratatui` with the `termina` backend feature (`default-features = false`, curated feature set in the root `Cargo.toml`). Alternate-screen enter/exit is handled manually via CSI escapes in `src/tui/escape.rs` — do not switch to a backend-managed alt-screen toggle without reason.

### UI design — modern, borderless, muted

The visual style is defined in `src/tui/theme.rs` and used by all submodel `view` methods. Follow these principles when adding or modifying UI:

**Color palette** — all colors are `Color::Rgb` (true color), Catppuccin Mocha-inspired. Never use pure RGB primaries (`Color::Red`, `Color::Green`, etc.) or the `Stylize` shorthand colors (`.red()`, `.cyan()`, etc.) in UI code — import from `theme` instead. The palette:

| Constant | Role |
|---|---|
| `BG` | App background (deep slate, fills the whole frame) |
| `SURFACE` | Unfocused pane/section background |
| `SURFACE_FOCUSED` | Focused pane background (brighter slate) |
| `OVERLAY` | Popup/modal background |
| `ACCENT` | Focus color — titles, keybindings, active markers, highlights |
| `ACCENT_BG` | Selection background (muted blue, not harsh inversion) |
| `TEXT` | Primary text in focused panes |
| `TEXT_DIM` | Secondary text, unfocused pane items |
| `TEXT_MUTED` | Hints, descriptions, placeholders |
| `SUCCESS` / `WARNING` / `ERROR` | Semantic colors (sage / amber / coral — never pure RGB) |

**Windows and sections** — no borders. Use `theme::section_block(title, focused)` which returns a `Block::new()` with a background fill (`SURFACE` or `SURFACE_FOCUSED`) and the title rendered inside via `.title_top()` in `ACCENT` + bold. The block has `Padding::horizontal(1)` so content is inset from the bg edge. `Block::inner()` reserves the title row, so `List`/`Paragraph` content starts below the title automatically.

**Overlays** — use `theme::overlay_block(title)` for popups (command menu, add-provider form). It fills with `OVERLAY` bg and has `Padding::uniform(1)` for breathing room. `Clear` is still rendered first to wipe underlying cells before the bg fill.

**Selection** — `highlight_style` uses `Style::new().bg(ACCENT_BG).fg(TEXT)` (no `Modifier::REVERSED`). The `highlight_symbol("▶ ")` stays in `ACCENT` as the cursor marker. Active provider/model items use `theme::active_marker(is_active)` → `●` in `ACCENT` (distinct from the selection cursor `▶`).

**Layout** — `App::view` renders a 3-row vertical layout:
1. **Title bar** (1 row) — `theme::title_bar(...)` with `SURFACE` bg: app name in `ACCENT` bold, route name in `TEXT_DIM`, active provider:model in `TEXT`.
2. **Content** (`Min(0)`) — inset by 1 cell left/right (margin between pane edges and screen edge). Delegated to the active submodel's `view`.
3. **Status footer** (1 row) — `SURFACE` bg: left = contextual help via `theme::help_line(...)` (keybindings as styled `Span`s: keys in `ACCENT`, labels in `TEXT_MUTED`); right = error/loading state in `ERROR`/`WARNING`.

**Pane gutter** — two-pane layouts use `Layout::horizontal(...).spacing(1)` so the `BG` shows through as a 1-cell gap between panes (visual separation without borders).

**Search input pill** — when search is active, the search `Paragraph` gets `SURFACE_FOCUSED` bg + `ACCENT` fg to look like an input field; inactive shows a `TEXT_MUTED` hint.

**Help text** — use `theme::help_line(&[("key", "label"), ...])` to render keybinding rows with keys in `ACCENT` and labels in `TEXT_MUTED`, separated by spacing. Never render help as a flat unstyled string.

When adding a new screen or widget, use the `theme` helpers — do not inline `Block::bordered()`, raw `Color` values, or `Stylize` shorthand colors.

### Launch flow

On startup `shuvarie-core` loads config and checks for connected providers. If none are configured, the TUI opens to the **model selection / connect provider** screen first; otherwise it opens to the chat view. Model selection is also reachable from a menu at any time.

## Conventions

- **Edition 2024**, resolver v3, workspace-inherited package metadata (`version.workspace`, etc.).
- **Errors**: `color-eyre` in the binary; library crates should use typed errors (thiserror or a manual enum) rather than `eyre`.
- **Comments**: do not add comments unless explicitly requested (see project-wide convention).
- **Formatting/lint**: `cargo fmt` (defaults; `rustfmt.toml` is intentionally empty) and `cargo clippy --all-targets` must pass.
- **Commits**: follow the `Assisted-By:` trailer convention in `CONTRIBUTING.md` for AI-assisted commits. Do not commit unless explicitly asked.
- **Skills**: `.agents/skills/` contains `ratatui`, `tokio`, `turso-db`, `toasty`, and `rig` references — consult them when working on TUI rendering, async, storage, ORM, or LLM provider/agent code. The `ratatui` skill covers Ratatui 0.30.x and the Termina backend this project uses (instead of the default Crossterm backend). The `rig` skill covers Rig 0.41.x (the `shuvarie-llm` dependency) — provider clients, agents, tools, streaming, RAG, and memory; the root binary never calls `rig` directly.

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Known issues to resolve

- `shuvarie-llm` implements model listing and `ProviderClient::build` (M1); streaming completion and message types land in M4/M5.
- `shuvarie-core` implements config load/save, `has_connected_providers()`, the `Command`/`Event` enums, and the core task (`run`) that owns the in-memory config and orchestrates provider/model listing (M2/M3). The storage layer (Turso + Toasty) and domain `Message`/`Update` logic land in M6.
- The core task is spawned in `main`; the TUI loop uses `termina`'s `EventStream` + `tokio::select!` to wake on terminal or core events (M3 done).
- The chat view is a placeholder (M4 will implement message composition and rendering).
- Model search uses `nucleo` (fuzzy matcher) via `src/tui/search.rs`; the Ctrl+P command menu (`src/tui/command_menu.rs`) is a small extensible registry of `CommandEntry`s.

## Tips

### AGENTS.md

When changes are made, it's a good practice to check AGENTS.md if there is stale guidance and update it.

### ROADMAP.md

See `ROADMAP.md` for the milestone plan. Update it when milestones ship or scope changes.
