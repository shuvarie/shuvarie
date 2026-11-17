//! Converting between JSON and the text a person reads.
//!
//! JSON is the only thing that crosses between the host and a script: tool
//! arguments go in as a JSON object and come back as a JSON value, and the
//! runtime does the Python-side conversion. What is left here is display --
//! the one-line preview a call's trace shows, and the cut that keeps a result
//! from swallowing the context window.

use serde_json::Value;

/// A one-line preview of a value, for a call's trace.
pub fn preview(value: &Value, max_chars: usize) -> String {
    let text = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    truncate(&text, max_chars)
}

/// Truncate on a character boundary, marking that it happened.
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn previews_are_one_line_and_marked_when_cut() {
        assert_eq!(
            preview(&json!({ "path": "src/main.rs" }), 80),
            r#"{"path":"src/main.rs"}"#
        );
        assert_eq!(preview(&json!("a string"), 80), "a string");
        assert_eq!(preview(&json!("abcdefghij"), 5), "abcd…");
        // Truncation counts characters, not bytes.
        assert_eq!(preview(&json!("日本語テキスト"), 4), "日本語…");
    }

    #[test]
    fn a_short_string_is_untouched() {
        assert_eq!(truncate("exact", 5), "exact");
        assert_eq!(truncate("", 5), "");
    }
}
