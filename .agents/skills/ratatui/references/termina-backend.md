# Termina Backend for Ratatui

This project uses the **Termina** backend (`ratatui-termina` → `termina 0.3.3`) instead of the default Crossterm backend. Termina is a cross-platform VT (virtual terminal) manipulation library from the Helix editor project. It keeps the terminal protocol visible: applications write typed CSI/OSC/DCS escape values from `termina::escape` instead of assembling byte strings, and read typed `termina::event::Event` values instead of decoding input by hand.

`ratatui-termina::TerminaBackend` implements Ratatui's `Backend` trait by writing Termina CSI/SGR escape sequences. It wraps a caller-provided `termina::Terminal`, so the app can keep using Termina's event reader and typed terminal protocol surface alongside Ratatui rendering. The `termina` crate is re-exported as `ratatui_termina::termina`, so callers share the same Termina types as the backend.

**The backend does NOT enter raw mode, switch to the alternate screen, enable bracketed paste, or install cleanup.** The app must configure those terminal modes with Termina before creating the backend and restore them when the session ends.

## Crate layout

- `termina` 0.3.3 — VT manipulation: `PlatformTerminal`, `Terminal` (trait), `Event`, `Parser`, `EventReader`, `escape` (CSI/OSC/DCS), `style`.
- `ratatui-termina` 0.1.0 — `TerminaBackend` implementing `ratatui_core::backend::Backend`. Re-exports `termina`. Features: `underline-color` (default), `scrolling-regions`, `unstable-backend-writer` (wrapped-terminal accessors).
- The umbrella `ratatui` crate (0.30.2) re-exports `TerminaBackend` at `ratatui::backend::TerminaBackend` when the `termina` feature is on, and re-exports `termina` itself at `ratatui::termina`.

## Setup (manual — do not use `ratatui::init()`/`run()`)

`ratatui::init()`, `ratatui::restore()`, and `ratatui::run()` are convenience helpers gated on the **`crossterm`** feature. This repo disables default features and uses Termina, so those helpers are unavailable. Build the `Terminal` manually:

```rust
use std::io::{self, Write};
use ratatui::prelude::*;
use termina::{event::{KeyCode, KeyEvent}, Event, PlatformTerminal, Terminal};

pub fn run_tui() -> io::Result<()> {
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let mut rat = ratatui::Terminal::new(TerminaBackend::new(term))?;
    init_terminal(rat.backend_mut().terminal_mut())?;

    let res: io::Result<()> = 'outer: loop {
        rat.draw(|frame| render(frame))?;

        let term = rat.backend().terminal();
        loop {
            let event = term.read(|ev| !ev.is_escape())?;
            if matches!(event, Event::Key(KeyEvent { code: KeyCode::Char('q'), .. })) {
                break 'outer Ok(());
            }
        }
    };

    let deinit = deinit_terminal(rat.backend_mut().terminal_mut());
    res.and(deinit)
}
```

The real code lives in `src/tui.rs` and wraps this in The Elm Architecture (`handle_event` → `AppMessage` → `update` → `AppReturn`). Prefer that shape for new features.

## Raw mode + alternate screen

- `PlatformTerminal::enter_raw_mode()` enables raw input. Pair with `enter_cooked_mode()` (or rely on RAII) to restore.
- Alternate screen is entered/exited **manually** by writing a DEC private mode CSI (see `src/tui/escape.rs`). Do not switch to a backend-managed alt-screen toggle without reason.

```rust
use termina::escape::csi::{Csi, DecPrivateMode, DecPrivateModeCode, Mode};

pub const ENTER_ALTERNATE_SCREEN: Csi = Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ClearAndEnableAlternateScreen,
)));

pub const EXIT_ALTERNATE_SCREEN: Csi = Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ClearAndEnableAlternateScreen,
)));
```

Write these to the wrapped `PlatformTerminal` via `write!(terminal, "{}", CSI_CONST)?` then `terminal.flush()?` (the `Csi` type implements `Display`). See `src/tui.rs::init_terminal`/`deinit_terminal`.

## Backend access

`TerminaBackend` wraps the `PlatformTerminal`. To reach the underlying terminal:

- `rat.backend().terminal()` → `&PlatformTerminal` (for event reading, size queries).
- `rat.backend_mut().terminal_mut()` → `&mut PlatformTerminal` (for direct writes, flush, mode changes).

`terminal()` and `terminal_mut()` are gated on the **`unstable-backend-writer`** feature, which this project enables. Without it, you can only reach the backend through Ratatui's `Backend` trait methods.

