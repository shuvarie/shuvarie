# AGENTS.md

## Workspace crates

- `./` (binary `shuvarie`): TUI.
- `./crates/core/` (`shuvarie-core`): Core module
- `./crates/config/` (`shuvarie-config`): config module (for KDL parser etc)
- `./crates/llm/` (`shuvarie-llm`): LLM module (Rig bridge)
- `./crates/db/` (`shuvarie-db`): DB module (migrations and Toasty ORM)
- `./crates/highlight/` (`shuvarie-highlight`): Highlight module
- `./crates/lsp/` (`shuvarie-lsp`): LSP module

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
