# AGENTS.md

Shuvarie (シュヴァリエ, "chevalier") is a terminal-based AI agent coding tool built in Rust. This file orients AI agents (and humans) to the codebase, conventions, and intended architecture so that changes stay consistent.

## Workspace crates

- `./` (binary `shuvarie`) — TUI rendering, event loop, input handling, channel wiring. **No business logic lives here.** Renders state produced by `shuvarie-core`.
- `./crates/core/` (`shuvarie-core`) — App state, config loading, the async core task that orchestrates LLM and database work, and the six agent tools (`tools.rs`: read/write/edit files, run commands, list dirs, grep). Owns the domain `Session`/`Event`/`Command` logic and persists through `shuvarie-db`. Depends on `shuvarie-llm` (config holds a `shuvarie_llm::Provider`; tools implement `shuvarie_llm::Tool`).
- `./crates/llm/` (`shuvarie-llm`) — Thin wrapper over `rig`: `Provider` enum (9 variants) with metadata, `ProviderClient` builder, `ModelInfo`, model listing, non-streaming completion API (`complete` via `rig::completion::Chat`), streaming agent chat (`stream` — multi-turn with tools), the portable `Tool` trait + `ToolDefinition` (the boundary type `shuvarie-core` implements; rig `DynamicTool`s are built inside `shuvarie-llm`), `ChatMsg`/`Role` message types. No TUI concerns.
- `./crates/db/` (`shuvarie-db`) — Persistence layer: Toasty models (`Session`, `Message`), the `Store` wrapper over `toasty::Db` (Turso embedded SQLite via `toasty-driver-turso`), embedded schema migrations, and a `migrate` bin (toasty-cli) for managing them. No TUI concerns.

When adding a feature: put domain logic in `shuvarie-core`, LLM/SDK glue in `shuvarie-llm`, storage/ORM glue in `shuvarie-db`, and only rendering + input dispatch in the root binary.

## Architecture

### The Elm Architecture (TEA) — hierarchical

The TUI follows `map_event → message → update → return`:

- `map_event` (pure, no side effects): maps a terminal `Event` (or an async channel message) to a message (e.g. `AppMessage`). Takes `&self`/`&App` only — never mutates.
- `update` (mutates the model): consumes a message and may produce an effect (e.g. `AppEffect::Quit`) or an effect for the parent.
- `view(&self, frame: &mut Frame<'_>, area: Rect)`: draws current state. A TEA model is **not** required to implement `ratatui::Widget`; the dedicated `view` method takes a `Frame` + `Rect` and may compose multiple widgets, so it is not limited to a single widget's render contract. No side effects in `view`.

The root `App` composes **submodels**, each following the same TEA shape with its own message enum, `map_event`, `update`, and `view`. Submodels own their state and logic; the parent dispatches events, forwards grouped messages, handles routing, and processes core events:

```
App (parent)
├── HomeScreen          — HomeMessage, map_event(&self), update → Option<HomeEffect>, view(&self)
├── SessionScreen       — SessionMessage, map_event(&self), update → Option<SessionEffect>, view(&self)
├── CommandMenu         — CommandMenuMessage, map_event, update → Option<CommandMenuEffect>, view(&self)
├── AddProviderForm     — AddProviderMessage, map_event(key, stage), update → AddProviderOutcome, view(&self)
├── ModelPicker         — ModelPickerMessage, map_event, update → Option<ModelPickerEffect>, view(&self)
├── Welcome             — WelcomeMessage, map_event, (no update — parent handles directly), view(&self)
└── ConfirmQuit         — ConfirmQuitMessage, map_event, (no update — parent handles directly), view(&self)
```

When creating a new component, it's usually expected to be a TEA model, unless such an architecture doesn't fit the requirement.

There should be a `new` and a `view` method, and a `update` method when data updating is required.

**Module layout** (`src/tui/`):

