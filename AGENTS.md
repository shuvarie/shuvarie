# AGENTS.md

Shuvarie (シュヴァリエ, "chevalier") is a terminal-based AI agent coding tool built in Rust. This file orients AI agents (and humans) to the codebase, conventions, and intended architecture so that changes stay consistent.

## Workspace crates

- `./` (binary `shuvarie`) — TUI rendering, event loop, input handling, channel wiring. **No business logic lives here.** Renders state produced by `shuvarie-core`.
- `./crates/core/` (`shuvarie-core`) — App state, config loading, storage layer (Turso + Toasty ORM), and the async core task that orchestrates LLM and database work. Owns the domain `Message`/`Update` logic.
- `./crates/llm/` (`shuvarie-llm`) — Thin wrapper over `rig`: `Provider` enum, model listing, streaming completion API, message types. No TUI concerns.

When adding a feature: put domain logic in `shuvarie-core`, LLM/SDK glue in `shuvarie-llm`, and only rendering + input dispatch in the root binary.

## Architecture

### The Elm Architecture (TEA)

The TUI follows `handle_event → message → update → return`:

- `handle_event` (pure): maps a terminal `Event` (or an async channel message) to a message (e.g. `AppMessage`).
- `update` (mutates the model): consumes a message and may produce a return (e.g. `AppReturn::Quit`).
- `view(&self, frame: &mut Frame<'_>, area: Rect)`: draws current state. A TEA model is **not** required to implement `ratatui::Widget`; the dedicated `view` method takes a `Frame` + `Rect` and may compose multiple widgets, so it is not limited to a single widget's render contract.

Extend features by adding a message variant (and a return if conditional parent actions or feedbacks are necessary), an `update` arm, and `view` logic — do not introduce side effects in `handle_event` or `view`.

### Async runtime: Tokio

`main` is `#[tokio::main]`. The TUI loop runs on the main thread using `termina`'s blocking `read`. All LLM and database work runs on a spawned **core task**. Communication is via `tokio::sync::mpsc` channels:

- TUI → core: commands (e.g. `SendMessage`, `SelectModel`, `LoadHistory`).
- Core → TUI: events (e.g. `TokenReceived`, `StreamDone`, `ModelsLoaded`, `HistoryLoaded`) that are converted into `AppMessage` variants and fed into `update`.

Keep the TUI thread free of `await`s; offload any blocking work to the core task (or `spawn_blocking`).

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

### Launch flow

On startup `shuvarie-core` loads config and checks for connected providers. If none are configured, the TUI opens to the **model selection / connect provider** screen first; otherwise it opens to the chat view. Model selection is also reachable from a menu at any time.

## Conventions

- **Edition 2024**, resolver v3, workspace-inherited package metadata (`version.workspace`, etc.).
- **Errors**: `color-eyre` in the binary; library crates should use typed errors (thiserror or a manual enum) rather than `eyre`.
- **Comments**: do not add comments unless explicitly requested (see project-wide convention).
- **Formatting/lint**: `cargo fmt` (defaults; `rustfmt.toml` is intentionally empty) and `cargo clippy --all-targets` must pass.
- **Commits**: follow the `Assisted-By:` trailer convention in `CONTRIBUTING.md` for AI-assisted commits. Do not commit unless explicitly asked.
- **Skills**: `.agents/skills/` contains `ratatui`, `tokio`, `turso-db`, and `toasty` references — consult them when working on TUI rendering, async, storage, or ORM code. The `ratatui` skill covers Ratatui 0.30.x and the Termina backend this project uses (instead of the default Crossterm backend).

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Known issues to resolve

- `Cargo.toml` declares `license = "MIT"` but `LICENSE` is BSD-3-Clause — reconcile before publishing.
- `crates/core` and `crates/llm` still contain the default `cargo new` `add` boilerplate; replace with real crate roots when implementing their milestones.
- The root binary does not yet depend on `shuvarie-core` or `shuvarie-llm` — wire them in during M0.

## Tips

### AGENTS.md

When changes are made, it's a good practice to check AGENTS.md if there is stale guidance and update it.

### ROADMAP.md

See `ROADMAP.md` for the milestone plan. Update it when milestones ship or scope changes.
