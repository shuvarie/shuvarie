# Roadmap

Shuvarie is a terminal AI agent coding tool. This roadmap moves from the current scaffold toward a usable streaming chat client, then to persistence, and finally to agent capabilities.

Each milestone is intended to be independently mergeable and to leave the binary building and the tests passing. Milestones are sequential; later ones depend on earlier ones.

## Current state

- Working TUI loop (ratatui + termina backend), TEA pattern with hierarchical submodels (`HomeScreen`, `SessionScreen`, overlays).
- `shuvarie-core`, `shuvarie-db`, `shuvarie-llm`, `shuvarie-catalog`, and `shuvarie-highlight` wired as dependencies of the root binary, with real crate roots and typed error enums (`CoreError`, `DbError`, `LlmError` via `thiserror`).
- `shuvarie-db` provides the Toasty/Turso persistence layer: `Session`/`Message` models, a `Store` wrapper (`.shuvarie/data.db` in the working directory), embedded migrations (see `crates/db/toasty/`), and a `migrate` bin (toasty-cli) for managing them.
- `shuvarie-catalog` is the uniform source of provider + model info: the `Provider` enum (9 variants) with metadata (display name, requires-api-key, default base URL), `ModelInfo`, `TokenUsage`, and embedded data files (`data/models.toml`: unified `[models.*]` stats — context length + per-mtok input/output rates per canonical `<org>/<model>` id, orgs matching Hugging Face; `data/providers/*.toml`: fallback rates plus `aliases` from provider-specific ids to canonical ids and `variants` — kind-labeled deployment tiers like `pro`/`thinking` with optional override rates/context), with `resolve`/`estimate_cost`/`enrich`/`variants` (exact then longest-prefix alias matching, provider-specific overrides win, provider-default fallback for unknown ids, `enrich` fills missing runtime context lengths).
- `shuvarie-llm` exposes a `ProviderClient` that builds the right `rig` client per kind, an async `list_models` returning a lightweight `ModelInfo`, and a streaming `stream` (via `rig::streaming::StreamingChat`) yielding a portable `StreamItem` stream (`Delta`/`ToolStart`/`ToolResult`/`Done`/`Error`). Rig types stay out of `shuvarie-core`; `ChatMsg`/`Role` are the portable message types, and rig→catalog conversions (`model_info_from_rig`, `token_usage_from_rig`) live here.
- `shuvarie-core` defines the config schema (`Config`, `ProviderConfig`, `UiPrefs`), loads/saves `~/.config/shuvarie/config.toml` via the `dirs` crate, and exposes `has_connected_providers()`. An in-memory `Session` struct holds messages + token/cost totals (input/output/reasoning/cache + cost, accumulated from real `Usage` on each completed reply) plus persisted `id`/`title` tracking.
- `tokio` (`rt-multi-thread`, `macros`, `sync`) in the root binary and `shuvarie-core`; `main` is `#[tokio::main]` and spawns the core task.
- The core task (`shuvarie_core::run`) owns the in-memory `Config` and active `Session`, handles `Command`s (ping, list/add/remove/set-active providers, set active model, save config, start session, send message, cancel stream), and emits `Event`s (pong, models loaded/error, config saved/error, session started, token received, stream done/error/cancelled, usage update). LLM streams run on a spawned task inside the core task, so the loop stays responsive to `CancelStream` (abort) while tokens flow. The TUI and core communicate via `tokio::sync::mpsc` channels.
- Two-route TUI: **Home** (new-session empty state with centered ASCII art + input) and **Session** (left sidebar + content with centered title bar + transparent chat history + input). No global title bar; branding lives in the sidebar and Home logo. Overlays float over either route: Welcome (first-run), AddProvider (two-stage wizard: kind list then details), ModelPicker (fuzzy search), CommandMenu (Ctrl+M), ConfirmQuit (Ctrl+C).
- Provider management is overlay-based (no dedicated route). Adding a provider shows a kind list first, then a details form with auto-populated default name (suffix-numbered if taken) and default base URL. Model auto-selection on `ModelsLoaded`: keeps last-used model if present, else picks the first.
- Responses stream token-by-token into the in-flight assistant message (status row "streaming…"); `Ctrl+C` mid-stream aborts and discards the partial reply (`Ctrl+C` while idle opens the confirm-quit dialog). Session Context panel in the sidebar shows accumulated input/output/reasoning/cache tokens and an estimated cost (per-model rates from the `shuvarie-catalog` embedded table, provider-default fallback for unknown ids).
- Model listing is enriched from `shuvarie-catalog` (`enrich` fills missing runtime context lengths), so the ModelPicker shows a context label and the sidebar's used-context percentage works with real data.
- Sessions and message history persist to `.shuvarie/data.db` (Turso embedded SQLite via Toasty, in the working directory). On launch the most recent session loads into the chat view; the `Ctrl+M` command menu offers "Switch session" (list/resume/`Ctrl+D`-to-delete with a second `Ctrl+D` to confirm) and "New session". Schema changes are managed with toasty migrations (see `crates/db/toasty/` and the `migrate` bin).
- Input areas feature a virtual cursor (reversed block at cursor position), Emacs keybindings (Ctrl+B/F/A/E/D/H/K, Alt+B/F, Ctrl+N/P), and content-width-sized centered text in overlays.
- Quit is via `Ctrl+C` → confirm dialog (Enter or Ctrl+C again to confirm, Esc to cancel); `q` no longer quits.
- Agent tool calling (M7.1): `shuvarie-llm` exposes a portable `Tool` trait; `shuvarie-core` implements six tools (read/write/edit files, run commands, list dirs, grep) with a workspace-root escape guard; the streaming agent loop runs up to 20 turns, and tool calls/results render inline in the chat pane (`›` running, `✓` ok, `✗` error). Tool-call-only replies surface a "(tool output only)" placeholder instead of an empty message.
- Markdown + code highlighting: new `shuvarie-highlight` crate renders user/system/assistant message bodies through pulldown-cmark (bold/italic/strikethrough, inline code, headings, lists, quotes, tables, links) with fenced code blocks highlighted via syntect (pure-rust regex-fancy, programmatic heraldic theme) and a custom ` ```diff ` renderer (`+`/`-`/`@@` tinctures); code-block lines get a subtle surface background. Re-parses the streaming pending text each token.

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

The launch gate: if no providers are connected, show the Welcome overlay first.

- [x] Add `AppMessage` variants for provider/model selection (e.g. `OpenModelSelect`, `ProviderAdded`, `ProviderRemoved`, `ModelSelected`, `ModelsLoaded`).
- [x] Render a provider list with add/remove flows: choose provider kind, enter API key and base URL, fetch and list available models, pick the active model.
- [x] Wire selection actions through `shuvarie-core` (which updates config) — the TUI never writes config directly.
- [x] On launch: if `has_connected_providers()` is false, show the Home route with a Welcome overlay; otherwise route to Home.
- [x] Make model selection reachable via the Ctrl+M command menu → "Select model" → ModelPicker overlay.
- [x] Model search: nucleo fuzzy filter over the model list in the ModelPicker overlay.
- [x] Ctrl+M command menu overlay (originally Ctrl+P; moved to Ctrl+M for Emacs keybinds) with searchable command list.

## M3 — Tokio bridge & async core task

Stand up the channel plumbing that M4+ depends on, without yet doing LLM calls.

- [x] Define command and event enums for TUI↔core communication (`shuvarie_core::Command`, `shuvarie_core::Event`).
- [x] Spawn the core task in `main`; hand it a command receiver and an event sender.
- [x] On the TUI thread, use termina's `event-stream` feature with `tokio::select!` between `EventStream::next()` and `event_rx.recv()` so the loop wakes on either a key event or a core event.
- [x] Verify the bridge with a round-trip echo (e.g. TUI sends a ping, core echoes an event that the TUI renders).

## M4 — Basic chat UI

Message composition and rendering, with non-streaming send. Two-route layout (Home + Session), left sidebar, overlay-based provider management, Emacs keybindings, virtual cursor, and Ctrl+C confirm-quit.

- [x] Add conversation state: in-memory `Session` in core, mirrored message list in `SessionScreen`, input buffer with virtual cursor.
- [x] Home route: centered ASCII art (double sword behind shield) + centered input area; `Enter` starts a session and switches to Session route.
- [x] Session route: left sidebar (Shuvarie + version, Context: tokens + cost, LSP/Skills inactive placeholders) + content pane with centered title bar atop transparent chat history + input + status row.
- [x] `Enter` submits the input buffer as a `SendMessage` command; core calls `ProviderClient::complete` (non-streaming via `rig::completion::Chat`) and emits `MessageReceived`/`ReplyError`.
- [x] Status row showing "thinking…" while awaiting a reply; error display on `ReplyError`.
- [x] Provider management overhaul: two-stage AddProvider wizard (kind list → details form with auto-named default + default base URL); ModelPicker overlay with fuzzy search; auto-select last-used-or-first model on `ModelsLoaded`.
- [x] Welcome overlay on first launch (no providers) → Enter opens AddProvider wizard; auto-dismisses when a provider connects.
- [x] Emacs keybindings on all text inputs (Ctrl+B/F/A/E/D/H/K, Alt+B/F) and list navigations (Ctrl+N/P); virtual cursor (reversed block) in all input areas.
- [x] Quit via `Ctrl+C` → confirm dialog (Enter/Ctrl+C to confirm, Esc to cancel); `q` no longer quits. Command menu moved to `Ctrl+M`.
- [x] Remove the dedicated model-selection route; all provider/model management is overlay-based.

## M5 — Streaming responses

Replace the non-streaming send with token streaming for live feedback.

- [x] In `shuvarie-llm`: wrap `rig`'s streaming completion API (`StreamingPrompt`/`StreamingChat`) and surface an async stream of token deltas + a completion/error event.
- [x] In the core task: forward token deltas as `TokenReceived` events; emit `StreamDone` or `StreamError` at the end. Replace the `complete` call in the `SendMessage` handler with the streaming path.
- [x] In the TUI: append tokens to the in-flight assistant message and re-render incrementally; handle cancellation (`Ctrl+C` mid-stream — reuse the confirm-quit dialog or a dedicated interrupt) by sending a `CancelStream` command.
- [x] Wire real token usage and cost into the sidebar Context panel (input/output/reasoning/cache tokens from `Usage`; per-session cost aggregation).

## M6 — Persistence (Turso + Toasty)

Persist chat sessions and message history so conversations survive restarts.

- [x] Add `shuvarie-db` (new crate) with `turso` (embedded SQLite via `toasty-driver-turso`) and `toasty`.
- [x] Define Toasty models for `Session` and `Message` (role, content, provider, model, usage/cost, timestamps), migrations managed by the toasty-cli `migrate` bin and embedded via `embed_migrations!`.
- [x] On launch: load the most recent session into the chat view; on send/receive: persist messages.
- [x] Session switcher UI (list of sessions, new session, delete session) — `Ctrl+M` → "Switch session", plus a "New session" command.
- [x] Keep provider config in the TOML file — do not migrate secrets into the database.

## M7+ — Agent features

Milestone M7.1 ships; the M7.2 catalog milestone ships; the rest remain future work.

- [x] M7.1 — Tool calling (file read/write/edit, shell execution, list, grep) via a portable `Tool` trait over rig's `DynamicTool`, with a multi-turn agent loop (`max_turns` 20), an agent preamble, inline tool-activity rendering in the chat pane, and a workspace-root escape guard on all tools. Deferred follow-ups: tool-output token counting, configurable turn budget, approval prompts for edits/commands (see below).
- [x] M7.2 — Uniform provider/model catalog: the `shuvarie-catalog` crate centralizes `Provider`/`ModelInfo`/`TokenUsage`, split embedded data (`data/models.toml` — canonical `<org>/<model>` stats; `data/providers/*.toml` — fallback rates, `aliases`, and kind-labeled `variants` deployment tiers), and `resolve`/`estimate_cost`/`enrich`/`variants` (exact then longest-prefix alias matching, provider-specific overrides win, provider-default fallback for unknown ids). Deferred follow-up: user-configurable rate overrides, and surfacing deployment-tier variants in the ModelPicker.
- Multi-agent orchestration and task decomposition.
- Context files / project-aware prompts (AGENTS.md scanning, `.shuvarie/context`).
- RAG over chat history using Turso vector search and full-text search.
- LSP integration (real LSP servers in the sidebar instead of inactive placeholders).
- Skills system (real skills panel instead of inactive placeholder).
- Theming, keybinding configuration, and expanding the command palette.
- Workspace/checkout integration, diff review, approval prompts for edits.
- Export/import sessions.

## Non-goals (for now)

- Being a general-purpose chat client unrelated to coding tasks.
- A GUI or web frontend.
- Hosting models ourselves (we integrate with providers via `rig`).