| File | Role |
|---|---|
| `tui.rs` | Event loop: `EventStream` + `tokio::select!`, terminal init/deinit, calls `App::view` |
| `event.rs` | `Event` enum — `Terminal(termina::Event)` / `Core(shuvarie_core::Event)` — the single input type for `App::map_event` |
| `app.rs` | Parent `App`: `Route` (`Home`/`Session`), `Overlay` enum, `AppMessage` (grouped), `map_event` dispatch, `update`, `view` (content routing + overlays) |
| `context.rs` | `UpdateCtx` — shared `Config` + `Command` sender passed to submodel `update` calls |
| `home.rs` | `HomeScreen` submodel + `HomeMessage` + `HomeEffect` (ASCII art + input) |
| `session.rs` | `SessionScreen` submodel + `SessionMessage` + `SessionEffect` (sidebar + chat history + input) |
| `sidebar.rs` | `Sidebar` view widget (Shuvarie+version, Context, LSP, Skills) |
| `logo.rs` | Double-sword-behind-shield ASCII art for the Home screen |
| `add_provider.rs` | `AddProviderForm` submodel (two-stage wizard: SelectKind → Details) + `AddProviderMessage` + `AddProviderOutcome` |
| `model_picker.rs` | `ModelPicker` overlay submodel + `ModelPickerMessage` + `ModelPickerEffect` |
| `session_picker.rs` | `SessionPicker` overlay submodel (session list, resume, `Ctrl+D` delete with confirm) + `SessionPickerMessage` + `SessionPickerEffect` |
| `command_menu.rs` | `CommandMenu` submodel + `CommandMenuMessage` + `CommandMenuEffect` |
| `welcome.rs` | `Welcome` overlay (first-run) + `WelcomeMessage` |
| `confirm_quit.rs` | `ConfirmQuit` overlay (Ctrl+C) + `ConfirmQuitMessage` |
| `search.rs` | `Search` — nucleo fuzzy `filter_indices` helper |
| `list.rs` | Stateless list rendering helpers — `scroll_offset_for` (scroll-into-view computed in `update`), `render_list_item`/`render_list_item_line` (bake selection `▶ ` prefix + `ACCENT_BG` row style into `ListItem`) |
| `widgets.rs` | `InputBuffer` — char buffer + cursor (Emacs movement, virtual cursor rendering) |
| `theme.rs` | Color palette + style helpers (see UI design below) |
| `escape.rs` | CSI escape sequences for alt-screen enter/exit |

**Message grouping**: `AppMessage` wraps submodel messages — `Home(HomeMessage)`, `Session(SessionMessage)`, `AddProvider(AddProviderMessage)`, `ModelPicker(ModelPickerMessage)`, `CommandMenu(CommandMenuMessage)`, `Welcome(WelcomeMessage)`, `SessionPicker(SessionPickerMessage)` — plus parent-only variants (`OpenCommandMenu`, `RequestQuit`, `ConfirmQuit`, `CancelQuit`, `Resized { rows, cols }`, `ConfigSaved`, `ConfigError`, `ModelsLoaded`, `ModelsError`, `SessionsLoaded`, `SessionLoaded`, `SessionCreated`, `SessionDeleted`, `SessionError`). The TUI `Event` enum (`src/tui/event.rs`) unifies terminal and core inputs: `Event::Terminal(termina::Event)` and `Event::Core(shuvarie_core::Event)`. `App::map_event(ev, &app)` matches on `Event` — terminal events are dispatched overlay-aware then route-aware (below), while core events are mapped to `AppMessage` directly: stream/usage events (`TokenReceived`, `StreamDone`, `StreamError`, `StreamCancelled`, `UsageUpdate`) wrap into `Session(...)`, session events (`SessionLoaded`, `SessionCreated`, `SessionDeleted`, `SessionsLoaded`, `SessionError`) stay at the parent level (they populate the chat view and the SessionPicker), `Pong` closes the command menu, and `ConfigSaved`/`ConfigError`/`ModelsLoaded`/`ModelsError` stay at the parent level (the parent reloads config from disk on `ConfigSaved` so submodels see fresh provider data, and auto-selects a model on `ModelsLoaded`). `Resized` is sent from the `tui.rs` event loop when the terminal size changes and dispatched in `App::update` to the open overlay's `Resize { viewport_height }` submessage (computed from the popup rect + overlay block inner layout) so the overlay can recompute its scroll offset.

**Event dispatch flow** (`App::map_event`, overlay-aware, then route-aware):

1. `Event::Terminal` — if an overlay is open → route to that overlay's `map_event` (CommandMenu, AddProviderForm, ModelPicker, Welcome, ConfirmQuit).
2. Else `Ctrl+C` → `RequestQuit` (opens ConfirmQuit overlay; cancels the stream first if the session is streaming).
3. Else `Ctrl+M` → `OpenCommandMenu` (parent-level global keybind).
4. Else route to the active screen's `map_event` (`Home` or `Session`).
5. `Event::Core` — mapped to `AppMessage` directly (see Message grouping above).

