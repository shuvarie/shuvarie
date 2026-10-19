//! A Rhai syntax for syntect, which ships the curated Sublime package list and
//! has no Rhai in it.
//!
//! Without this, `highlight_code("rhai", …)` falls back to plain text and a
//! codemode script renders unhighlighted. The grammar lives in
//! `syntaxes/rhai.sublime-syntax` rather than in Rust: it is a declarative
//! grammar, it reads like every other syntax definition in the ecosystem, and
//! fixing a rule is a one-line diff instead of a nested-struct edit.

use std::sync::LazyLock;

use syntect::parsing::{SyntaxDefinition, SyntaxSet};

/// The grammar itself, compiled into the binary — the same posture the codemode
/// crates take, so an installed binary needs no data directory.
const RHAI: &str = include_str!("../syntaxes/rhai.sublime-syntax");

/// The syntax definition, parsed once.
fn definition() -> &'static SyntaxDefinition {
    static DEFINITION: LazyLock<SyntaxDefinition> = LazyLock::new(|| {
        // `true` matches `load_defaults_newlines`, the set this is added to: a
        // grammar built with the other convention parses every line one short.
        SyntaxDefinition::load_from_str(RHAI, true, None).expect("the bundled Rhai syntax is valid")
    });
    &DEFINITION
}

/// syntect's default set with Rhai added.
///
/// Building this re-links the whole set, so it is done once and shared: the
/// link is what makes a definition actually parse, and skipping it leaves every
/// context empty — which looks exactly like "no highlighting".
pub fn syntax_set() -> &'static SyntaxSet {
    static SET: LazyLock<SyntaxSet> = LazyLock::new(|| {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        builder.add(definition().clone());
        builder.build()
    });
    &SET
}

#[cfg(test)]
mod tests {
    use syntect::parsing::{ParseState, ScopeStack, ScopeStackOp};

    use super::*;

    /// The TUI addresses the grammar by token, so the token has to resolve —
    /// otherwise the lookup silently falls back to plain text.
    #[test]
    fn the_rhai_syntax_resolves_by_token() {
        let set = syntax_set();
        let found = set.find_syntax_by_token("rhai").expect("`rhai` resolves");
        assert_eq!(found.name, "Rhai");
        assert_eq!(found.file_extensions, vec!["rhai".to_string()]);
    }

    /// A set that builds but does not link leaves every context empty, so
    /// assert the scopes the palette actually styles.
    #[test]
    fn the_rules_paint_the_scopes_the_palette_styles() {
        let set = syntax_set();
        let syntax = set.find_syntax_by_token("rhai").unwrap();
        let mut parse = ParseState::new(syntax);
        let line = "// note\nlet total = 1 + 2;\nlet s = `n=${total}`;\n";
        let ops = parse
            .parse_line(line, set)
            .expect("the syntax parses without an internal error");

        let scopes: Vec<String> = ops
            .iter()
            .filter_map(|(_, op)| match op {
                ScopeStackOp::Push(scope) => Some(scope.build_string()),
                _ => None,
            })
            .collect();
        for expected in [
            "comment.line.double-slash",
            "keyword.control",
            "constant.numeric",
            "string.quoted.other",
        ] {
            assert!(
                scopes.iter().any(|scope| scope.contains(expected)),
                "no `{expected}` scope in {scopes:?}"
            );
        }
    }

    /// The default set still parses after being rebuilt around the new syntax:
    /// an added grammar that breaks the shared context ids would show up here
    /// rather than as unhighlighted Rust in the transcript.
    #[test]
    fn the_default_syntaxes_still_resolve_and_parse() {
        let set = syntax_set();
        let rust = set.find_syntax_by_token("rs").expect("rust resolves");
        let mut parse = ParseState::new(rust);
        let ops = parse.parse_line("fn main() {}\n", set).unwrap();
        // `storage.type.function` is what Rust emits for `fn`, and the palette's
        // keyword rule covers it -- so this asserts both that the default
        // grammars still link and that the shared theme rules still apply.
        assert!(
            ops.iter().any(|(_, op)| matches!(
                op,
                ScopeStackOp::Push(scope)
                    if scope.build_string().contains("storage.type.function")
            )),
            "rust stopped highlighting: {ops:?}"
        );
    }

