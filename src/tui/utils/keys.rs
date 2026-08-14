use termina::event::{KeyEvent, Modifiers};

#[inline]
pub fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

#[inline]
pub fn alt(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::ALT)
}
