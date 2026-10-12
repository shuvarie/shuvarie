use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Keyboard, KittyKeyboardFlags, Mode, Window,
    XtermKeyModifierResource,
};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc, Selection};
use termina::escape::{OSC, ST};

pub const ENTER_ALTERNATE_SCREEN: Csi = Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ClearAndEnableAlternateScreen,
)));

pub const EXIT_ALTERNATE_SCREEN: Csi = Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ClearAndEnableAlternateScreen,
)));

// Enable the Kitty keyboard protocol so modified keys (e.g. Shift+Enter) carry
// their modifier bits instead of collapsing to the base key. We push the flags
// so they can be popped cleanly on exit. Terminals that don't support this
// protocol ignore the sequence, so we degrade gracefully — on such terminals
// Shift+Enter is indistinguishable from Enter and the newline binding is inert.
//
// `DISAMBIGUATE_ESCAPE_CODES | REPORT_ALTERNATE_KEYS` = bits 1 | 4 = 5.
pub const ENABLE_KITTY_KEYBOARD: Csi = Csi::Keyboard(Keyboard::PushFlags(
    KittyKeyboardFlags::from_bits_truncate(5),
));

pub const DISABLE_KITTY_KEYBOARD: Csi = Csi::Keyboard(Keyboard::PopFlags(1));

// Query the current kitty keyboard flags (`CSI ? u`). Kitty-protocol terminals
// (Ghostty, Kitty, iTerm2, WezTerm, ...) answer immediately; tmux does not
// implement the kitty protocol toward panes and stays silent, which is what
// the startup probe in `super` uses to detect it.
pub const QUERY_KITTY_FLAGS: Csi = Csi::Keyboard(Keyboard::QueryFlags);

// Query the cell size in pixels (`CSI 16 t`; xterm window ops). Answered by
// the kitty-graphics-family terminals (kitty, Ghostty, foot, iTerm2,
// WezTerm, Konsole) — the image renderers' metrics when the window ioctl
// report has no pixel dims. The report parses as
// `Window::ReportCellSizePixelsResponse`.
pub const QUERY_CELL_SIZE_PX: &str = "\x1b[16t";

// Fallback for terminals without the kitty keyboard protocol (notably tmux,
// which ignores both the flag push and `CSI ? u`): request xterm
// `modifyOtherKeys`. The request goes through termina's typed key-modifier
// resource command (`CSI > 4 ; Pv m`, resource 4 = "other keys") rather than a
// hand-written string, like every other escape here. tmux then tracks the
// request and starts forwarding modified keys toward the pane, which termina
// parses into modifier-carrying key events.
//
// The level matters. Level 1 only escapes keys that have no legacy encoding:
// chords whose control byte already exists keep it, and Ctrl+M is CR — exactly
// what Enter sends — so the Ctrl+M command-menu binding could never fire under
// tmux. Level 2 escapes every modified chord, sending Ctrl+M as CSI-u while
// plain Enter keeps its CR. tmux folds both levels into one "extended" mode
// before 3.5 (`input.c`: `m == 1 || m == 2`) and only distinguishes level 2
// from 3.5 on, where `extended-keys-format csi-u` makes it emit the CSI-u form
// termina understands (e.g. Ctrl+M as `CSI 109;5u`).
//
// Request level 2 only where that encoding holds. Elsewhere — and in tmux
// configured with `extended-keys-format xterm` — extended keys arrive as
// xterm's `CSI 27 ; mod ; code ~`, which termina's input parser drops, so
// level 2 would merely turn today's working legacy chords (Ctrl+C, Ctrl+J,
// ...) into sequences nothing can decode.
//
// No termina equivalent covers the *disable* verb: `Mode::XtermKeyMode`
// renders the `m` (set/reset value) and `? n` (query) forms, not xterm's
// `CSI > Pv n` "disable modifiers". `value: None` renders `CSI > 4 ; m`,
// which tmux accepts as a reset, but termina documents that same form as a
// *query*, so the explicit `n` verb stays hand-written below.
pub const REQUEST_MODIFY_OTHER_KEYS_LEVEL1: Csi = Csi::Mode(Mode::XtermKeyMode {
    resource: XtermKeyModifierResource::OtherKeys,
    value: Some(1),
});

