use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Keyboard, KittyKeyboardFlags, Mode,
};

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
