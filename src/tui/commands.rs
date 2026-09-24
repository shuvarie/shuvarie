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
    GenTitle,
    Export,
    UndoLastTurn,
    Replay,
    Compact,
    Reload,
    McpServers,
    McpReconnect,
    ToggleSidebar,
    Search,
    AssistedBy,
    Quit,
}

impl CommandAction {
    pub const ALL: [CommandAction; 20] = [
        CommandAction::OpenModelSelect,
        CommandAction::AddProvider,
        CommandAction::OpenSessionPicker,
        CommandAction::OpenTree,
        CommandAction::OpenScenePicker,
        CommandAction::OpenVariantPicker,
        CommandAction::NewSession,
        CommandAction::EditTitle,
        CommandAction::GenTitle,
        CommandAction::Export,
        CommandAction::UndoLastTurn,
        CommandAction::Replay,
        CommandAction::Compact,
        CommandAction::Reload,
        CommandAction::McpServers,
        CommandAction::McpReconnect,
        CommandAction::ToggleSidebar,
        CommandAction::Search,
        CommandAction::AssistedBy,
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
            CommandAction::GenTitle => "gen-title",
            CommandAction::Export => "export",
            CommandAction::UndoLastTurn => "undo",
            CommandAction::Replay => "replay",
            CommandAction::Compact => "compact",
            CommandAction::Reload => "reload",
            CommandAction::McpServers => "mcp",
            CommandAction::McpReconnect => "mcp-reconnect",
            CommandAction::ToggleSidebar => "sidebar",
            CommandAction::Search => "search",
            CommandAction::AssistedBy => "assisted-by",
            CommandAction::Quit => "quit",
        }
    }

    /// Whether the command accepts free-form arguments after its name
    /// (e.g. `/title My title`, `/compact focus on the parser work`).
    pub fn takes_args(self) -> bool {
        matches!(
            self,
            CommandAction::Compact
                | CommandAction::EditTitle
                | CommandAction::Export
                | CommandAction::McpReconnect
                | CommandAction::OpenScenePicker
                | CommandAction::OpenVariantPicker
                | CommandAction::Search
        )
    }
}

#[derive(Clone)]
pub struct CommandEntry {
    pub name: String,
    pub description: String,
    pub action: CommandRef,
    pub available: bool,
}

/// What running an entry dispatches to: a built-in UI action or a custom
/// command (a prompt template loaded from `.shuvarie/commands`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandRef {
    Builtin(CommandAction),
    Custom {
        /// The command directory name (its slash alias).
        name: String,
        /// The command's `model` frontmatter override
        /// (`<provider_type>/<model>`), passed through to the core for the
        /// turn.
        model: Option<String>,
    },
}

impl CommandRef {
    /// The slash alias this entry answers to: a builtin's slash name, or a
    /// custom command's name — rendered `custom:<name>` when it collides with
    /// a builtin's name (the builtin keeps the plain alias).
    pub fn slash_alias(&self) -> String {
        match self {
            CommandRef::Builtin(action) => action.slash_name().to_string(),
            CommandRef::Custom { name, .. } => {
                if CommandAction::ALL.iter().any(|a| a.slash_name() == name) {
                    format!("custom:{name}")
                } else {
                    name.clone()
                }
            }
        }
    }
}

impl CommandEntry {
    pub fn builtin(name: &'static str, description: &'static str, action: CommandAction) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            action: CommandRef::Builtin(action),
            available: true,
        }
    }

    pub fn custom(command: &shuvarie_core::CustomCommand) -> Self {
        Self {
            name: command.title.clone(),
            description: "Custom command".to_string(),
            action: CommandRef::Custom {
                name: command.name.clone(),
                model: command.model.clone(),
            },
            available: true,
        }
    }
}

