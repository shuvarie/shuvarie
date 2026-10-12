//! Classify a bracketed-paste payload as ordinary text or as a dropped file
//! path.
//!
//! Dragging a file onto a terminal mostly types its path into the program as
//! one bracketed-paste payload — but the encoding is the terminal's choice
//! (quoted, backslash-escaped, percent-encoded, or a `file://` URI; see
//! `docs/designs/images-and-drops.md`). [`classify`] decodes the common forms
//! and accepts the payload as a path only when it is a single line with a
//! path shape, so pasting prose (or a lone word) stays text.

/// What a paste payload turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// Ordinary text: paste it into the composer as-is.
    Text,
    /// A dropped file path, decoded (no quotes, escapes, or `file://`; spaces
    /// literal).
    Path(String),
}

/// Classify a paste payload. A payload that is not a single-line path-shaped
/// token is [`Payload::Text`].
pub fn classify(payload: &str) -> Payload {
    let trimmed = payload.trim_matches(|ch: char| ch.is_whitespace());
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) {
        return Payload::Text;
    }
    match decode(trimmed) {
        Some(path) if looks_like_path(&path) => Payload::Path(path),
        _ => Payload::Text,
    }
}

/// Decode the wrapping a terminal may put around a dropped path: surrounding
/// quotes, a `file://` scheme, backslash escapes, and percent-encoding.
/// `None` for a non-local URI (a remote `file://host/…`, or any other scheme)
/// or bytes that are not UTF-8 after percent-decoding.
fn decode(token: &str) -> Option<String> {
    let token = strip_quotes(token).unwrap_or(token);
    let token = match token.strip_prefix("file://") {
        Some(rest) => {
            let rest = rest.strip_prefix("localhost").unwrap_or(rest);
            // `file:///abs/path` leaves a leading `/`; anything else is a
            // remote host we cannot read.
            rest.starts_with('/').then_some(rest)?
        }
        // A different scheme (`https://…`) is a URL, not an attachable file.
        None if token.contains("://") => return None,
        None => token,
    };
    let token = if has_escape_pair(token) {
        unescape_backslashes(token)
    } else {
        // No `\ ` marker: a backslash is a path character (Windows), not an
        // escape, so leave it alone.
        token.to_string()
    };
    let decoded = percent_decode(&token)?;
    (!decoded.is_empty()).then_some(decoded)
}

/// The contents of a matching pair of surrounding quotes, if present.
fn strip_quotes(token: &str) -> Option<&str> {
    let bytes = token.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'\'' || bytes[0] == b'"')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        return Some(&token[1..token.len() - 1]);
    }
    None
}

/// Whether the payload actually uses backslash escapes: `\ ` (an escaped
/// space) or `\\`.
fn has_escape_pair(token: &str) -> bool {
    let mut chars = token.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(&next) = chars.peek()
            && (next.is_whitespace() || next == '\\')
        {
            return true;
        }
    }
    false
}

