//! JSON is the only thing that crosses between the host and a script.
//!
//! Tool arguments go in as a JSON object and come out as a JSON value; nothing
//! Rhai-shaped is ever sent over the bridge, which is what keeps the script
//! thread free to own its engine (and Rhai free of its `sync` feature).

use rhai::Dynamic;
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConvertError {
    #[error("{0}")]
    Rhai(#[from] Box<rhai::EvalAltResult>),
}

/// JSON in: a script's arguments.
pub fn to_dynamic(value: &Value) -> Result<Dynamic, ConvertError> {
    Ok(rhai::serde::to_dynamic(value)?)
}

/// JSON out: whatever a script produced.
///
/// A script value with no JSON equivalent (a function pointer, a unit) is a
/// mistake worth reporting rather than silently dropping: `()` becomes null,
/// and anything else Rhai cannot serialize is an error the result block shows.
pub fn from_dynamic(value: &Dynamic) -> Result<Value, ConvertError> {
    if value.is_unit() {
        return Ok(Value::Null);
    }
    Ok(rhai::serde::from_dynamic(value)?)
}

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
    fn json_round_trips_through_a_script_value() {
        for value in [
            json!(null),
            json!(true),
            json!(7),
            json!(-7),
            json!(1.5),
            json!("text"),
            json!([1, "two", null]),
            json!({ "nested": { "k": "v" } }),
        ] {
            let dynamic = to_dynamic(&value).unwrap();
            assert_eq!(from_dynamic(&dynamic).unwrap(), value, "{value}");
        }
    }

    #[test]
    fn unit_is_null_rather_than_an_error() {
        assert_eq!(from_dynamic(&Dynamic::UNIT).unwrap(), Value::Null);
    }

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
}
