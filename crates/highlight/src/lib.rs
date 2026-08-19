pub mod diff;
pub mod md;
pub mod syntax;
pub mod theme;

pub use md::{plain, render};

#[cfg(test)]
mod tests;
