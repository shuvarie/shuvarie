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
    Continue,
    Reload,
    Quit,
}

impl CommandAction {
    pub const ALL: [CommandAction; 10] = [
        CommandAction::OpenModelSelect,
        CommandAction::AddProvider,
        CommandAction::OpenSessionPicker,
        CommandAction::NewSession,
        CommandAction::UndoLastTurn,
        CommandAction::Redo,
        CommandAction::Replay,
        CommandAction::Continue,
        CommandAction::Reload,
        CommandAction::Quit,
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
            CommandAction::Continue => "continue",
            CommandAction::Reload => "reload",
            CommandAction::Quit => "quit",
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
            name: "Continue",
            description: "Resume the interrupted reply",
            action: CommandAction::Continue,
            available: true,
        },
        CommandEntry {
            name: "Reload skills",
            description: "Re-discover skills without a restart",
            action: CommandAction::Reload,
            available: true,
        },
        CommandEntry {
            name: "Quit",
            description: "Exit the program",
            action: CommandAction::Quit,
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

/// A parsed `/skill:<name> [args]` invocation (also accepted with the `:`
/// trigger: `:skill:<name>`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInvocation {
    pub name: String,
    pub args: Option<String>,
}

/// Parse submitted text as a skill invocation: `<trigger>skill:<name> [args]`.
/// Escaped prefixes (`//`, `::`) never parse. The first token after `skill:`
/// is the skill name; the rest (trimmed) is the args.
pub fn parse_skill_invocation(text: &str) -> Option<SkillInvocation> {
    let text = text.trim();
    let first = text.chars().next()?;
    if !TRIGGER_CHARS.contains(&first) || is_escaped(text) {
        return None;
    }
    let rest = text[first.len_utf8()..].strip_prefix("skill:")?;
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some(SkillInvocation {
        name: name.to_string(),
        args: (!args.is_empty()).then(|| args.to_string()),
    })
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
        assert_eq!(parse_command("/continue"), Some(CommandAction::Continue));
        assert_eq!(parse_command("/reload"), Some(CommandAction::Reload));
        assert_eq!(parse_command(":Continue"), Some(CommandAction::Continue));
        assert_eq!(parse_command("  /new  "), Some(CommandAction::NewSession));
        assert_eq!(parse_command(":quit"), Some(CommandAction::Quit));
        assert_eq!(parse_command("/QUIT"), Some(CommandAction::Quit));
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

    #[test]
    fn parse_skill_invocations() {
        assert_eq!(
            parse_skill_invocation("/skill:tokio"),
            Some(SkillInvocation {
                name: "tokio".into(),
                args: None
            })
        );
        assert_eq!(
            parse_skill_invocation("/skill:tokio explain buffering"),
            Some(SkillInvocation {
                name: "tokio".into(),
                args: Some("explain buffering".into())
            })
        );
        assert_eq!(
            parse_skill_invocation(":skill:tokio"),
            Some(SkillInvocation {
                name: "tokio".into(),
                args: None
            })
        );
        assert_eq!(
            parse_skill_invocation("  /skill:tokio  "),
            Some(SkillInvocation {
                name: "tokio".into(),
                args: None
            })
        );
    }

    #[test]
    fn parse_skill_invocation_rejects() {
        assert_eq!(parse_skill_invocation("//skill:tokio"), None);
        assert_eq!(parse_skill_invocation("::skill:tokio"), None);
        assert_eq!(parse_skill_invocation("/skill:"), None);
        assert_eq!(parse_skill_invocation("/skill: x"), None);
        assert_eq!(parse_skill_invocation("/skills:tokio"), None);
        assert_eq!(parse_skill_invocation("skill:tokio"), None);
        assert_eq!(parse_skill_invocation("/undo"), None);
        assert_eq!(parse_skill_invocation("hello"), None);
    }
}
