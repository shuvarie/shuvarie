# AGENTS.md

## Workspace crates

- `./` (binary `shuvarie`): TUI.
- `./crates/core/` (`shuvarie-core`): Core module
- `./crates/config/` (`shuvarie-config`): config module (for KDL parser etc)
- `./crates/llm/` (`shuvarie-llm`): LLM module (Rig bridge)
- `./crates/db/` (`shuvarie-db`): DB module (migrations and Toasty ORM)
- `./crates/highlight/` (`shuvarie-highlight`): Highlight module
- `./crates/lsp/` (`shuvarie-lsp`): LSP module
- `./crates/mcp/` (`shuvarie-mcp`): MCP module

## Architecture

### The Elm Architecture (TEA)

The TUI follows `map_event → message → update → return`.

The root `App` composes submodels (session screen, chat pane, overlays), each with its own message enum, `map_event`, `update`, and `view`; the parent dispatches events and forwards grouped messages.

- `map_event` — pure (`&self` only), no side effects; terminal events dispatch overlay-aware first, then route-aware.
- `update` — mutates the model and may return an effect; **all side effects happen here** (send core commands via `ctx.send(...)`; `UpdateCtx` carries `Connections` + the `Command` sender).
- `view` — draws state, `&self`, no side effects; may compose multiple widgets (a model need not implement `ratatui::Widget`).

## Rendering & UI

### Rendering stack

`ratatui` 0.30.x with the `termina` backend (not Crossterm).

## Build & test

```sh
cargo build
cargo test
cargo clippy --all-targets
cargo fmt --check
```

## Conventions

- Add an end newline when creating a new file
- When creating test cases, make sure they're reliably reproducible and able to represent typical use cases or edge cases. Update or delete stale test cases after updating the logic if necessary.
