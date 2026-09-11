pub mod code;
pub mod diff;
pub mod md;
pub mod syntax;
pub mod table;
pub mod theme;

pub use md::{MdPass, plain, render, render_pass};

#[cfg(test)]
mod tests;
