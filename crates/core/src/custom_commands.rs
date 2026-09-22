use std::path::{Path, PathBuf};

use serde::Deserialize;

use shuvarie_config::WORKSPACE_DIR_NAME;

const COMMAND_FILE_NAME: &str = "command.md";
/// The placeholder replaced by the invocation's arguments.
const ARGUMENTS_PLACEHOLDER: &str = "{{arguments}}";

/// A custom command: a prompt template living in
/// `<workspace .shuvarie(-dev) dir or config_dir>/commands/<name>/command.md`.
/// The frontmatter records `title` (display fallback: the directory name) and
/// an optional `model` override (`<provider_type>/<model>`) the turn streams
/// on.
#[derive(Debug, Clone)]
pub struct CustomCommand {
    /// The directory name; also the slash alias.
    pub name: String,
    /// The frontmatter `title`, falling back to [`Self::name`].
    pub title: String,
    /// The frontmatter `model` override: `<provider_type>/<model>`.
    pub model: Option<String>,
    /// The command directory (holding `command.md`).
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CustomCommandWarning {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct CustomCommands {
    pub commands: Vec<CustomCommand>,
    pub warnings: Vec<CustomCommandWarning>,
}

impl CustomCommands {
    /// Loads the command roster: the workspace `<WORKSPACE_DIR_NAME>/commands`
    /// tree first (it wins over duplicates), then the global config dir's
    /// `commands`.
    pub fn load(workspace_root: &Path) -> CustomCommands {
        let global = shuvarie_config::config_dir().ok();
        Self::load_from(workspace_root, global.as_deref())
    }

    /// The walk behind [`CustomCommands::load`], with an injectable global
    /// location so tests stay hermetic.
    fn load_from(workspace_root: &Path, global_config: Option<&Path>) -> CustomCommands {
        let mut out = CustomCommands::default();
        out.collect(&workspace_root.join(WORKSPACE_DIR_NAME).join("commands"));
        if let Some(global) = global_config {
            out.collect(&global.join("commands"));
        }
        out.commands.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Every immediate subdirectory of `dir` holding a `command.md` is a
    /// command. Names duplicate-checked against the commands loaded so far —
    /// the workspace dir is collected first, so a later (global) duplicate is
    /// skipped with a warning.
    fn collect(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let (command, messages) = CustomCommand::from_dir(&path, name);
            for message in messages {
                self.warnings.push(CustomCommandWarning {
                    path: path.join(COMMAND_FILE_NAME),
                    message,
                });
            }
            if let Some(command) = command {
                if self
                    .commands
                    .iter()
                    .any(|loaded| loaded.name == command.name)
                {
                    self.warnings.push(CustomCommandWarning {
                        path: path.join(COMMAND_FILE_NAME),
                        message: format!(
                            "duplicate command name \"{}\" ignored (the workspace command wins)",
                            command.name
                        ),
                    });
                } else {
                    self.commands.push(command);
                }
            }
        }
    }
}

impl CustomCommand {
    /// Load the command rooted at `dir`; returns it plus validation messages.
    /// `None` means the command is not loaded (unreadable, unparsable) —
    /// every failure still yields a message.
    fn from_dir(dir: &Path, name: &str) -> (Option<CustomCommand>, Vec<String>) {
        let mut messages = Vec::new();
        let file = dir.join(COMMAND_FILE_NAME);
        let Ok(content) = std::fs::read_to_string(&file) else {
            messages.push(format!("failed to read {COMMAND_FILE_NAME}"));
            return (None, messages);
        };
        for error in crate::skills::validate_name(name) {
            messages.push(error);
        }
        let front_matter = match front_matter_split(&content) {
            Some((yaml, _)) => match yaml_serde::from_str::<FrontMatter>(yaml) {
                Ok(front_matter) => Some(front_matter),
                Err(_) => {
                    messages.push(format!("failed to parse {COMMAND_FILE_NAME} frontmatter"));
                    return (None, messages);
                }
            },
            None => None,
        };
        let model = front_matter
            .as_ref()
            .and_then(|fm| fm.model.as_deref())
            .map(str::trim)
            .filter(|model| !model.is_empty());
        if let Some(model) = model
            && parse_model_spec(model).is_none()
        {
            messages.push(format!(
                "model must be `<provider>/<model>` (found `{model}`); \
                 the override will not resolve"
            ));
        }
        let title = front_matter
            .as_ref()
            .and_then(|fm| fm.title.as_deref())
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| name.to_string());
        (
            Some(CustomCommand {
                name: name.to_string(),
                title,
                model: model.map(ToOwned::to_owned),
                path: dir.to_path_buf(),
            }),
            messages,
        )
    }

    /// The command's prompt content: the `command.md` body without
    /// frontmatter, with every [`ARGUMENTS_PLACEHOLDER`] occurrence replaced
    /// by `args` (an empty string when no args were given); when the body has
    /// no placeholder and args are present, they are appended after a blank
    /// line.
    pub fn invocation_content(&self, args: Option<&str>) -> std::io::Result<String> {
        let content = std::fs::read_to_string(self.path.join(COMMAND_FILE_NAME))?;
        let body = command_body(&content);
        Ok(match args {
            Some(args) if body.contains(ARGUMENTS_PLACEHOLDER) => {
                body.replace(ARGUMENTS_PLACEHOLDER, args)
            }
            Some(args) => format!("{body}\n\n{args}"),
            None => body.replace(ARGUMENTS_PLACEHOLDER, ""),
        })
    }
}

/// Split a `model` spec (`<provider_type>/<model>`) at its first `/`; the
/// model id itself may contain further slashes.
pub fn parse_model_spec(spec: &str) -> Option<(&str, &str)> {
    let (type_name, model) = spec.split_once('/')?;
    let type_name = type_name.trim();
    let model = model.trim();
    if type_name.is_empty() || model.is_empty() {
        return None;
    }
    Some((type_name, model))
}

#[derive(Debug, Deserialize)]
struct FrontMatter {
    title: Option<String>,
    model: Option<String>,
}

/// Split `---\n<yaml>\n---\n<body>` into the YAML and the trimmed body.
pub(crate) fn front_matter_split(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    let end = rest.find("\n---")?;
    Some((&rest[..end], rest[end + 4..].trim()))
}

/// The `command.md` body without frontmatter; the whole trimmed content when
/// no frontmatter is present.
fn command_body(content: &str) -> &str {
    match front_matter_split(content) {
        Some((_, body)) => body,
        None => content.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_command(root: &Path, name: &str, contents: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(COMMAND_FILE_NAME), contents).unwrap();
    }

    #[test]
    fn loads_workspace_and_global_commands() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let global = dir.path().join("global");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "commit",
            "---\ntitle: Commit code\nmodel: anthropic/claude-x\n---\n\n# Commit\n\n{{arguments}}\n",
        );
        write_command(&global.join("commands"), "review", "# Review\n\nbody\n");

