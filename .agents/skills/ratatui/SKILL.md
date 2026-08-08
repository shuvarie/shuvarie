---
name: ratatui
description: >-
  You are an expert in Ratatui, the Rust library for cooking up terminal user
  interfaces via immediate-mode rendering into an intermediate Buffer. You help
  developers build TUI apps using widgets, Layout, styling (Text/Line/Span,
  Style/Stylize), the Frame/Terminal draw contract, and the pluggable backend
  system. This skill covers Ratatui 0.30.x (the workspace split: ratatui-core,
  ratatui-widgets, and per-backend crates) and specifically the Termina backend
  (ratatui-termina / termina 0.3.x) which this project uses instead of the default
  Crossterm backend. Docs: https://docs.rs/ratatui/latest/ratatui/ and
  https://docs.rs/termina/latest/termina/.
license: MIT
metadata:
  author: shuvarie
  version: 0.30.2
  category: Frontend Development
  tags:
    - rust
    - tui
    - ratatui
    - termina
    - terminal
    - rendering
    - widgets
---

# Ratatui — Terminal User Interfaces for Rust

Ratatui is a Rust library for building terminal user interfaces. It uses **immediate rendering with intermediate buffers**: for each frame, the app renders all widgets into a `Buffer`, Ratatui diffs against the previous frame, and only the changed cells are flushed to the terminal. This is the opposite of retained-mode GUIs — there is no widget tree that auto-redraws.

This project (`shuvarie`) pins **Ratatui 0.30.2** with `default-features = false` and the **`termina`** backend (`ratatui-termina` → `termina 0.3.3`) rather than the default Crossterm backend. Always reach for the Termina backend patterns in this skill, not the Crossterm examples in upstream docs.

## Project Cargo setup (reference)

```toml
[dependencies]
termina = "0.3.3"

[dependencies.ratatui]
version = "0.30.2"
default-features = false
features = [
    "all-widgets",
    "layout-cache",
    "macros",
    "std",
    "termina",              # NOT "crossterm"
    "underline-color",
    "widget-calendar",
    "unstable-backend-writer",
]
```

Crate workspace (since 0.30.0): the `ratatui` crate re-exports everything apps need; `ratatui-core` (traits/types for widget-library authors), `ratatui-widgets` (built-in widgets), `ratatui-macros`, and the backend crates `ratatui-crossterm` / `ratatui-termion` / `ratatui-termina` / `ratatui-termwiz`. Prefer the umbrella `ratatui` crate unless you are writing a reusable widget library.

## Critical Rules

Before writing Ratatui code in this repo, know these constraints:

- **No side effects in `handle_event` or `view`.** This project follows The Elm Architecture: `handle_event` maps a terminal `Event` to an `AppMessage` (pure), `update` mutates the model (may return `AppReturn`), and `view(&self, frame, area)` only renders. See `AGENTS.md`.
- **TUI thread must not `await`.** `main` is `#[tokio::main]`; the TUI loop runs on the main thread using Termina's **blocking** `read`. Offload LLM/DB work to the spawned core task via `tokio::sync::mpsc`. Do not call async code from the render/event loop.
- **Use the Termina backend, not Crossterm.** Ratatui's default examples use `crossterm::event` and `ratatui::init()`/`run()`. Those convenience functions are `crossterm`-feature-gated and are **not available** here. Construct `ratatui::Terminal::new(TerminaBackend::new(term))` manually and manage alt-screen enter/exit yourself (see `src/tui.rs`, `src/tui/escape.rs`).
- **Alt-screen is entered/exited manually via CSI escapes** in `src/tui/escape.rs`. Do not switch to a backend-managed alt-screen toggle without reason. Ratatui's `init`/`restore`/`run` helpers would conflict with the manual Termina setup.
- **A TEA model is not required to implement `Widget`.** A `view(&self, frame: &mut Frame<'_>, area: Rect)` method may compose multiple widgets into `area`; don't force single-widget `render` methods.
- **Render the whole frame each pass.** Ratatui diffs buffers, so partial redraws are unnecessary and harmful — always render the complete UI inside `terminal.draw(|frame| ...)`.
- **Don't block the backend between draws.** Long work stalls the terminal; channel it to the core task and let the TUI loop keep drawing.
- **`underline-color` is backend-specific** — supported by Crossterm, Termina, and Termwiz; not on Windows 7. It's enabled in this project and safe with Termina.

## Quickstart (Termina backend, manual setup)

