//! Opt-in raw input tracing, for diagnosing what a terminal actually sends.
//!
//! Set `SHUVARIE_TRACE_INPUT=1` (or `true`/`yes`/`on`) to append every raw
//! terminal input event — paste payloads and key events, escaped — to
//! `$TMPDIR/shuvarie-input-trace.log`; any other value is taken as the file
//! path. It exists so a user can confirm how *their* terminal delivers a file
//! drop (one bracketed paste, a burst of keystrokes, or nothing at all) before
//! trusting the dropped-path fallback. See `docs/designs/images-and-drops.md`.
//!
//! Disabled (unset, `0`, `false`, `off`, `no`) the module is a no-op: the sink
//! is opened lazily on the first event and never touched again.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use termina::event::Event as TerminalEvent;

/// The trace sink, initialized once from the environment. `None` when tracing
/// is off or the file could not be opened.
static TRACE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Record one raw terminal event. A no-op unless tracing is enabled.
pub fn record(event: &TerminalEvent) {
    let Some(sink) = TRACE.get_or_init(open_from_env) else {
        return;
    };
    let Some(line) = format_line(event) else {
        return;
    };
    if let Ok(mut file) = sink.lock() {
        let _ = writeln!(file, "{line}");
    }
}

/// Open the trace file named by `SHUVARIE_TRACE_INPUT`. Called once.
fn open_from_env() -> Option<Mutex<File>> {
    let value = std::env::var("SHUVARIE_TRACE_INPUT").ok()?;
    let path = trace_path(&value)?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    Some(Mutex::new(file))
}

/// The file to write, or `None` when tracing is off. `1`/`true`/`yes`/`on`
/// select the default path; anything else is used verbatim as a path; the
/// off spellings (and an empty value) disable.
fn trace_path(value: &str) -> Option<PathBuf> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "off" | "no" => None,
        "1" | "true" | "yes" | "on" => Some(default_path()),
        _ => Some(PathBuf::from(value.trim())),
    }
}

/// The default trace location: a fixed name in the temp directory, so the user
/// can `tail -f` it without hunting for a pid-stamped path.
fn default_path() -> PathBuf {
    std::env::temp_dir().join("shuvarie-input-trace.log")
}

/// One trace line for an event, or `None` for the terminal's replies to our own
/// queries (CSI/OSC/DCS), which are startup noise rather than user input.
fn format_line(event: &TerminalEvent) -> Option<String> {
    match event {
        TerminalEvent::Paste(text) => {
            Some(format!("paste len={} {}", text.len(), escape_payload(text)))
        }
        TerminalEvent::Key(key) => Some(format!(
            "key {:?} mods={:?} kind={:?}",
            key.code, key.modifiers, key.kind
        )),
        TerminalEvent::Mouse(mouse) => Some(format!(
            "mouse {:?} col={} row={}",
            mouse.kind, mouse.column, mouse.row
        )),
        TerminalEvent::WindowResized(size) => {
            Some(format!("resize cols={} rows={}", size.cols, size.rows))
        }
        TerminalEvent::FocusIn => Some("focus in".to_string()),
        TerminalEvent::FocusOut => Some("focus out".to_string()),
        _ => None,
    }
}

/// Escape a payload for the trace file: printable ASCII verbatim (with `"` and
/// `\` escaped), the common control characters as `\n`/`\r`/`\t`, and every
/// other byte as `\xNN` so nothing can forge a line break.
fn escape_payload(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for byte in text.bytes() {
        match byte {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7e => out.push(byte as char),
            other => out.push_str(&format!("\\x{other:02x}")),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_payload_quotes_and_escapes() {
        assert_eq!(escape_payload("/tmp/a b.png"), "\"/tmp/a b.png\"");
        assert_eq!(escape_payload("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(escape_payload("line\nbreak"), "\"line\\nbreak\"");
        // Non-ASCII is hex-escaped byte-wise, so the file stays ASCII.
        assert_eq!(escape_payload("é"), "\"\\xc3\\xa9\"");
    }

    #[test]
    fn trace_path_maps_spellings() {
        assert!(trace_path("").is_none());
        assert!(trace_path("0").is_none());
        assert!(trace_path("false").is_none());
        assert!(trace_path("off").is_none());
        assert!(trace_path("no").is_none());
        assert_eq!(trace_path("1"), Some(default_path()));
        assert_eq!(trace_path("YES"), Some(default_path()));
        assert_eq!(trace_path("/tmp/x.log"), Some(PathBuf::from("/tmp/x.log")));
    }

    #[test]
    fn format_line_covers_paste_and_keys() {
        let pasted = TerminalEvent::Paste("/tmp/shot.png".to_string());
        assert_eq!(
            format_line(&pasted).as_deref(),
            Some("paste len=13 \"/tmp/shot.png\"")
        );
    }
}
