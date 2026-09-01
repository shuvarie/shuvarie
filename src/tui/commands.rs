pub const TRIGGER_CHARS: [char; 2] = ['/', ':'];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandAction {
    OpenModelSelect,
    AddProvider,
    OpenSessionPicker,
    NewSession,
    UndoLastTurn,
    Redo,
    Replay,
    Resume,
}

impl CommandAction {
    pub const ALL: [CommandAction; 8] = [
        CommandAction::OpenModelSelect,
        CommandAction::AddProvider,
        CommandAction::OpenSessionPicker,
        CommandAction::NewSession,
        CommandAction::UndoLastTurn,
        CommandAction::Redo,
        CommandAction::Replay,
        CommandAction::Resume,
    ];

    pub fn slash_name(self) -> &'static str {
        match self {
            CommandAction::OpenModelSelect => "model",
            CommandAction::AddProvider => "provider",
            CommandAction::OpenSessionPicker => "sessions",
            CommandAction::NewSession => "new",
            CommandAction::UndoLastTurn => "undo",
            CommandAction::Redo => "redo",
            CommandAction::Replay => "replay",
            CommandAction::Resume => "resume",
        }
    }
}

#[derive(Clone)]
pub struct CommandEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub action: CommandAction,
    pub available: bool,
}

pub fn default_commands() -> Vec<CommandEntry> {
    vec![
        CommandEntry {
            name: "Select model",
            description: "Pick the active model",
            action: CommandAction::OpenModelSelect,
            available: true,
        },
        CommandEntry {
            name: "Add provider",
            description: "Add a new LLM provider",
            action: CommandAction::AddProvider,
            available: true,
        },
        CommandEntry {
            name: "Switch session",
            description: "Resume or delete past sessions",
            action: CommandAction::OpenSessionPicker,
            available: true,
        },
        CommandEntry {
            name: "New session",
            description: "Start a fresh conversation",
            action: CommandAction::NewSession,
            available: true,
        },
        CommandEntry {
            name: "Undo last turn",
            description: "Revert last chat + file changes",
            action: CommandAction::UndoLastTurn,
            available: true,
        },
        CommandEntry {
            name: "Redo",
            description: "Restore the last undone turn",
            action: CommandAction::Redo,
            available: true,
        },
        CommandEntry {
            name: "Replay last turn",
            description: "Undo + re-run the last turn",
            action: CommandAction::Replay,
            available: true,
        },
        CommandEntry {
            name: "Resume stream",
            description: "Restart an interrupted turn",
            action: CommandAction::Resume,
            available: true,
        },
    ]
}

/// Whether the text starts with a doubled trigger char (`//` or `::`), which
/// escapes the prefix into a literal character.
pub fn is_escaped(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), Some(d)) => TRIGGER_CHARS.contains(&c) && c == d,
        _ => false,
    }
}

/// Strip one escaped trigger char (`//x` -> `/x`).
pub fn unescape(text: &str) -> &str {
    if is_escaped(text) {
        let skip = text.chars().next().map_or(0, char::len_utf8);
        &text[skip..]
    } else {
        text
    }
}

/// Parse submitted text as a slash command: the whole (trimmed) text must be a
/// single token of the form `<trigger><alias>` matching a known command
/// (case-insensitive). Escaped prefixes (`//`, `::`) never parse.
pub fn parse_command(text: &str) -> Option<CommandAction> {
    let text = text.trim();
    let first = text.chars().next()?;
    if !TRIGGER_CHARS.contains(&first) || is_escaped(text) {
        return None;
    }
    let rest = &text[first.len_utf8()..];
    if rest.is_empty() || rest.chars().any(char::is_whitespace) {
        return None;
    }
    CommandAction::ALL
        .iter()
        .copied()
        .find(|a| a.slash_name().eq_ignore_ascii_case(rest))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaped_prefixes_detected() {
        assert!(is_escaped("//"));
        assert!(is_escaped("//x"));
        assert!(is_escaped("::"));
        assert!(is_escaped("::x"));
        assert!(!is_escaped("/x"));
        assert!(!is_escaped(":x"));
        assert!(!is_escaped("/"));
        assert!(!is_escaped(":"));
        assert!(!is_escaped("x//"));
        assert!(
            is_escaped("// x"),
            "the doubled prefix is the escape sequence"
        );
    }

    #[test]
    fn unescape_strips_one_char() {
        assert_eq!(unescape("//model"), "/model");
        assert_eq!(unescape("::hi"), ":hi");
        assert_eq!(unescape("///x"), "//x");
        assert_eq!(unescape("/model"), "/model");
        assert_eq!(unescape("plain"), "plain");
    }

    #[test]
    fn parse_recognizes_commands() {
        assert_eq!(
            parse_command("/model"),
            Some(CommandAction::OpenModelSelect)
        );
        assert_eq!(
            parse_command(":MODEL"),
            Some(CommandAction::OpenModelSelect)
        );
        assert_eq!(parse_command("/undo"), Some(CommandAction::UndoLastTurn));
        assert_eq!(parse_command("  /new  "), Some(CommandAction::NewSession));
        assert_eq!(parse_command("/resume"), Some(CommandAction::Resume));
    }

    #[test]
    fn parse_rejects_non_commands() {
        assert_eq!(parse_command("//model"), None);
        assert_eq!(parse_command("::model"), None);
        assert_eq!(parse_command("/unknown"), None);
        assert_eq!(parse_command("/model x"), None);
        assert_eq!(parse_command("/"), None);
        assert_eq!(parse_command(":"), None);
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command(""), None);
        assert_eq!(parse_command("/undo now"), None);
        assert_eq!(parse_command(":/undo"), None);
    }
}
