//! The result block: what the model reads back.
//!
//! One block, in the shape the references converge on -- how it went, the
//! script's value, what it printed, and what it called. Every one of those is
//! needed: the value is the answer, the console output is why it went wrong,
//! and the trace is what tells the model that a call was denied rather than
//! empty.

use serde_json::Value;

use crate::convert::truncate;
use crate::script::{CallTrace, ErrorKind, ScriptOutcome, humantime};

/// Render one run as the text the `codemode` tool returns, cut to fit.
pub fn block(outcome: &ScriptOutcome, max_chars: usize) -> String {
    let rendered = render(outcome);
    if rendered.chars().count() <= max_chars {
        return rendered;
    }
    let kept = truncate(&rendered, max_chars);
    format!(
        "{kept}\n… truncated: {} characters of output",
        rendered.chars().count()
    )
}

fn render(outcome: &ScriptOutcome) -> String {
    let mut out = header(outcome);

    match &outcome.result {
        Ok(value) => {
            // The `text()` items in order, then the script's own value: one
            // list, so a script that emits two things and returns a third
            // reads as three sections in the order it produced them.
            let mut sections: Vec<String> = value.items.iter().map(render_value).collect();
            if let Some(value) = &value.value {
                sections.push(render_value(value));
            }
            for section in sections {
                out.push('\n');
                out.push_str(&section);
            }
            out.push('\n');
        }
        Err(error) => {
            out.push('\n');
            out.push_str(&format!("{}: {}", kind_name(error.kind), error.message));
            if let (Some(line), Some(column)) = (error.line, error.column) {
                out.push_str(&format!("\n  at line {line}, column {column}"));
            } else if let Some(line) = error.line {
                out.push_str(&format!("\n  at line {line}"));
            }
            out.push('\n');
        }
    }

    if !outcome.logs.is_empty() {
        out.push_str("\n<console_output>\n");
        for log in &outcome.logs {
            out.push_str(log);
            out.push('\n');
        }
        out.push_str("</console_output>\n");
    }

    if !outcome.calls.is_empty() {
        out.push_str(&format!("\nTool calls ({}):\n", outcome.calls.len()));
        for (index, call) in outcome.calls.iter().enumerate() {
            out.push_str(&trace_line(index + 1, call));
        }
    }

    if outcome.dropped > 0 {
        out.push_str(&format!(
            "\n{} entries were dropped: the script produced more output than one result can carry.\n",
            outcome.dropped
        ));
    }

    out
}

fn header(outcome: &ScriptOutcome) -> String {
    let elapsed = humantime(outcome.elapsed);
    let calls = outcome.calls.len();
    match &outcome.result {
        Ok(_) => {
            let mut header = format!("Script completed in {elapsed}");
            if calls > 0 {
                header.push_str(&format!(
                    " ({calls} tool call{})",
                    if calls == 1 { "" } else { "s" }
                ));
            }
            header.push('.');
            header
        }
        Err(error) => format!(
            "Script failed in {elapsed} ({}).",
            kind_name(error.kind).to_lowercase()
        ),
    }
}

fn kind_name(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Parse => "Parse error",
        ErrorKind::Eval => "Error",
        ErrorKind::Limit => "Limit",
        ErrorKind::Stopped => "Stopped",
    }
}

/// One call in the trace: what, how long, and how it went.
fn trace_line(index: usize, call: &CallTrace) -> String {
    let outcome = match &call.error {
        None => "ok".to_string(),
        Some(error) => format!("failed: {}", truncate(error, 200)),
    };
    format!(
        "  {index}. {} — {} — {outcome}\n     {}\n",
        call.qualified,
        humantime(call.duration),
        call.args
    )
}

/// A value as the model should read it: text bare, structure as JSON.
fn render_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::script::{CallTrace, ScriptError, ScriptValue};

    fn call(qualified: &str, error: Option<&str>) -> CallTrace {
        CallTrace {
            qualified: qualified.to_string(),
            host_name: qualified.to_string(),
            args: "#{ path: \"src/main.rs\" }".to_string(),
            duration: Duration::from_millis(12),
            error: error.map(str::to_string),
        }
    }

    fn completed(items: Vec<Value>, value: Option<Value>) -> ScriptOutcome {
        ScriptOutcome {
            result: Ok(ScriptValue { items, value }),
            logs: Vec::new(),
            calls: Vec::new(),
            elapsed: Duration::from_millis(12),
            dropped: 0,
        }
    }

    #[test]
    fn a_value_only_result_is_just_the_value() {
        let outcome = completed(Vec::new(), Some(Value::String("42 files".to_string())));
        assert_eq!(
            block(&outcome, 20_000),
            "Script completed in 12ms.\n42 files\n"
        );
    }

    #[test]
    fn emitted_values_and_the_return_value_read_in_order() {
        let outcome = completed(
            vec![Value::String("first".to_string())],
            Some(serde_json::json!({ "count": 2 })),
        );
        assert_eq!(
            block(&outcome, 20_000),
            "Script completed in 12ms.\nfirst\n{\n  \"count\": 2\n}\n"
        );
    }

    #[test]
    fn structure_is_json_and_text_is_bare() {
        let outcome = completed(
            vec![serde_json::json!([1, 2])],
            Some(Value::String("plain".to_string())),
        );
        assert_eq!(
            block(&outcome, 20_000),
            "Script completed in 12ms.\n[\n  1,\n  2\n]\nplain\n"
        );
    }

    #[test]
    fn a_failure_names_its_kind_position_and_cause() {
        let mut outcome = completed(Vec::new(), None);
        outcome.result = Err(ScriptError {
            kind: ErrorKind::Eval,
            message: "Function not found: nope ()".to_string(),
            line: Some(4),
            column: Some(9),
        });
        assert_eq!(
            block(&outcome, 20_000),
            "Script failed in 12ms (error).\nError: Function not found: nope ()\n  \
             at line 4, column 9\n"
        );
    }

    #[test]
    fn logs_and_calls_are_carried_alongside_the_value() {
        let mut outcome = completed(Vec::new(), Some(Value::String("done".to_string())));
        outcome.logs = vec!["log: looking".to_string()];
        outcome.calls = vec![
            call("tools::read_file", None),
            call("tools::grep", Some("denied by the user")),
        ];
        outcome.elapsed = Duration::from_millis(2_450);

        assert_eq!(
            block(&outcome, 20_000),
            "Script completed in 2.5s (2 tool calls).\ndone\n\n\
             <console_output>\nlog: looking\n</console_output>\n\n\
             Tool calls (2):\n  \
             1. tools::read_file — 12ms — ok\n     #{ path: \"src/main.rs\" }\n  \
             2. tools::grep — 12ms — failed: denied by the user\n     #{ path: \"src/main.rs\" }\n"
        );
    }

    #[test]
    fn a_long_result_is_cut_and_says_so() {
        let outcome = completed(Vec::new(), Some(Value::String("x".repeat(500))));
        let rendered = block(&outcome, 100);
        assert!(rendered.chars().count() < 200, "{}", rendered.len());
        // The header (25) plus a newline, the value, and the closing newline.
        assert!(
            rendered.ends_with("… truncated: 527 characters of output"),
            "{rendered}"
        );
    }

    #[test]
    fn dropped_entries_are_announced() {
        let mut outcome = completed(Vec::new(), None);
        outcome.dropped = 7;
        assert!(block(&outcome, 20_000).contains("7 entries were dropped"));
    }

    #[test]
    fn a_silent_script_still_says_it_ran() {
        assert_eq!(
            block(&completed(Vec::new(), None), 20_000),
            "Script completed in 12ms.\n"
        );
    }
}
