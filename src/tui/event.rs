/// Enumeration of all possible events that the Shuvarie TUI might be handling.
///
/// An `Event` can be consumed by the `map_event()` method that maps the event to a message.
pub enum Event {
    /// Event from a terminal emulator via Termina
    Terminal(termina::Event),
    /// Event from the Shuvarie core
    Core(shuvarie_core::Event),
}
