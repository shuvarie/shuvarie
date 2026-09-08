//! Helper functions for mapping Termina keys
//!
//! NOTE:
//!
//! "Match nice" in function rustdoc (priority when used in match: -20 highest (matched first), 20 lowest (matched last))

use termina::event::{KeyEvent, Modifiers};

/// Map ctrl key
///
/// Match nice: 0
#[inline]
pub fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

/// Map alt key
///
/// Match nice: 0
#[inline]
pub fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::ALT)
}

/// Map ctrl+alt key
///
/// Match nice: -1
#[allow(dead_code)]
#[inline]
pub fn ctrl_alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL | Modifiers::ALT)
}

/// Map ctrl+shift key
///
/// Match nice: -1
#[allow(dead_code)]
#[inline]
pub fn ctrl_shift(key: &KeyEvent) -> bool {
    key.modifiers
        .contains(Modifiers::CONTROL | Modifiers::SHIFT)
}

/// Map ctrl+alt+shift key
///
/// Match nice: -2
#[allow(dead_code)]
#[inline]
pub fn ctrl_alt_shift(key: &KeyEvent) -> bool {
    key.modifiers
        .contains(Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT)
}

/// Map alt+shift key
///
/// Match nice: -1
#[inline]
pub fn alt_shift(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::SHIFT | Modifiers::ALT)
}
