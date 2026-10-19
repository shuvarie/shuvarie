pub mod code;
pub mod diff;
pub mod md;
/// A Rhai syntax definition for syntect, which does not ship one.
pub mod rhai;
pub mod syntax;
pub mod table;
pub mod theme;

pub use md::{MdPass, plain, render, render_dim, render_pass, render_pass_dim};

#[cfg(test)]
mod tests;