## Event reading

Termina reads typed `Event` values. `Event` covers key/mouse/focus/resize/bracketed-paste **and** terminal protocol responses (CSI/OSC/DCS), so the app can issue a query and read the response from the same enum.

- `PlatformTerminal::event_reader()` → `EventReader`; call `reader.read(predicate)?`.
- `termina::Terminal::read(predicate)` (trait method on `PlatformTerminal`) — **blocking** read. The predicate filters events: pass `|ev| !ev.is_escape()` to skip escape-noise; pass `|_| true` to accept everything.
- For a non-blocking / async event source, Termina offers an `EventStream` (behind the `event-stream` feature, which uses `futures-core`); not enabled in this project. The TUI loop uses blocking `read` on the main thread and offloads async work to a spawned core task via `tokio::sync::mpsc`.

```rust
use termina::{event::{KeyCode, KeyEvent, KeyEventKind, Modifiers}, Event};

match event {
    Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
        KeyCode::Char('q') => /* quit */,
        KeyCode::Char('c') if key.modifiers.contains(Modifiers::CONTROL) => /* ctrl-c */,
        KeyCode::Enter => /* submit */,
        _ => {}
    },
    Event::Resize(_) => /* trigger a redraw on next loop iteration */,
    Event::Paste(text) => /* bracketed paste payload */,
    _ => {}
}
```

`KeyEvent` carries `code: KeyCode`, `modifiers: Modifiers`, `kind: KeyEventKind` (Press/Release/Repeat), and `state: KeyEventState`. `KeyCode` mirrors Crossterm's shape (`Char`, `Enter`, `Backspace`, `Left`/`Right`/`Up`/`Down`, `F(u8)`, `Tab`, `Esc`, ...).

## Other `termina` surfaces

- `termina::Parser` — incremental parser for terminal input bytes. Useful for PTY tests/multiplexers when you already own the input source and only need parsing, not a real terminal handle. `parser.parse(bytes, is_eof)` then `parser.pop()`.
- `termina::escape::csi` / `osc` / `dcs` — typed escape sequences. `Csi::Cursor`, `Csi::Mode`/`Mode::SetDecPrivateMode`, `Csi::Sgr`, etc. All implement `Display`.
- `termina::style` — cell styling types (used internally by the backend to emit SGR).
- `termina::OneBased` — one-based terminal coordinates (cursor positions).
- `termina::WindowSize` — terminal dimensions.

## Windows note

Termina speaks VT on Windows via ConPTY (requires 64-bit Windows 10.0.17763+). VT mode needs a terminal supporting the Kitty Keyboard Protocol for full key combinations (Windows Terminal Preview v1.25.622.0+). For terminals without it, enable the `windows-legacy` feature in `termina` to use the legacy console input reader (no Kitty protocol, no bracketed paste, but extended key events work). This project targets Linux/macOS primarily.

## Resize handling

Ratatui does not auto-redraw on resize. Termina emits `Event::Resize(...)`; on the next `terminal.draw(...)` call, Ratatui re-reads the backend's current size and lays out against the real dimensions. Just keep the event loop iterating:

```rust
Event::Resize(_) => {
    // no special action needed; the next rat.draw() will pick up the new size
    // for Fullscreen/Inline viewports. For Fixed viewport, call rat.resize(...).
}
```

For the `Fixed` viewport, call `Terminal::resize(new_area)` explicitly because it keeps its configured rectangle until you do.

## Cleanup discipline

- Always restore cooked mode and exit the alt screen, even on error. Use the `res.and(deinit_res)` pattern from `src/tui.rs` so a `draw`/`read` error still runs `deinit_terminal`.
- `try_draw` errors leave the terminal, cursor, and internal buffers potentially out of sync; return the error and let the surrounding setup path restore state on exit.
- Mutating the backend directly (bypassing Ratatui's diffing) desynchronizes the internal view; run a full `draw` pass or clear the terminal before relying on Ratatui's diff again.

## Feature flags (termina / ratatui-termina)

`termina`:
- `event-stream` (optional, off here) — async `EventStream` backed by `futures-core`.
- `windows-legacy` (Windows only) — legacy console input reader.

`ratatui-termina`:
- `underline-color` (default, on here) — SGR underline-color writes from cell underline colors.
- `scrolling-regions` (off here) — enables terminal scrolling regions for `Terminal::insert_before`.
- `unstable-backend-writer` (on here) — `backend().terminal()` / `backend_mut().terminal_mut()` accessors.