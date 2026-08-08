# Roadmap

Shuvarie is a terminal AI agent coding tool. This roadmap moves from the current scaffold (a minimal quit-on-q TUI) toward a usable streaming chat client, then to persistence, and finally to agent capabilities.

Each milestone is intended to be independently mergeable and to leave the binary building and the tests passing. Milestones are sequential; later ones depend on earlier ones.

## Current state

- Working TUI loop (ratatui + termina backend), TEA pattern, renders "Press q to quit", exits on `q`.
- `shuvarie-core` and `shuvarie-llm` wired as dependencies of the root binary, with real crate roots and typed error enums (`CoreError`, `LlmError` via `thiserror`).
- `shuvarie-llm` exposes a `Provider` enum (8 variants: OpenAi-compatible, OpenRouter, Groq, Together, DeepSeek, Anthropic, Gemini, Ollama) with metadata (display name, requires-api-key, default base URL), a `ProviderClient` that builds the right `rig` client per kind, and an async `list_models` returning a lightweight `ModelInfo` (rig types stay out of `shuvarie-core`).
- `shuvarie-core` defines the config schema (`Config`, `ProviderConfig`, `UiPrefs`), loads/saves `~/.config/shuvarie/config.toml` via the `dirs` crate (creates the dir on save, returns a default config when the file is missing), and exposes `has_connected_providers()`. `shuvarie-core` now depends on `shuvarie-llm`.
- `tokio` (`rt-multi-thread`, `macros`) in the root binary and `shuvarie-core`; `main` is `#[tokio::main]` (core task not yet spawned).
- `serde` + `toml` in `shuvarie-core`; `serde` in `shuvarie-llm` (for the `Provider` enum).
- No chat UI, model-selection UI, or persistence yet.

## M0 — Foundations

Wire the pieces together and remove boilerplate. No user-facing change.

- [x] Add `tokio` (with `rt-multi-thread`, `macros`) to the root binary and `shuvarie-core`.
- [x] Add `serde` + `toml` to `shuvarie-core` for config loading.
- [x] Decide: `shuvarie-llm` and `shuvarie-core` are kept separate for now (no shared types yet); revisit if shared types are needed.
- [x] Wire `shuvarie-core` and `shuvarie-llm` as dependencies of the root binary.
- [x] Replace the `add` boilerplate in both sub-crates with real (possibly near-empty) crate roots and module stubs.
- [x] Make `main` async (`#[tokio::main]`) without yet spawning the core task; keep the existing TUI loop working.
- [x] Reconcile `Cargo.toml` `license` field with the BSD-3-Clause `LICENSE` file.
- [x] Establish typed error enums in library crates (thiserror); keep `color-eyre` only in the binary.

## M1 — Config & provider registry

Provider abstraction and config persistence, with no UI yet.

- [x] In `shuvarie-llm`: define a `Provider` enum (`OpenAiCompatible`, `Anthropic`, `Gemini`, `Ollama`) with metadata (display name, requires API key, default base URL).
- [x] In `shuvarie-llm`: implement model listing per provider via `rig` (and a static fallback list for Ollama/offline).
- [x] In `shuvarie-core`: define the config file schema (`~/.config/shuvarie/config.toml`) — providers (with API key + base URL), active provider, active model, UI prefs.
- [x] In `shuvarie-core`: load/save config, create the config dir if missing, and expose `has_connected_providers() -> bool`.
- [x] Unit tests for config round-trip serialization and the "no providers" detection.

## M2 — Model selection UI

The launch gate: if no providers are connected, show this screen first.

- [ ] Add `AppMessage` variants for provider/model selection (e.g. `OpenModelSelect`, `ProviderAdded`, `ProviderRemoved`, `ModelSelected`, `ModelsLoaded`).
- [ ] Render a provider list with add/remove flows: choose provider kind, enter API key and base URL, fetch and list available models, pick the active model.
- [ ] Wire selection actions through `shuvarie-core` (which updates config) — the TUI never writes config directly.
- [ ] On launch: if `has_connected_providers()` is false, route to the model-selection screen; otherwise route to the chat view (placeholder for now).
- [ ] Make model selection reachable from the chat view via a keybinding (e.g. `Tab` or a `:` command menu stub).

## M3 — Tokio bridge & async core task

Stand up the channel plumbing that M4+ depends on, without yet doing LLM calls.

- [ ] Define command and event enums for TUI↔core communication.
- [ ] Spawn the core task in `main`; hand it a command receiver and an event sender.
- [ ] On the TUI thread, poll `termina`'s blocking `read` and drain the core→TUI channel each frame (non-blocking try_recv) so async results are folded into `handle_event`.
- [ ] Verify the bridge with a round-trip echo (e.g. TUI sends a ping, core echoes an event that the TUI renders).

## M4 — Basic chat UI

Message composition and rendering, with non-streaming send as a stepping stone.

- [ ] Add conversation state to `App`: input buffer, message list (user/assistant/system), active provider/model indicator.
- [ ] Render an input area (single-line for now, multi-line later) and a scrollable message list using ratatui widgets.
- [ ] `Enter` submits the input buffer as a `SendMessage` command; `Esc`/`q` behavior refined so `q` no longer quits while typing.
- [ ] On first pass, send via `rig` non-streaming and render the full assistant reply when it returns.
- [ ] Status bar showing active provider + model, plus a "thinking…" indicator while awaiting a reply.

## M5 — Streaming responses

Replace the non-streaming send with token streaming for live feedback.

- [ ] In `shuvarie-llm`: wrap `rig`'s streaming completion API and surface an async stream of token deltas + a completion/error event.
- [ ] In the core task: forward token deltas as `TokenReceived` events; emit `StreamDone` or `StreamError` at the end.
- [ ] In the TUI: append tokens to the in-flight assistant message and re-render incrementally; handle cancellation (e.g. `Ctrl+C` mid-stream) by sending a `CancelStream` command.

## M6 — Persistence (Turso + Toasty)

Persist chat sessions and message history so conversations survive restarts.

- [ ] Add `turso` (embedded SQLite) and `toasty` to `shuvarie-core`.
- [ ] Define Toasty models for `Session` and `Message` (role, content, provider, model, timestamps).
- [ ] On launch: load the most recent session into the chat view; on send/receive: persist messages.
- [ ] Session switcher UI (list of sessions, new session, delete session).
- [ ] Keep provider config in the TOML file — do not migrate secrets into the database.

## M7+ — Agent features (future, out of first-pass scope)

These are deliberately left undetailed; expand them into their own milestones when work begins.

- Tool calling (file read/write, shell execution, web fetch) via `rig` tool support.
- Multi-turn agent loops with tool-result feedback and a stop condition.
- Context files / project-aware prompts.
- RAG over chat history using Turso vector search and full-text search.
- Theming, command palette (`:` menu), keybinding configuration.
- Workspace/checkout integration, diff review, approval prompts for edits.
- Export/import sessions, multi-agent orchestration.

## Non-goals (for now)

- Being a general-purpose chat client unrelated to coding tasks.
- A GUI or web frontend.
- Hosting models ourselves (we integrate with providers via `rig`).