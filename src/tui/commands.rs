pub const TRIGGER_CHARS: [char; 2] = ['/', ':'];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandAction {
    OpenModelSelect,
    AddProvider,
    OpenSessionPicker,
    OpenTree,
    OpenScenePicker,
    OpenVariantPicker,
    NewSession,
    EditTitle,
    UndoLastTurn,
    Replay,
    Reload,
    ToggleSidebar,
    Quit,
}

impl CommandAction {
    pub const ALL: [CommandAction; 13] = [
        CommandAction::OpenModelSelect,
        CommandAction::AddProvider,
        CommandAction::OpenSessionPicker,
        CommandAction::OpenTree,
        CommandAction::OpenScenePicker,
        CommandAction::OpenVariantPicker,
        CommandAction::NewSession,
        CommandAction::EditTitle,
        CommandAction::UndoLastTurn,
        CommandAction::Replay,
        CommandAction::Reload,
        CommandAction::ToggleSidebar,
        CommandAction::Quit,
    ];

    pub fn slash_name(self) -> &'static str {
        match self {
            CommandAction::OpenModelSelect => "model",
            CommandAction::AddProvider => "provider",
            CommandAction::OpenSessionPicker => "sessions",
            CommandAction::OpenTree => "tree",
            CommandAction::OpenScenePicker => "scene",
            CommandAction::OpenVariantPicker => "variant",
            CommandAction::NewSession => "new",
            CommandAction::EditTitle => "title",
            CommandAction::UndoLastTurn => "undo",
            CommandAction::Replay => "replay",
            CommandAction::Reload => "reload",
            CommandAction::ToggleSidebar => "sidebar",
            CommandAction::Quit => "quit",
        }
    }

    /// Whether the command accepts free-form arguments after its name
    /// (e.g. `/title My title`).
    pub fn takes_args(self) -> bool {
        matches!(
            self,
            CommandAction::EditTitle
                | CommandAction::OpenScenePicker
                | CommandAction::OpenVariantPicker
        )
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
            name: "Session tree",
            description: "Walk the tree, fork from a node",
            action: CommandAction::OpenTree,
            available: true,
        },
        CommandEntry {
            name: "Switch scene",
            description: "Pick the scene the agent runs under",
            action: CommandAction::OpenScenePicker,
            available: true,
        },
        CommandEntry {
            name: "Select variant",
            description: "Pick the model's reasoning effort",
            action: CommandAction::OpenVariantPicker,
            available: true,
        },
        CommandEntry {
            name: "Edit title",
            description: "Rename the current session",
            action: CommandAction::EditTitle,
            available: true,
        },
        CommandEntry {
            name: "Undo last turn",
            description: "Fork before the last prompt",
            action: CommandAction::UndoLastTurn,
            available: true,
        },
        CommandEntry {
            name: "Replay last turn",
            description: "Fork + re-run the last turn",
            action: CommandAction::Replay,
            available: true,
        },
        CommandEntry {
            name: "Reload skills",
            description: "Re-discover skills without a restart",
            action: CommandAction::Reload,
            available: true,
        },
        CommandEntry {
            name: "Toggle sidebar",
            description: "Collapse or expand the sidebar",
            action: CommandAction::ToggleSidebar,
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

/// A parsed `<trigger><alias> [args]` submission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedCommand {
    pub action: CommandAction,
    /// The trimmed remainder after the command name, `Some` only when
    /// non-empty. Commands that take no args (see
    /// [`CommandAction::takes_args`]) never parse with one.
    pub args: Option<String>,
}

/// Parse submitted text as a slash command: the whole (trimmed) text must be
/// of the form `<trigger><alias>` matching a known command
/// (case-insensitive), optionally followed by an argument string for commands
/// that take one. Escaped prefixes (`//`, `::`) never parse.
pub fn parse_command(text: &str) -> Option<ParsedCommand> {
    let text = text.trim();
    let first = text.chars().next()?;
    if !TRIGGER_CHARS.contains(&first) || is_escaped(text) {
        return None;
    }
    let rest = &text[first.len_utf8()..];
    let (alias, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if alias.is_empty() {
        return None;
    }
    let action = CommandAction::ALL
        .iter()
        .copied()
        .find(|a| a.slash_name().eq_ignore_ascii_case(alias))?;
    if !args.is_empty() && !action.takes_args() {
        return None;
    }
    Some(ParsedCommand {
        action,
        args: (!args.is_empty()).then(|| args.to_string()),
    })
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
            Some(ParsedCommand {
                action: CommandAction::OpenModelSelect,
                args: None
            })
        );
        assert_eq!(
            parse_command(":MODEL"),
            Some(ParsedCommand {
                action: CommandAction::OpenModelSelect,
                args: None
            })
        );
        assert_eq!(
            parse_command("/undo"),
            Some(ParsedCommand {
                action: CommandAction::UndoLastTurn,
                args: None
            })
        );
        assert_eq!(
            parse_command("/reload"),
            Some(ParsedCommand {
                action: CommandAction::Reload,
                args: None
            })
        );
        assert_eq!(
            parse_command("  /new  "),
            Some(ParsedCommand {
                action: CommandAction::NewSession,
                args: None
            })
        );
        assert_eq!(
            parse_command(":quit"),
            Some(ParsedCommand {
                action: CommandAction::Quit,
                args: None
            })
        );
        assert_eq!(
            parse_command("/QUIT"),
            Some(ParsedCommand {
                action: CommandAction::Quit,
                args: None
            })
        );
    }

    #[test]
    fn parse_command_with_arguments() {
        assert_eq!(
            parse_command("/title My title"),
            Some(ParsedCommand {
                action: CommandAction::EditTitle,
                args: Some("My title".into())
            })
        );
        assert_eq!(
            parse_command(":TITLE   spaced   out  "),
            Some(ParsedCommand {
                action: CommandAction::EditTitle,
                args: Some("spaced   out".into())
            })
        );
        assert_eq!(
            parse_command("/title"),
            Some(ParsedCommand {
                action: CommandAction::EditTitle,
                args: None
            })
        );
        assert_eq!(
            parse_command("/title   "),
            Some(ParsedCommand {
                action: CommandAction::EditTitle,
                args: None
            }),
            "whitespace-only remainder is no args"
        );
        assert_eq!(
            parse_command("/variant"),
            Some(ParsedCommand {
                action: CommandAction::OpenVariantPicker,
                args: None
            })
        );
        assert_eq!(
            parse_command("/variant   "),
            Some(ParsedCommand {
                action: CommandAction::OpenVariantPicker,
                args: None
            }),
            "whitespace-only remainder is no args"
        );
        assert_eq!(
            parse_command("/variant HIGH"),
            Some(ParsedCommand {
                action: CommandAction::OpenVariantPicker,
                args: Some("HIGH".into())
            })
        );
    }

    #[test]
    fn parse_rejects_non_commands() {
        assert_eq!(parse_command("//model"), None);
        assert_eq!(parse_command("::model"), None);
        assert_eq!(parse_command("/unknown"), None);
        assert_eq!(
            parse_command("/model x"),
            None,
            "commands without args never parse with one"
        );
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