**Update forwarding**: `App::update` matches `AppMessage` — parent-only variants are handled in-place; submodel variants are forwarded to `submodel.update(msg, &self.ctx)`. `CommandMenu::update` returns `Option<CommandMenuEffect>`; the parent matches effects (`OpenModelSelect`, `AddProvider`, `OpenSessionPicker`, `NewSession`) to perform parent-level actions (opening the ModelPicker overlay, opening the AddProviderForm, opening the SessionPicker / starting a fresh session). `HomeScreen::update` returns `Option<HomeEffect>` (`Submit` → start session); `SessionScreen::update` returns `Option<SessionEffect>` (`SendMessage` → send command to core). `AddProviderForm::update` returns `AddProviderOutcome` (`Submit` → add provider + set active + list models; `Cancel` → close or back to kind list). `ModelPicker::update` returns `Option<ModelPickerEffect>` (`Selected` → set active model). `SessionPicker::update` returns `Option<SessionPickerEffect>` (`LoadSession` → resume; `DeleteSession` → delete with a second `Ctrl+D` to confirm; `NewSession` → fresh conversation).

**`UpdateCtx`** (`src/tui/context.rs`): carries `config: Config` and `cmd_tx: Sender<Command>`. Passed by shared reference (`&UpdateCtx`) to submodel `update` calls so they can read config and send commands without owning them. The parent owns the `UpdateCtx` and reloads `config` from disk on `ConfigSaved`.

**Adding a feature**: add a message variant to the relevant submodel's enum (and an effect/return if the parent needs to act), an `update` arm, and `view` logic. Keep side effects out of `map_event` and `view` — send commands via `ctx.send(...)` in `update` only. If a new submodel is needed, add a message enum, `map_event`, `update(&UpdateCtx)`, and `view`, then compose it in `App` and add a grouped `AppMessage` variant.

The `view` method must have an immutable `self` reference (`&self`) as parameter — it never mutates the model.

### Async runtime: Tokio

`main` is `#[tokio::main]`. The TUI loop runs on the main thread using `termina`'s `EventStream` (the `event-stream` feature) so the loop can `tokio::select!` between terminal key events and core events without blocking. All LLM and database work runs on a spawned **core task** (`shuvarie_core::run`). Communication is via `tokio::sync::mpsc` channels:

- TUI → core: commands (e.g. `SendMessage`, `StartSession`, `ListModels`, `AddProvider`, `SetActiveModel`) — `shuvarie_core::Command`.
- Core → TUI: events (e.g. `MessageReceived`, `ReplyError`, `SessionStarted`, `ModelsLoaded`, `ConfigSaved`) — `shuvarie_core::Event` — wrapped in `Event::Core` by the `tui.rs` event loop, mapped to `AppMessage` variants by `App::map_event`, and fed into `update`.

Keep the TUI thread free of `await`s on blocking work; offload any blocking work to the core task (or `spawn_blocking`). The `select!` loop wakes on either a terminal event or a core event, so live updates (model lists, streaming tokens) render without requiring a keypress.

### Storage: hybrid

- **Config file** (`~/.config/shuvarie/config.toml`, via `toml` + `serde`): API keys, base URLs, the active provider/model, UI preferences. Keeps secrets out of the database.
- **Database** (`.shuvarie/data.db` in the working directory — Turso embedded SQLite + Toasty ORM): chat sessions and message history. Provider config stays in the TOML file regardless.
- **Migrations**: schema is managed with toasty migrations, not `push_schema` (which emits bare `CREATE TABLE` and fails on re-open). Migration files live in `crates/db/toasty/` and are embedded into the binary via `toasty::embed_migrations!`; `Store::open` applies them automatically. Regenerate with `cargo run -p shuvarie-db --bin migrate -- migration generate --name <change>`, then `apply` (or just reopen a store — the embedded set applies pending migrations at open). The `migrate` bin uses an in-memory DB and a programmatically-built `toasty-cli` config pointing at `crates/db/toasty` so it is CWD-independent.

### LLM providers

All provider access goes through `rig` in `shuvarie-llm`. Supported providers, via `rig`'s built-in clients:

