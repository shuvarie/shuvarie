# Rendering and Frames

## Immediate-mode rendering model

For each frame, the app renders **all** widgets into an intermediate `Buffer`. Ratatui diffs the new buffer against the previous one and flushes only the changed cells to the terminal. There is no retained widget tree, no auto-redraw, and no invalidation. The app's job is: read events → mutate model → draw the full UI → repeat.

```rust
terminal.draw(|frame| {
    // render the COMPLETE ui here every pass
})?;
```

- `Terminal::draw(closure) -> io::Result<CompletedFrame>` — panics become `Err` via the closure; runs the closure with a `Frame`, diffs, flushes.
- `Terminal::try_draw(closure) -> io::Result<CompletedFrame>` — same but the closure returns `io::Result<()>` so errors propagate instead of panicking. Prefer `try_draw` when rendering can fail (e.g. a widget that does I/O).
- After a successful `draw`/`try_draw`, the returned `CompletedFrame` describes the terminal state (buffer + area); it's only valid until the next successful draw.

## `Frame`

`Frame<'_>` is a consistent view of the terminal state for one render pass.

| Method | Purpose |
|--------|---------|
| `frame.area() -> Rect` | Full drawable area (after viewport/margins) |
| `frame.render_widget(widget, area)` | Render a `Widget` into `area` |
| `frame.render_stateful_widget(widget, area, &mut state)` | Render a `StatefulWidget` |
| `frame.set_cursor_position((x, y))` | Place the terminal cursor (for input fields) |
| `frame.set_widget_area(name, area)` / `frame.widget_area(name)` | Named area bookkeeping for hit-testing (0.30+) |
| `frame.buffer_mut() -> &mut Buffer` | Direct buffer access (escape hatch) |
| `frame.count() -> usize` | Frame counter (monotonic) |
| `frame.put_widget(area, widget)` | Place a widget without rendering immediately (deferred composition) |

A TEA `view(&self, frame: &mut Frame, area: Rect)` method takes `Frame` + `Rect` and may compose multiple widgets into `area` — it is **not** limited to a single widget's `render` contract. See `src/tui.rs::App::view`.

## `Buffer` / `Cell`

`Buffer` is a 2D grid of `Cell`s of a given `Rect`. Each `Cell` stores a `Grapheme` (the symbol), a `Style`, and a `GraphemeFocus` bit. Ratatui builds the buffer during `draw`, then diffs against the previous frame's buffer to emit only the deltas via the backend.

- `buf.area()` — the buffer's `Rect`.
- `buf.cell_mut((x, y))` — `Option<&mut Cell>`; clamped to the buffer.
- `buf[(x, y)]` — direct indexed access (panics out of range).
- `buf.set_string(x, y, string, style)` — write a string starting at `(x, y)`.
- `Cell::set_char(c)`, `cell.set_string(s)`, `cell.set_style(style)` — mutators.

Custom widgets write into `&mut Buffer` directly via the `Widget::render` contract.

## `Terminal`

`Terminal<B: Backend>` owns the backend, the current and previous buffers, and the viewport.

- `Terminal::new(backend) -> io::Result<Terminal<B>>`
- `Terminal::with_options(backend, TerminalOptions { viewport, .. })` — manual viewport (Inline/Fixed).
- `terminal.draw(|frame| ...)` / `terminal.try_draw(|frame| ...)`.
- `terminal.backend() -> &B` / `terminal.backend_mut() -> &mut B`.
- `terminal.resize(new_size)` — resize the internal buffers. Needed only for `Fixed` viewport; Fullscreen/Inline re-read the backend size on the next `draw`.
- `terminal.insert_before(n, |frame| ...)` — render content *before* the current viewport region, scrolling existing content up (requires `scrolling-regions` backend feature for smooth behavior; off in this project).

## `Viewport`

`Viewport` controls which part of the terminal Ratatui draws into.

| Variant | Behavior |
|---------|----------|
| `Viewport::Fullscreen` (default) | Full terminal screen; alt-screen-like. Ratatui's `init()`/`run()` use this. |
| `Viewport::Inline(height)` | Render `height` rows inline at the bottom of the scrollback; the terminal scrolls normally above. Good for prompt-style apps. |
| `Viewport::Fixed(rect)` | Draw into a fixed rectangle; does not follow resizes until you call `Terminal::resize`. |

Use `init_with_options(TerminalOptions { viewport, .. })` (Crossterm-only helper) or `Terminal::with_options(backend, options)` for non-default viewports. With Termina, construct via `Terminal::with_options(TerminaBackend::new(term), TerminalOptions { viewport, .. })`.

## What happens on edge cases

- **Resize**: Ratatui does not redraw automatically. On the next `draw`, Fullscreen/Inline viewports re-read the backend's current size; Fixed keeps its configured rect until `Terminal::resize` is called. Keep the event loop iterating so the resize event drives a redraw.
- **`try_draw` returns `Err`**: the render pass stops early; Ratatui does not guarantee the terminal, cursor, and internal buffers are still synchronized. Return the error and let the setup path restore terminal state on exit.
- **Direct cursor moves** (via backend or `Terminal` cursor methods): can be overwritten by the next render pass if that pass sets cursor state through `Frame`. Pick one path (prefer `frame.set_cursor_position`) and use it consistently.
- **Direct backend mutation**: bypasses Ratatui's diffing and viewport bookkeeping. After doing that, run a full `draw` pass or clear the terminal before assuming Ratatui's internal view still matches the screen.

## Unstable features

The project enables `unstable-backend-writer` only. Do **not** reach for the following without enabling their flags (they are off here):

| Feature | Enables |
|---------|---------|
| `unstable-rendered-line-info` | `Paragraph::line_count`, `Paragraph::line_width` (experimental; see ratatui#293) |
| `unstable-widget-ref` | `WidgetRef` / `StatefulWidgetRef` traits — render `&self` widgets by reference (needed by some container compositions) |
| `unstable-backend-writer` **(on here)** | `TerminaBackend::terminal()` / `terminal_mut()` accessors to the wrapped `termina::PlatformTerminal` |
| `unstable` | All of the above at once |

## `TerminalOptions`

```rust
use ratatui::{TerminalOptions, Viewport};
let opts = TerminalOptions { viewport: Viewport::Fullscreen, ..Default::default() };
let mut terminal = ratatui::Terminal::with_options(TerminaBackend::new(term), opts)?;
```

Fields: `viewport` (the only commonly-set one). Other fields control internal behavior; leave at defaults unless you have a specific reason.

## Rendering pattern for this project

```rust
// src/tui.rs (abridged)
let res: io::Result<()> = 'outer: loop {
    rat.draw(|frame| app.view(frame, frame.area()))?;   // full render each pass

    let term = rat.backend().terminal();
    loop {
        let event = term.read(|ev| !ev.is_escape())?;    // blocking read
        if let Some(msg) = App::handle_event(&event) {   // pure mapping
            if let Some(ret) = app.update(msg) {          // mutate model
                match ret {
                    AppReturn::Quit => break 'outer Ok(()),
                }
            }
            break;                                        // redraw after handling
        }
    }
};
```

Rules carried over from `AGENTS.md`:
- No side effects in `handle_event` or `view`.
- The TUI thread never `await`s; async LLM/DB work runs on a spawned core task via `tokio::sync::mpsc`.
- Render the full frame every pass; Ratatui's diffing handles efficiency.