pub const REQUEST_MODIFY_OTHER_KEYS_LEVEL2: Csi = Csi::Mode(Mode::XtermKeyMode {
    resource: XtermKeyModifierResource::OtherKeys,
    value: Some(2),
});

// Reset modifyOtherKeys (`CSI > 4n`) after a fallback request, mirroring the
// kitty flag pop on exit.
pub const RESET_MODIFY_OTHER_KEYS: &str = "\x1b[>4n";

// Enable button-event mouse tracking (mode 1002: press/release/drag) with SGR
// extended coordinates (mode 1006). Termina parses the reports into
// `Event::Mouse`; terminals that don't support the modes ignore the sequence
// and mouse input simply never arrives.
pub const ENABLE_MOUSE: Csi = Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ButtonEventMouse,
)));

pub const ENABLE_SGR_MOUSE: Csi = Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::SGRMouse,
)));

pub const DISABLE_MOUSE: Csi = Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::ButtonEventMouse,
)));

pub const DISABLE_SGR_MOUSE: Csi = Csi::Mode(Mode::ResetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::SGRMouse,
)));

// Bracketed paste (mode 2004): pastes arrive as a single `Event::Paste`
// payload instead of a burst of keystrokes, so line feeds stay text rather
// than triggering key bindings like Enter. Terminals without the mode ignore
// the sequence and keep the legacy keystroke behavior.
pub const ENABLE_BRACKETED_PASTE: Csi = Csi::Mode(Mode::SetDecPrivateMode(DecPrivateMode::Code(
    DecPrivateModeCode::BracketedPaste,
)));

pub const DISABLE_BRACKETED_PASTE: Csi = Csi::Mode(Mode::ResetDecPrivateMode(
    DecPrivateMode::Code(DecPrivateModeCode::BracketedPaste),
));

// OSC 9: show a desktop notification whose text is the payload. iTerm2
// introduced the form and most notification-capable terminals follow it —
// kitty and Ghostty (both of which also answer `OSC 99`), WezTerm, foot,
// Konsole, Windows Terminal — while terminals without it ignore the
// sequence. termina's `Osc` enum has no notification variant, so the escape
// is hand-written like the query constants above; the introducer and the
// string terminator (`ESC \`) are termina's own `OSC`/`ST`, the pair every
// `Osc` rendering is built from.
//
// The body is sanitized rather than escaped: a BEL or ESC inside it would
// terminate the sequence early or smuggle another control sequence into the
// stream, and an embedded newline would break the single-line toast, so
// control characters are dropped and line breaks collapse to spaces.
pub fn notify(text: &str) -> String {
    let mut body = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\n' | '\r' | '\t' => body.push(' '),
            ch if ch.is_control() => {}
            ch => body.push(ch),
        }
    }
    // ConEmu laid `9;4` (progress) and `9;9` (cwd) over OSC 9, so a terminal
    // that understands both reads a payload beginning with a digit as one of
    // those rather than as a toast. Everything we build here starts with the
    // app name, which makes this inert; the space keeps the sequence
    // unambiguous even if a future caller passes text that does begin so.
    if body.starts_with(|ch: char| ch.is_ascii_digit()) {
        body.insert(0, ' ');
    }
    format!("{OSC}9;{body}{ST}")
}

// Push/pop the window title on the title stack (CSI 22;2t / 23;2t) so the
// pre-existing tab title is restored on exit. Terminals without a title stack
// ignore these sequences.
pub fn push_window_title() -> Csi {
    Csi::Window(Box::new(Window::PushWindowTitle))
}

pub fn pop_window_title() -> Csi {
    Csi::Window(Box::new(Window::PopWindowTitle))
}

// OSC 2: set the window (tab) title. Terminals that don't render titles
// simply ignore the sequence.
pub fn set_window_title(title: &str) -> Osc<'_> {
    Osc::SetWindowTitle(title)
}

pub fn set_clipboard(text: &str) -> Osc<'_> {
    Osc::SetSelection(Selection::CLIPBOARD, text)
}

// OSC 11 query: asks the terminal for its current background color. Terminals
// that don't answer dynamic color queries simply never reply, so the caller
// must bound the wait and default to a sensible mode.
pub fn query_background_color() -> Osc<'static> {
    Osc::ChangeDynamicColors(
        DynamicColorNumber::TextBackgroundColor,
        vec![ColorOrQuery::Query],
    )
}