        let commands = CustomCommands::load_from(&workspace, Some(&global));
        let names: Vec<&str> = commands.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["commit", "review"]);
        let commit = &commands.commands[0];
        assert_eq!(commit.title, "Commit code");
        assert_eq!(commit.model.as_deref(), Some("anthropic/claude-x"));
        assert!(commands.warnings.is_empty());
    }

    #[test]
    fn workspace_wins_over_global_duplicate() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let global = dir.path().join("global");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "commit",
            "workspace body\n",
        );
        write_command(&global.join("commands"), "commit", "global body\n");
        write_command(&global.join("commands"), "other", "other body\n");

        let commands = CustomCommands::load_from(&workspace, Some(&global));
        let names: Vec<&str> = commands.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["commit", "other"]);
        assert_eq!(
            commands.commands[0].invocation_content(None).unwrap(),
            "workspace body"
        );
        assert_eq!(
            commands
                .warnings
                .iter()
                .filter(|w| w.message.contains("duplicate"))
                .count(),
            1
        );
    }

    #[test]
    fn no_commands_dir_is_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let commands = CustomCommands::load_from(dir.path(), None);
        assert!(commands.commands.is_empty());
        assert!(commands.warnings.is_empty());
    }

    #[test]
    fn missing_command_md_warns() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(workspace.join(WORKSPACE_DIR_NAME).join("commands/empty")).unwrap();
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "real",
            "body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let names: Vec<&str> = commands.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
        assert_eq!(commands.warnings.len(), 1);
        assert!(commands.warnings[0].message.contains("failed to read"));
    }

    #[test]
    fn invalid_name_warns_but_loads() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "Bad_Name",
            "body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        assert_eq!(commands.commands.len(), 1);
        assert!(
            commands
                .warnings
                .iter()
                .any(|w| w.message.contains("invalid characters"))
        );
    }

    #[test]
    fn unparsable_frontmatter_warns() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "broken",
            "---\n[not yaml\n---\nbody\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        assert!(commands.commands.is_empty());
        assert!(
            commands
                .warnings
                .iter()
                .any(|w| w.message.contains("frontmatter"))
        );
    }

    #[test]
    fn missing_frontmatter_uses_whole_file_as_body() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "plain",
            "just body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let command = &commands.commands[0];
        assert_eq!(command.title, "plain");
        assert_eq!(command.model, None);
        assert_eq!(command.invocation_content(None).unwrap(), "just body");
    }

    #[test]
    fn title_defaults_to_name_and_blank_model_is_none() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "cmd",
            "---\ntitle: \"  \"\nmodel: \"   \"\n---\nbody\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let command = &commands.commands[0];
        assert_eq!(command.title, "cmd");
        assert_eq!(command.model, None);
        assert!(commands.warnings.is_empty());
    }

    #[test]
    fn frontmatter_with_trailing_comment_parses() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "commit",
            concat!(
                "---\n",
                "title: Commit code\n",
                "model: openai/gpt-5.6-luna # <provider_type>/<model>; use current model if unset\n",
                "---\n\n",
                "# Commit code command\n\n",
                "Use `git commit -m '<message>'` to commit...\n\n",
                "{{arguments}}\n"
            ),
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let command = &commands.commands[0];
        assert_eq!(command.title, "Commit code");
        assert_eq!(command.model.as_deref(), Some("openai/gpt-5.6-luna"));
        assert!(commands.warnings.is_empty(), "{:?}", commands.warnings);
        assert_eq!(
            command.invocation_content(Some("tidy up")).unwrap(),
            "# Commit code command\n\nUse `git commit -m '<message>'` to commit...\n\ntidy up",
            "placeholder replaced by the args"
        );
    }

    #[test]
    fn invocation_replaces_placeholder_everywhere() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "cmd",
            "a {{arguments}} b\n{{arguments}}\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let command = &commands.commands[0];
        assert_eq!(command.invocation_content(Some("x")).unwrap(), "a x b\nx");
        assert_eq!(command.invocation_content(None).unwrap(), "a  b\n");
    }

    #[test]
    fn invocation_appends_args_without_placeholder() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "cmd",
            "body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let command = &commands.commands[0];
        assert_eq!(
            command.invocation_content(Some("the args")).unwrap(),
            "body\n\nthe args"
        );
        assert_eq!(command.invocation_content(None).unwrap(), "body");
    }

    #[test]
    fn parse_model_spec_splits_on_first_slash() {
        assert_eq!(
            parse_model_spec("openai/gpt-5.6-luna"),
            Some(("openai", "gpt-5.6-luna"))
        );
        assert_eq!(
            parse_model_spec("anthropic/claude/sonnet/4"),
            Some(("anthropic", "claude/sonnet/4"))
        );
        assert_eq!(parse_model_spec("nomodel"), None);
        assert_eq!(parse_model_spec("/model"), None);
        assert_eq!(parse_model_spec("provider/"), None);
        assert_eq!(
            parse_model_spec(" provider / model "),
            Some(("provider", "model"))
        );
    }

    #[test]
    fn hidden_and_junk_dirs_skipped() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            ".hidden",
            "body\n",
        );
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "node_modules",
            "body\n",
        );
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            "real",
            "body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        let names: Vec<&str> = commands.commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn name_exceeding_max_length_warns() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = dir.path().join("workspace");
        let long = "a".repeat(65);
        write_command(
            &workspace.join(WORKSPACE_DIR_NAME).join("commands"),
            &long,
            "body\n",
        );

        let commands = CustomCommands::load_from(&workspace, None);
        assert!(
            commands
                .warnings
                .iter()
                .any(|w| w.message.contains("exceeds 64 characters"))
        );
    }
}