    /// The scopes in effect at byte `at` of `line`, as rendered strings.
    fn scopes_at(line: &str, at: usize) -> Vec<String> {
        let set = syntax_set();
        let syntax = set.find_syntax_by_token("rhai").unwrap();
        let mut parse = ParseState::new(syntax);
        let mut stack = ScopeStack::new();
        for (index, op) in parse.parse_line(line, set).expect("the syntax parses") {
            if index > at {
                break;
            }
            stack.apply(&op).unwrap();
        }
        stack
            .as_slice()
            .iter()
            .map(|scope| scope.build_string())
            .collect()
    }

    /// The scopes pushed while parsing each line, with state carried across
    /// lines -- the shape that catches a context left open at a line's end.
    fn pushed_per_line(script: &str) -> Vec<Vec<String>> {
        let set = syntax_set();
        let syntax = set.find_syntax_by_token("rhai").unwrap();
        let mut parse = ParseState::new(syntax);
        script
            .lines()
            .map(|line| {
                parse
                    .parse_line(line, set)
                    .expect("the syntax parses")
                    .iter()
                    .filter_map(|(_, op)| match op {
                        ScopeStackOp::Push(scope) => Some(scope.build_string()),
                        _ => None,
                    })
                    .collect()
            })
            .collect()
    }

    fn is_string(scope: &str) -> bool {
        scope.contains("string.quoted.other")
    }

    /// The text after an interpolation is still string text, and the backtick
    /// that follows closes it.
    ///
    /// This regressed once: the interpolation's `}` rule sat after
    /// `include: main`, and includes expand inline, so main's punctuation rule
    /// matched the `}` first. The context never popped, the rest of the literal
    /// read as code, and the closing backtick was taken for the start of a new
    /// string -- so the leak continued onto the next line.
    #[test]
    fn a_strings_tail_stays_a_string_after_an_interpolation() {
        let line = "let s = `n=${total} more`;";

        let tail = &line[line.find("more").unwrap()..];
        assert!(
            scopes_at(line, line.find("more").unwrap())
                .iter()
                .any(|scope| is_string(scope)),
            "`{tail}` left the string: {:?}",
            scopes_at(line, line.find("more").unwrap())
        );

        let close = line.rfind('`').unwrap();
        assert!(
            scopes_at(line, close)
                .iter()
                .any(|scope| scope.contains("punctuation.definition.string.end")),
            "the closing backtick should end the literal: {:?}",
            scopes_at(line, close)
        );

        // Nothing after the literal is still inside it.
        assert!(
            !scopes_at(line, line.len() - 1)
                .iter()
                .any(|scope| scope.contains("string")),
            "the literal stayed open: {:?}",
            scopes_at(line, line.len() - 1)
        );
    }

    /// A brace-grouped expression inside an interpolation does not end it: the
    /// braces are counted, so `#{...}`'s `}` is not mistaken for the
    /// interpolation's own.
    #[test]
    fn a_map_literal_inside_an_interpolation_does_not_close_it() {
        let line = "let s = `m=${#{ a: 1 }.a} tail`;";
        let tail = line.find("tail").unwrap();
        assert!(
            scopes_at(line, tail).iter().any(|scope| is_string(scope)),
            "the tail left the string: {:?}",
            scopes_at(line, tail)
        );
        assert!(
            !scopes_at(line, line.len() - 1)
                .iter()
                .any(|scope| scope.contains("string")),
            "the literal stayed open: {:?}",
            scopes_at(line, line.len() - 1)
        );
    }

    /// The symptom a reader actually saw: lines after an interpolated literal
    /// kept rendering as string text, so the rest of the script went unpainted.
    #[test]
    fn a_closed_interpolation_does_not_swallow_the_next_line() {
        let pushed = pushed_per_line("let s = `n=${total} more`;\nlet t = 1;\n");
        assert!(
            pushed[1]
                .iter()
                .any(|scope| scope.contains("keyword.control")),
            "the second line did not highlight: {:?}",
            pushed[1]
        );
    }

    /// Two interpolations in one literal, the second on the same line as the
    /// first's text: each pair opens and closes on its own.
    #[test]
    fn several_interpolations_in_one_literal_close_cleanly() {
        let line = "`a ${b} c ${d} e`;";
        let tail = line.rfind('e').unwrap();
        assert!(
            scopes_at(line, tail).iter().any(|scope| is_string(scope)),
            "the tail left the string: {:?}",
            scopes_at(line, tail)
        );
        assert!(
            !scopes_at(line, line.len() - 1)
                .iter()
                .any(|scope| scope.contains("string")),
            "the literal stayed open: {:?}",
            scopes_at(line, line.len() - 1)
        );
    }
}