pub fn default_commands() -> Vec<CommandEntry> {
    vec![
        CommandEntry::builtin(
            "Select model",
            "Pick the active model",
            CommandAction::OpenModelSelect,
        ),
        CommandEntry::builtin(
            "Add provider",
            "Add a new LLM provider",
            CommandAction::AddProvider,
        ),
        CommandEntry::builtin(
            "Switch session",
            "Resume or delete past sessions",
            CommandAction::OpenSessionPicker,
        ),
        CommandEntry::builtin(
            "New session",
            "Start a fresh conversation",
            CommandAction::NewSession,
        ),
        CommandEntry::builtin(
            "Session tree",
            "Walk the tree, fork from a node",
            CommandAction::OpenTree,
        ),
        CommandEntry::builtin(
            "Switch scene",
            "Pick the scene the agent runs under",
            CommandAction::OpenScenePicker,
        ),
        CommandEntry::builtin(
            "Select variant",
            "Pick the model's reasoning effort",
            CommandAction::OpenVariantPicker,
        ),
        CommandEntry::builtin(
            "Edit title",
            "Rename the current session",
            CommandAction::EditTitle,
        ),
        CommandEntry::builtin(
            "Generate title",
            "Draft the session title with the LLM",
            CommandAction::GenTitle,
        ),
        CommandEntry::builtin(
            "Export session",
            "Write the session to a JSON file",
            CommandAction::Export,
        ),
        CommandEntry::builtin(
            "Undo last turn",
            "Fork before the last prompt",
            CommandAction::UndoLastTurn,
        ),
        CommandEntry::builtin(
            "Replay last turn",
            "Fork + re-run the last turn",
            CommandAction::Replay,
        ),
        CommandEntry::builtin(
            "Compact context",
            "Summarize older history; optional focus instruction",
            CommandAction::Compact,
        ),
        CommandEntry::builtin(
            "Reload skills",
            "Re-discover skills and commands without a restart",
            CommandAction::Reload,
        ),
        CommandEntry::builtin(
            "MCP servers",
            "Show MCP server states",
            CommandAction::McpServers,
        ),
        CommandEntry::builtin(
            "Reconnect MCP",
            "Reconnect an MCP server",
            CommandAction::McpReconnect,
        ),
        CommandEntry::builtin(
            "Toggle sidebar",
            "Collapse or expand the sidebar",
            CommandAction::ToggleSidebar,
        ),
        CommandEntry::builtin(
            "Search chat",
            "Highlight text in the chat live",
            CommandAction::Search,
        ),
        CommandEntry::builtin(
            "Assisted by",
            "Show the Assisted-By trailer; Enter copies it",
            CommandAction::AssistedBy,
        ),
        CommandEntry::builtin("Quit", "Exit the program", CommandAction::Quit),
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

/// A parsed `<trigger><name> [args]` custom-command submission (also accepted
/// in the `custom:`-prefixed form `<trigger>custom:<name> [args]`, which is
/// how a command sharing a builtin's name is invoked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomInvocation {
    pub name: String,
    pub args: Option<String>,
}

/// Whether `name` could be a custom-command name (the loader-validated
/// set: lowercase a-z, 0-9, and hyphens).
fn is_command_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Parse submitted text as a custom-command invocation. Escaped prefixes
/// (`//`, `::`) never parse. A builtin's slash name without the `custom:`
/// prefix is never claimed here — it belongs to the builtin (which
/// [`parse_command`] handled or rejected for its args).
pub fn parse_custom_invocation(text: &str) -> Option<CustomInvocation> {
    let text = text.trim();
    let first = text.chars().next()?;
    if !TRIGGER_CHARS.contains(&first) || is_escaped(text) {
        return None;
    }
    let rest = &text[first.len_utf8()..];
    let (prefixed, rest) = match rest.strip_prefix("custom:") {
        Some(custom) => (true, custom),
        None => (false, rest),
    };
    let (name, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if name.is_empty() || !is_command_name(name) {
        return None;
    }
    if !prefixed && CommandAction::ALL.iter().any(|a| a.slash_name() == name) {
        return None;
    }
    Some(CustomInvocation {
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
        assert_eq!(
            parse_command("/compact"),
            Some(ParsedCommand {
                action: CommandAction::Compact,
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
            parse_command("/compact focus on the parser work"),
            Some(ParsedCommand {
                action: CommandAction::Compact,
                args: Some("focus on the parser work".into())
            })
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
    fn parse_mcp_commands() {
        assert_eq!(
            parse_command("/mcp"),
            Some(ParsedCommand {
                action: CommandAction::McpServers,
                args: None
            })
        );
        assert_eq!(
            parse_command("/MCP"),
            Some(ParsedCommand {
                action: CommandAction::McpServers,
                args: None
            })
        );
        assert_eq!(parse_command("/mcp extra"), None, "/mcp takes no arguments");
        assert_eq!(
            parse_command("/mcp-reconnect github"),
            Some(ParsedCommand {
                action: CommandAction::McpReconnect,
                args: Some("github".into())
            })
        );
        assert_eq!(
            parse_command("/mcp-reconnect"),
            Some(ParsedCommand {
                action: CommandAction::McpReconnect,
                args: None
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
    fn parse_recognizes_search_command() {
        assert_eq!(
            parse_command("/search"),
            Some(ParsedCommand {
                action: CommandAction::Search,
                args: None
            })
        );
        assert_eq!(
            parse_command(":SEARCH foo bar"),
            Some(ParsedCommand {
                action: CommandAction::Search,
                args: Some("foo bar".into())
            })
        );
    }

    #[test]
    fn parse_recognizes_export_with_optional_path() {
        assert_eq!(
            parse_command("/export"),
            Some(ParsedCommand {
                action: CommandAction::Export,
                args: None
            })
        );
        assert_eq!(
            parse_command("/EXPORT backups/out.json"),
            Some(ParsedCommand {
                action: CommandAction::Export,
                args: Some("backups/out.json".into())
            })
        );
    }

    #[test]
    fn parse_recognizes_gen_title_command() {
        assert_eq!(
            parse_command("/gen-title"),
            Some(ParsedCommand {
                action: CommandAction::GenTitle,
                args: None
            })
        );
        assert_eq!(
            parse_command(":GEN-TITLE"),
            Some(ParsedCommand {
                action: CommandAction::GenTitle,
                args: None
            })
        );
        // The command takes no arguments, so it never parses with one.
        assert_eq!(parse_command("/gen-title now"), None);
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

    #[test]
    fn parse_custom_invocations() {
        assert_eq!(
            parse_custom_invocation("/commit"),
            Some(CustomInvocation {
                name: "commit".into(),
                args: None
            })
        );
        assert_eq!(
            parse_custom_invocation(":commit fix the tests"),
            Some(CustomInvocation {
                name: "commit".into(),
                args: Some("fix the tests".into())
            })
        );
        assert_eq!(
            parse_custom_invocation("/commit fix the tests"),
            Some(CustomInvocation {
                name: "commit".into(),
                args: Some("fix the tests".into())
            })
        );
        assert_eq!(
            parse_custom_invocation("  /custom:model  "),
            Some(CustomInvocation {
                name: "model".into(),
                args: None
            })
        );
    }

    #[test]
    fn parse_custom_invocation_rejects() {
        assert_eq!(parse_custom_invocation("//commit"), None);
        assert_eq!(parse_custom_invocation("::commit"), None);
        assert_eq!(parse_custom_invocation("/custom:"), None);
        // Invalid name charset: falls through to a literal send.
        assert_eq!(parse_custom_invocation("/usr/bin/ls"), None);
        assert_eq!(parse_custom_invocation("/Commit"), None);
        assert_eq!(parse_custom_invocation("/skill:x"), None);
        assert_eq!(parse_custom_invocation("hello"), None);
    }

    #[test]
    fn slash_alias_collisions_render_as_custom() {
        assert_eq!(
            CommandRef::Custom {
                name: "commit".into(),
                model: None
            }
            .slash_alias(),
            "commit"
        );
        assert_eq!(
            CommandRef::Custom {
                name: "model".into(),
                model: None
            }
            .slash_alias(),
            "custom:model"
        );
        assert_eq!(
            CommandRef::Builtin(CommandAction::OpenModelSelect).slash_alias(),
            "model"
        );
    }
}