```rust
use std::io::{self, Write};

use ratatui::prelude::*;
use termina::{
    event::{KeyCode, KeyEvent},
    Event, PlatformTerminal, Terminal,
};

fn run_tui() -> io::Result<()> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    init_terminal(rat.backend_mut().terminal_mut())?;

    let res: io::Result<()> = 'outer: loop {
        rat.draw(|frame| render(frame))?;

        let term = rat.backend().terminal();
        loop {
            let event = term.read(|ev| !ev.is_escape())?;
            if should_quit(&event) {
                break 'outer Ok(());
            }
        }
    };

    deinit_terminal(rat.backend_mut().terminal_mut())?;
    res
}

fn render(frame: &mut Frame) {
    frame.render_widget("Hello, Ratatui!", frame.area());
}

fn should_quit(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })
    )
}
```

The actual project structure (`src/tui.rs`) wraps this in The Elm Architecture (`App::handle_event` → `AppMessage` → `App::update` → `AppReturn`). Prefer that shape for new code.

## Feature Decision Tree

Use this to decide which reference file to load:

**Need the Termina backend, alt-screen escapes, raw mode, event reading, or how to wire Termina into `ratatui::Terminal`?**
→ Read `references/termina-backend.md`

**Need the widget catalog (`Block`, `Paragraph`, `List`, `Table`, `Gauge`, `Tabs`, `Block`, borders, etc.), `Layout`, `Constraint`, `Rect`, or nested layouts?**
→ Read `references/widgets-and-layout.md`

**Need `Text`/`Line`/`Span`, `Style`, `Stylize`, `Color`, `Modifier`, or the shorthand styling syntax (`"x".red().bold()`)?**
→ Read `references/styling-text.md`

**Need `Frame`, `Terminal::draw` / `try_draw`, `Buffer`/`Cell`, `Viewport` (Fullscreen/Inline/Fixed), `CompletedFrame`, or the `unstable-*` features?**
→ Read `references/rendering-and-frames.md`

**Need `ratatui-macros` (`line!`, `span!`, `text!`, `layout!`, `rect!`, `buffer!`) for terser construction?**
→ Read `references/macros.md`

## API Surface (Cheat Sheet)

| Item | Path | Purpose |
|------|------|---------|
| `Frame` | `ratatui::Frame` | Consistent view of terminal state for one render pass; `frame.render_widget(w, area)`, `frame.area()`, `frame.render_stateful_widget(...)` |
| `Terminal` | `ratatui::Terminal` | Owns the backend + buffers; `draw(\|frame\| ...)`, `try_draw(...)`, `backend()`, `backend_mut()`, `resize(...)` |
| `Backend` (trait) | `ratatui::backend::Backend` | Pluggable backend contract; this project uses `TerminaBackend` |
| `TerminaBackend` | `ratatui::backend::TerminaBackend` | Bridges `termina::PlatformTerminal` to Ratatui's `Backend` trait |
| `Viewport` | `ratatui::Viewport` | `Fullscreen` (default), `Inline(height)`, `Fixed(rect)` |
| `Buffer` / `Cell` | `ratatui::buffer::{Buffer, Cell}` | The intermediate grid Ratatui diffs each frame |
| `Layout` / `Constraint` | `ratatui::layout::{Layout, Constraint}` | Split a `Rect` into sub-areas; `Layout::vertical(...).areas(rect)` returns `[Rect; N]` |
| `Rect` | `ratatui::layout::Rect` | A rectangular area (`x`, `y`, `width`, `height`) |
| `Text` / `Line` / `Span` | `ratatui::text::{Text, Line, Span}` | Styled text building blocks |
| `Style` / `Color` / `Modifier` / `Stylize` | `ratatui::style::{Style, Color, Modifier, Stylize}` | Styling; `Stylize` enables `"x".red().bold()` shorthand |
| `Widget` (trait) | `ratatui::widgets::Widget` | Implement `render(self, area, buf)` to make a custom widget |
| `Block` | `ratatui::widgets::Block` | Bordered/titled container; `.bordered().title(...)` |
| `Paragraph` | `ratatui::widgets::Paragraph` | Multi-line text with alignment + wrapping |
| `List` / `ListState` | `ratatui::widgets::{List, ListState}` | Selectable list; stateful widget |
| `Table` / `Row` / `Cell` | `ratatui::widgets::{Table, Row, Cell}` | Tabular data |
| `Gauge` / `BarChart` | `ratatui::widgets::{Gauge, BarChart}` | Progress / bars |
| `Tabs` | `ratatui::widgets::Tabs` | Tab strip |
| `prelude` | `ratatui::prelude` | One glob import for common items |
| `macros` | `ratatui::macros` (via `ratatui-macros`) | `line!`, `span!`, `text!`, `layout!`, `rect!`, `buffer!` |

