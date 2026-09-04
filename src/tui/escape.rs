use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Keyboard, KittyKeyboardFlags, Mode, Window,
};
use termina::escape::osc::Osc;

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
