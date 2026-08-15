pub enum Event {
    Terminal(termina::Event),
    Core(shuvarie_core::Event),
}
