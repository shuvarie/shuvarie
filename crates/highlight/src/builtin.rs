//! syntect's own syntax set, built once.
//!
//! Every language the TUI highlights -- Rust, Python, JSON, and the rest -- is in
//! the curated Sublime set syntect ships, so this is only the "build it once" part.
//! It is a module of its own because the set is expensive to build and is what
//! makes a definition actually parse: skipping the build leaves every context
//! empty, which looks exactly like "no highlighting".

use std::sync::LazyLock;

use syntect::parsing::SyntaxSet;

/// syntect's default set, linked and ready to parse.
pub fn syntax_set() -> &'static SyntaxSet {
    static SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    &SET
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The TUI addresses a grammar by token, so the tokens it asks for have to
    /// resolve -- otherwise the lookup silently falls back to plain text.
    #[test]
    fn the_languages_the_tui_names_resolve() {
        let set = syntax_set();
        for (token, name) in [("python", "Python"), ("rust", "Rust"), ("json", "JSON")] {
            let found = set
                .find_syntax_by_token(token)
                .unwrap_or_else(|| panic!("`{token}` resolves"));
            assert_eq!(found.name, name, "`{token}` is not {name}");
        }
    }
}
