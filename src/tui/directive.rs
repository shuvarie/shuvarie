//! The composer's `@path` directive token encoding.
//!
//! A directive is `@` followed by a path. The tokenizer ends a directive at
//! the first whitespace, so a path that contains whitespace (a dropped
//! screenshot, say) must escape it. There is exactly one canonical encoding,
//! shared by typed directives and dropped paths:
//!
//! - a backslash before each whitespace character, and before each backslash.
//!
//! It is decoded by [`crate::tui::session`]'s `at_directives` at send time and
//! by the `@` mention popup at completion time, so both surfaces agree on what
//! a directive is. See `docs/designs/images-and-drops.md`.

/// Escape a path for the composer's `@` directive token: whitespace and
/// backslashes get a leading backslash so the whitespace-delimited tokenizer
/// keeps the whole path in one token.
pub fn escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        if ch.is_whitespace() || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Decode a directive token back into its path: the inverse of [`escape`].
/// Deliberately conservative outside the escape set — a backslash that escapes
/// neither whitespace nor another backslash is kept literally, so a hand-typed
/// Windows path such as `C:\Users\me` survives untouched.
pub fn unescape(token: &str) -> String {
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(&next) = chars.peek()
            && (next.is_whitespace() || next == '\\')
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

/// Whether `text` contains whitespace that is not part of an `\<space>` escape
/// pair. A directive token is whitespace-free under this predicate.
pub fn has_unescaped_whitespace(text: &str) -> bool {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            // Skip the escaped character, whatever it is: it never ends the
            // token, and an escaped whitespace keeps the token alive.
            chars.next();
            continue;
        }
        if ch.is_whitespace() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_round_trips_whitespace_and_backslashes() {
        for path in [
            "/tmp/Screenshot 2026-05-27 at 13.37.31.png",
            "a\tb.png",
            "C:\\Users\\me\\shot.png",
            "plain.png",
            "trailing ",
        ] {
            let escaped = escape(path);
            assert_eq!(unescape(&escaped), path, "round trip for {path:?}");
        }
    }

    #[test]
    fn escape_only_marks_whitespace_and_backslashes() {
        assert_eq!(escape("a b.png"), "a\\ b.png");
        assert_eq!(escape("C:\\Users\\me"), "C:\\\\Users\\\\me");
        assert_eq!(escape("plain.png"), "plain.png");
    }

    #[test]
    fn unescape_keeps_lone_backslashes_literal() {
        // A hand-typed Windows path: `\U`, `\m` escape nothing in our set.
        assert_eq!(
            unescape("C:\\Users\\me\\shot.png"),
            "C:\\Users\\me\\shot.png"
        );
        // The escape set itself still decodes.
        assert_eq!(unescape("a\\ b\\ c"), "a b c");
        assert_eq!(unescape("a\\\\b"), "a\\b");
    }

    #[test]
    fn unescaped_whitespace_detection() {
        assert!(!has_unescaped_whitespace("a\\ b\\ c.png"));
        assert!(has_unescaped_whitespace("a b.png"));
        assert!(has_unescaped_whitespace("a\\  b"));
        assert!(!has_unescaped_whitespace("plain.png"));
        assert!(!has_unescaped_whitespace("C:\\Users"));
    }
}