- **OpenAI-compatible** — OpenAI proper plus any OpenAI-compatible endpoint (OpenRouter, local `llama.cpp` server, etc.).
- **Anthropic** — Claude models.
- **Gemini** — Google models.
- **Ollama** — local models, no API key required (good for offline development).
- **Ollama Cloud** — Ollama's hosted cloud service (`https://ollama.com`, Bearer API key from https://ollama.com/settings/keys); same Ollama API as local, so it reuses rig's `ollama` client with a key + base URL.

`shuvarie-llm` exposes a `Provider` enum and a model-listing/streaming API; the root binary never calls `rig` directly.

### Rendering stack

`ratatui` with the `termina` backend feature (`default-features = false`, curated feature set in the root `Cargo.toml`). Alternate-screen enter/exit is handled manually via CSI escapes in `src/tui/escape.rs` — do not switch to a backend-managed alt-screen toggle without reason.

### UI design — modern, borderless, muted

The visual style is defined in `src/tui/theme.rs` and used by all submodel `view` methods. Follow these principles when adding or modifying UI:

**Color palette** — all colors are `Color::Rgb` (true color), a medieval/heraldic theme: dark iron-and-stone backgrounds, antique gold accent (heraldic *or*), parchment-cream text, and weathered tinctures for semantics. Never use pure RGB primaries (`Color::Red`, `Color::Green`, etc.) or the `Stylize` shorthand colors (`.red()`, `.cyan()`, etc.) in UI code — import from `theme` instead. The palette:

| Constant | Role |
|---|---|
| `BG` | App background (forged iron black) |
| `SURFACE` | Unfocused pane/section background (castle stone) |
| `SURFACE_FOCUSED` | Focused pane background (polished iron) |
| `OVERLAY` | Popup/modal background (weathered stone) |
| `ACCENT` | Heraldic gold — titles, keybindings, active markers, highlights |
| `ACCENT_BG` | Selection background (dark bronze, not harsh inversion) |
| `TEXT` | Primary text in focused panes (parchment cream) |
| `TEXT_DIM` | Secondary text, unfocused pane items (faded vellum) |
| `TEXT_MUTED` | Hints, descriptions, placeholders (weathered stone) |
| `SUCCESS` / `WARNING` / `ERROR` | Heraldic tinctures: vert (sage) / amber / gules (weathered red) — never pure RGB |

**Windows and sections** — no borders. Use `theme::section_block(title, focused)` which returns a `Block::new()` with a background fill (`SURFACE` or `SURFACE_FOCUSED`) and the title rendered inside via `.title_top()` in `ACCENT` + bold. The block has `Padding::horizontal(1)` so content is inset from the bg edge. `Block::inner()` reserves the title row, so `List`/`Paragraph` content starts below the title automatically.

**Overlays** — use `theme::overlay_block(title)` for popups (command menu, add-provider form). It fills with `OVERLAY` bg and has `Padding::uniform(1)` for breathing room. `Clear` is still rendered first to wipe underlying cells before the bg fill.

**Selection** — selection is **baked into the `ListItem`** (not via `List::highlight_style`/`highlight_symbol` or `ListState`). `list::render_list_item`/`render_list_item_line` prepend a `▶ ` prefix `Span` in `ACCENT` for the selected row (`  ` for unselected) and set `ListItem::style` to `Style::new().bg(ACCENT_BG).fg(TEXT)` for the selected row (so the full row width gets the `ACCENT_BG` fill, matching the old `highlight_style` behavior) or `Style::new()` for unselected. Lists are rendered statelessly via `frame.render_widget(List::new(visible), area)` — never `render_stateful_widget`. The selected index (`selected: usize`) and scroll `offset` live as plain fields on the model, mutated in `update`. Active provider/model items use `theme::active_marker(is_active)` → `●` in `ACCENT` (distinct from the selection cursor `▶`).

**Stateless widgets & scroll-into-view** — `ratatui-widgets` implements `Widget` for `&Widget` references (e.g. `&List`, `&Paragraph`, `&Block`), so widgets are rendered by reference without state. Scroll-into-view is **not** handled by a stateful widget at render time; instead the model tracks `selected`, `offset`, and `viewport_height`, and `update` calls `list::scroll_offset_for(selected, offset, viewport_height, len)` to recompute the offset whenever selection or viewport size changes. The viewport height arrives via a `Resize { viewport_height }` submodel message that `App::update` dispatches on `AppMessage::Resized { rows, cols }` (sent from the `tui.rs` event loop when the terminal size changes, computed from the popup rect + overlay block inner layout). `view` then slices the visible items with `.skip(offset).take(list_area.height)` before building the `List`. Do not introduce `ListState`/`TableState`/`ScrollbarState` for list selection — use this pattern instead.