/// Decode backslash escapes: whitespace and backslash directly, plus the
/// shell metacharacters a POSIX shell quotes. Any other `\x` is kept literal.
fn unescape_backslashes(token: &str) -> String {
    const SHELL_ESCAPED: &str = "\\'\"()[]{}!$&*;<>?|#~%`";
    let mut out = String::with_capacity(token.len());
    let mut chars = token.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(&next) = chars.peek()
            && (next.is_whitespace() || SHELL_ESCAPED.contains(next))
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

/// Decode `%XX` percent-encoding (as `text/uri-list` carries it). A `%` not
/// followed by two hex digits is kept literally. `None` when the decoded bytes
/// are not valid UTF-8.
fn percent_decode(token: &str) -> Option<String> {
    let bytes = token.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Whether a decoded payload has the shape of a file path rather than a word.
/// An absolute, home, or explicit-relative form is accepted directly; a
/// relative path needs a separator *and* a file-suffix-looking extension, so a
/// bare prose word (or a bare filename) is never mistaken for a drop.
fn looks_like_path(path: &str) -> bool {
    if path.contains(['\n', '\r']) {
        return false;
    }
    if path.starts_with('/')
        || path.starts_with("~/")
        || path.starts_with("./")
        || path.starts_with("../")
    {
        // A trailing separator is a directory, not an attachable file.
        return !path.ends_with('/');
    }
    // Windows drive-absolute (`C:\…`, `C:/…`) and UNC (`\\host\share`).
    let chars: Vec<char> = path.chars().collect();
    if chars.len() >= 3
        && chars[0].is_ascii_alphabetic()
        && chars[1] == ':'
        && (chars[2] == '\\' || chars[2] == '/')
    {
        return true;
    }
    if path.starts_with("\\\\") {
        return true;
    }
    (path.contains('/') || path.contains('\\')) && has_extension(path)
}

/// The same file-suffix heuristic the directive tokenizer uses: a final `.`
/// followed by 1–8 alphanumeric characters, and not a URL.
fn has_extension(path: &str) -> bool {
    !path.contains("://")
        && path.rsplit_once('.').is_some_and(|(_, ext)| {
            !ext.is_empty() && ext.len() <= 8 && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::directive;

    fn path(payload: &str) -> String {
        match classify(payload) {
            Payload::Path(path) => path,
            Payload::Text => panic!("expected a path for {payload:?}"),
        }
    }

    #[test]
    fn plain_and_escaped_paths_decode() {
        assert_eq!(path("/tmp/shot.png"), "/tmp/shot.png");
        assert_eq!(
            path("/tmp/Screenshot\\ 2026-05-27\\ at\\ 13.37.31.png"),
            "/tmp/Screenshot 2026-05-27 at 13.37.31.png",
        );
        assert_eq!(path("~/shots/a.webp"), "~/shots/a.webp");
        assert_eq!(path("./assets/logo.svg"), "./assets/logo.svg");
    }

    #[test]
    fn quoted_drops_decode() {
        assert_eq!(
            path("'/tmp/Screenshot 2026-05-27 at 13.37.31.png'"),
            "/tmp/Screenshot 2026-05-27 at 13.37.31.png",
        );
        assert_eq!(path("\"/tmp/a b.png\""), "/tmp/a b.png");
    }

    #[test]
    fn percent_encoded_and_uri_drops_decode() {
        assert_eq!(
            path("/tmp/Screenshot%202026-05-27%20at%2013.37.31.png"),
            "/tmp/Screenshot 2026-05-27 at 13.37.31.png",
        );
        assert_eq!(
            path("file:///tmp/Screenshot%20shot.png"),
            "/tmp/Screenshot shot.png",
        );
        assert_eq!(path("file://localhost/tmp/a.png"), "/tmp/a.png");
        // A remote host is not attachable.
        assert_eq!(classify("file://elsewhere/tmp/a.png"), Payload::Text);
        // A non-file scheme is a URL.
        assert_eq!(classify("https://x.com/a.png"), Payload::Text);
    }

    #[test]
    fn prose_and_bare_words_stay_text() {
        assert_eq!(classify("ping @bob about it"), Payload::Text);
        assert_eq!(classify("readme.md"), Payload::Text, "bare filename");
        assert_eq!(classify("hello"), Payload::Text);
        assert_eq!(classify(""), Payload::Text);
        assert_eq!(classify("   \n  "), Payload::Text);
        assert_eq!(
            classify("/tmp/a.png\n/tmp/b.png"),
            Payload::Text,
            "multi-line is text"
        );
        assert_eq!(
            classify("/tmp/notes.txt\n"),
            Payload::Path("/tmp/notes.txt".into()),
            "a single trailing newline still trims to a path"
        );
    }

    #[test]
    fn directory_paths_are_not_attachments() {
        assert_eq!(classify("/tmp/shots/"), Payload::Text);
    }

    #[test]
    fn windows_paths_are_recognized() {
        assert_eq!(path("C:\\Users\\me\\shot.png"), "C:\\Users\\me\\shot.png");
        assert_eq!(path("C:/Users/me/shot.png"), "C:/Users/me/shot.png");
    }

    #[test]
    fn decoded_paths_round_trip_through_the_directive_escape() {
        // Whatever `classify` accepts must survive the composer's escaping, so
        // the send-time tokenizer sees the same path back.
        for payload in [
            "/tmp/Screenshot 2026-05-27 at 13.37.31.png",
            "C:\\Users\\me\\My Documents\\shot.png",
            "~/My Pictures/a b.png",
        ] {
            let decoded = path(payload);
            assert_eq!(directive::unescape(&directive::escape(&decoded)), decoded);
        }
    }

    #[test]
    fn unescaped_backslashes_are_kept_literal() {
        // No `\ ` pair anywhere: backslashes are separators, not escapes.
        assert_eq!(path("C:\\Users\\me\\shot.png"), "C:\\Users\\me\\shot.png");
    }
}
