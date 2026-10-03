# AGENTS.md

## Workspace crates

- `./` (binary `shuvarie`): TUI.
- `./crates/core/` (`shuvarie-core`): Core module
- `./crates/config/` (`shuvarie-config`): config module (for KDL parser etc)
- `./crates/llm/` (`shuvarie-llm`): LLM module (Rig bridge)
- `./crates/db/` (`shuvarie-db`): DB module (migrations and Toasty ORM)
- `./crates/doc/` (`shuvarie-doc`): Document → GitHub-Flavored Markdown conversion (office docs, pdf, spreadsheets, epub, csv)
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

### Attachments & media

The attachment feature spans the stack around one currency type: `shuvarie-llm`'s `Attachment`
(metadata: kind/name/media_type/size/sha256) + `Blobs` (sha256 → bytes).

- `crates/doc/` (`shuvarie-doc`): pure (no process spawning) document → GFM-markdown
  conversion. `detect(bytes, ext)` (content signature first) + `to_markdown` + the
  `ConvertError` taxonomy (`NeedsOcr`/`Encrypted`/`Malformed`/`Unsupported`). Backends:
  delegated (`calamine`/`pdf-extract`/`rtf-parser`/`csv`) + hand-rolled zip+XML walkers
  (docx/pptx/odt/odp/epub).
- `crates/core/src/attachments.rs`: ingest (`resolve_directives` — @path read, sniff,
  image downscale/re-encode, document conversion) and request shaping (`prepare_for_send`
  — capability gate, text-only degradation, newest-first image budget), limits from the
  `attachments { … }` config via `AttachmentSettings`.
- `crates/core/src/office_convert.rs`: the optional external converter (`office-converter`
  config) for legacy `.doc`/`.ppt` — shared by the composer attachments and `read_document`.
- `crates/db/`: content-addressed `AttachmentBlob` store (sha256 → bytes, GC'd on delete
  cascades) + per-message `MessageAttachment` rows; session-file export/import carries base64 blobs.
- `src/tui/session/media.rs`: decode-once `MediaStore` (LRU-capped) + halfblock lifting
  through ratatui-image into ordinary `BodyChunk` lines; `blocks/media.rs` renders the
  per-turn attachment strip with hit regions; `viewer.rs` is the fullscreen gallery;
  `session/mention.rs` the compose-time preview/completion.

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
