use super::tabs::TabId;

/// Enumeration of all possible events that the Shuvarie TUI might be handling.
///
/// An `Event` can be consumed by the `map_event()` method that maps the event to a message.
#[allow(clippy::large_enum_variant)]
pub enum Event {
    /// Event from a terminal emulator via Termina
    Terminal(termina::Event),
    /// Event from one tab's Shuvarie core. The tab is carried along because
    /// every core reports about the agent behind *its* tab — a stream, a tool
    /// call, a session — and those must land there even while another tab is
    /// on screen.
    Core {
        tab: TabId,
        event: shuvarie_core::Event,
    },
}