## Conventions for This Repo

- **Import shape**: `use ratatui::prelude::*;` for the common types, plus explicit `use ratatui::widgets::{...};` for specific widgets. See `src/tui.rs`.
- **Backend access**: `rat.backend_mut().terminal_mut()` gives `&mut termina::PlatformTerminal` for direct writes (e.g. alt-screen CSI). `rat.backend().terminal()` gives `&PlatformTerminal` for event reading.
- **Event filtering**: `term.read(|ev| !ev.is_escape())` blocks until a non-escape event arrives — use the predicate to filter noise.
- **`color-eyre` in the binary**, typed errors (thiserror-style) in library crates. Never bubble `eyre` out of `shuvarie-core` / `shuvarie-llm`.
- **No comments** unless explicitly requested (project-wide convention; `cargo fmt` defaults; empty `rustfmt.toml`).
- **Lint gate**: `cargo fmt --check` and `cargo clippy --all-targets` must pass before a change is considered done.

## Common Pitfalls

1. **Using `ratatui::init()`/`run()`** — these require the `crossterm` feature and pull in Crossterm. This repo disables default features and uses Termina. Build the `Terminal` manually.
2. **Calling `event::read()` from crossterm** — there is no crossterm dependency; use `termina::PlatformTerminal::event_reader()` / `Terminal::read(predicate)`.
3. **Forgetting to restore the terminal** — wrap `run_app` so `deinit_terminal` runs even on error (see the `res.and(deinit_res)` pattern in `src/tui.rs`).
4. **Awaiting inside the TUI loop** — blocks rendering; route async work to the core task over mpsc channels.
5. **Mutating the backend directly between draws** — bypasses Ratatui's diffing; run a full draw or clear the terminal afterward.
6. **Mixing cursor APIs** — cursor changes via the backend can be overwritten by the next `Frame` render; pick one path and use it consistently.
7. **`Paragraph::line_count` / `line_width` / `WidgetRef`** — these are behind `unstable-rendered-line-info` and `unstable-widget-ref`; the project enables `unstable-backend-writer` but **not** these. Don't reach for them without enabling the flag.
8. **Assuming automatic redraw on resize** — Ratatui re-reads the backend size on the next `draw`; just keep the event loop running and call `draw` again.

## External Resources

- **Ratatui website** (concepts, tutorials, recipes): https://ratatui.rs/
- **Ratatui API docs**: https://docs.rs/ratatui/latest/ratatui/
- **Examples**: https://github.com/ratatui/ratatui/tree/main/ratatui/examples/README.md
- **Breaking changes**: https://github.com/ratatui/ratatui/blob/main/BREAKING-CHANGES.md
- **Termina crate**: https://docs.rs/termina/latest/termina/ and https://github.com/helix-editor/termina
- **ratatui-termina** (the backend glue): https://docs.rs/ratatui-termina/latest/ratatui_termina/
- **Forum**: https://forum.ratatui.rs · **Discord**: https://discord.gg/pMCEU9hNEj

## Complete File Index

| File | Description |
|------|-------------|
| `SKILL.md` | Main entry point — quickstart, decision tree, cheat sheet, repo conventions |
| `references/termina-backend.md` | Termina backend wiring: `PlatformTerminal`, `TerminaBackend`, raw mode, manual alt-screen CSI via `termina::escape`, blocking event reading, resize handling, `unstable-backend-writer` |
| `references/widgets-and-layout.md` | Widget catalog (`Block`, `Paragraph`, `List`/`ListState`, `Table`, `Gauge`, `BarChart`, `Tabs`, `Sparkline`, `Canvas`, `Clear`), `Layout`/`Constraint`/`Rect`, nested layouts, stateful widgets |
| `references/styling-text.md` | `Text`/`Line`/`Span`, `Style`/`Color`/`Modifier`, `Stylize` shorthand, alignment, wrapping, scroll/offset on `Paragraph` |
| `references/rendering-and-frames.md` | `Frame`, `Terminal::draw`/`try_draw`, `Buffer`/`Cell`, `Viewport` (Fullscreen/Inline/Fixed), `CompletedFrame`, `resize`, diffing semantics, unstable features |
| `references/macros.md` | `ratatui-macros`: `line!`, `span!`, `text!`, `layout!`, `rect!`, `buffer!`, `border!` |