**Scrollable text panes** — the session chat history is the one exception to the stateless pattern: it renders a `Paragraph` (with `Wrap`) into a cached `tui_scrollview::ScrollView` buffer and keeps a `ScrollViewState` on the model. Because TEA `view` takes `&self` but `render_stateful_widget` needs `&mut state`, the state lives in a `RefCell<ScrollViewState>` (`scroll_state` on `SessionScreen`); the `ScrollView` itself is also cached (in a `RefCell`), rebuilt only when the content changes (`scroll_dirty` flag set in `update` via `mark_scroll_dirty()`) or the pane width changes — never on every frame, so scrolling stays responsive while streaming. `update` drives scrolling with `state.scroll_up()`/`scroll_down()`, and auto-follows streaming output with `if state.is_at_bottom() { state.scroll_to_bottom() }` on token/stream-done/submit (so the view only pins to the bottom while the user hasn't scrolled up). The scroll-view buffer height is computed from `Paragraph::line_count(width)` (feature `unstable-rendered-line-info`), with the rightmost column reserved for the scrollbar. The scrollbar is drawn manually (buffer cells: `█` thumb in `ACCENT` on a `TEXT_MUTED` track) — do not use `ratatui::widgets::Scrollbar`, whose position math cannot bottom-align the thumb (offset clamps at `content - viewport`, so the thumb stops short of the track end at the bottom of the scroll).

**Tool activity in the chat pane** — the agent's tool calls render as compact lines inside the same scroll view, associated with the assistant message they belong to. `SessionScreen` keeps `tools: Vec<ToolActivity>` (name, args, status, output, message index); `SessionMessage::ToolStarted`/`ToolFinished` append/update entries (cleared on reset/load like `messages`). `rebuild_scroll_view` emits each tool as a marker line (`›` ACCENT running, `✓` SUCCESS, `✗` ERROR) with the name and truncated args, plus an indented one-line output preview, followed by the assistant text. An empty assistant turn (tool-call-only reply) renders "(tool output only — no text reply)" instead of an invisible empty message.

**Layout** — `App::view` renders the active submodel's `view` over the full area, then any overlays (no global footer; no global title bar; branding lives in the sidebar and Home logo). Each screen owns its footer row:
- `HomeScreen::view` — centered ASCII art + centered input area, with a 1-row help footer below the input (no sidebar).
- `SessionScreen::view` — left sidebar (30 cols, `SURFACE` bg) + 1-cell `BG` gutter + content pane with centered title bar atop transparent chat history + input + status row, and a 1-row footer below the input. The footer shows contextual help via `theme::help_line(...)` (keybindings as styled `Span`s: keys in `ACCENT`, labels in `TEXT_MUTED`); while streaming it swaps to stop/commands hints. Session-scoped errors (`SessionMessage::ShowError`, e.g. model listing or session errors) render in the same footer row in `ERROR` instead of the help line — there is no separate global error row.

**Pane gutter** — two-pane layouts use `Layout::horizontal(...).spacing(1)` so the `BG` shows through as a 1-cell gap between panes (visual separation without borders).

**Virtual cursor** — `InputBuffer::cursor_line(text_color, cursor_color)` builds a `Line` with the character at the cursor position rendered with `Modifier::REVERSED` (the char's bg becomes the cursor color). When the cursor is past the last character, a reversed space block is appended. All text input areas (home, session, add-provider details) use this for cursor visualization.

**Emacs keybindings** — all text inputs support `Ctrl+B`/`Ctrl+F` (char left/right), `Alt+B`/`Alt+F` (word left/right), `Ctrl+A`/`Ctrl+E` (home/end), `Ctrl+D` (delete forward), `Ctrl+H` (backspace alias), `Ctrl+K` (kill to end). List navigations support `Ctrl+N`/`Ctrl+P` (next/prev). Arrow keys and physical keys remain alongside.

**Quit** — via `Ctrl+C` → `ConfirmQuit` overlay (Enter or `Ctrl+C` again to confirm, Esc to cancel). `q` no longer quits. The command menu is opened with `Ctrl+M`.

**Help text** — use `theme::help_line(&[("key", "label"), ...])` to render keybinding rows with keys in `ACCENT` and labels in `TEXT_MUTED`, separated by spacing. Never render help as a flat unstyled string.

When adding a new screen or widget, use the `theme` helpers — do not inline `Block::bordered()`, raw `Color` values, or `Stylize` shorthand colors.

### Launch flow

On startup `shuvarie-core` loads config, opens the working-directory session store (`.shuvarie/data.db`), and — when launched with `-c`/`--current` (the CLI flag threaded into `run` as `load_current`) — loads the most recent session into the chat view. By default the TUI opens to the **Home** route; `--current` resumes the last session (the existing `SessionLoaded` → `Route::Session` path). If no providers are configured, the TUI opens to the **Home** route with a **Welcome** overlay prompting the user to add a provider; otherwise it opens to the **Home** route directly (the loaded session is one `Enter` away, or via `Ctrl+M` → "Switch session"). Provider/model management is overlay-based (AddProvider wizard, ModelPicker) and reachable at any time via the `Ctrl+M` command menu. When the user submits a message from Home, the app switches to the **Session** route.

## Conventions

- **Edition 2024**, resolver v3, workspace-inherited package metadata (`version.workspace`, etc.).
- **Errors**: `color-eyre` in the binary; library crates should use typed errors (thiserror or a manual enum) rather than `eyre`.
- **Comments**: do not add comments unless explicitly requested (see project-wide convention).
- **Formatting/lint**: `cargo fmt` (defaults; `rustfmt.toml` is intentionally empty) and `cargo clippy --all-targets` must pass.
- **Commits**: follow the `Assisted-By:` trailer convention in `CONTRIBUTING.md` for AI-assisted commits. Do not commit unless explicitly asked.
- **Skills**: `.agents/skills/` contains `ratatui`, `tokio`, `turso-db`, `toasty`, and `rig` references — consult them when working on TUI rendering, async, storage, ORM, or LLM provider/agent code. The `ratatui` skill covers Ratatui 0.30.x and the Termina backend this project uses (instead of the default Crossterm backend). The `rig` skill covers Rig 0.41.x (the `shuvarie-llm` dependency) — provider clients, agents, tools, streaming, RAG, and memory; the root binary never calls `rig` directly.
- **Reuse**: for logics that are commonly used in multiple places, consider making them utils or in a shared module.

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Known issues to resolve

- `shuvarie-llm` implements model listing and streaming agent chat (`ProviderClient::stream` via `rig::streaming::StreamingChat`, M5/M7.1).
- `shuvarie-db` implements the Toasty models (`Session`, `Message`), the `Store` wrapper (open/apply embedded migrations, list/load/create/append/delete, most-recent query) and the `migrate` bin (toasty-cli) for managing `crates/db/toasty/` migration files (M6).
- `shuvarie-core` implements config load/save, `has_connected_providers()`, the `Command`/`Event` enums (including `CancelStream`, stream events, and `ToolStarted`/`ToolFinished`), the in-memory `Session` struct (with `id`/`title` tracking), the persistence hooks into `shuvarie-db` (session row created lazily on first message; user message persisted on send, assistant message + real usage on stream done; most-recent session loaded on startup), the core task (`run`) that orchestrates provider/model listing and streaming chat (M5, M6), and the six agent tools (`tools.rs`, M7.1: read/write/edit files, run commands, list dirs, grep) with a workspace-root escape guard.
- `shuvarie-llm` streaming runs a multi-turn agent loop (`max_turns` 20) with a fixed agent preamble; `StreamItem` carries `Delta`/`ToolStart`/`ToolResult`/`Done`/`Error`. A `Done` with empty text after tool calls means a tool-output-only turn. Tool errors surface as JSON error objects so the TUI marks them failed while the model still sees the message.
- Cost estimates in the sidebar Context panel use a built-in per-provider price table (`shuvarie-llm::pricing`); token counts are real `Usage` data. Provider-specific pricing/configurable rates land later.
- LSP and Skills sidebar panels show "inactive" placeholders (M7+ will add real LSP/Skills systems).
- Model search uses `nucleo` (fuzzy matcher) via `src/tui/search.rs`; the Ctrl+M command menu (`src/tui/command_menu.rs`) is a small extensible registry of `CommandEntry`s.

## Tips

### AGENTS.md

When changes are made, it's a good practice to check AGENTS.md if there is stale guidance and update it.

### ROADMAP.md

See `ROADMAP.md` for the milestone plan. Update it when milestones ship or scope changes.
