//! `shuvarie mcp …`: add, list, and remove MCP servers.
//!
//! A mutation writes exactly one file — the `--config` file when given, the
//! global config under `--global`, else this workspace's local config — and
//! loads that file on its own rather than as part of the priority chain. The
//! other layers' sections are therefore never copied into it, and the servers
//! a workspace does not define keep coming from the global config.

use clap::Subcommand;
use color_eyre::eyre::bail;
use shuvarie_core::{
    Config, EnvsConfig, McpStdioConfig, McpTransport, valid_env_var_name, valid_mcp_server_name,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `shuvarie mcp <COMMAND>`.
#[derive(clap::Args)]
pub struct McpArgs {
    #[command(subcommand)]
    pub command: McpSubcommand,
}

#[derive(Subcommand)]
pub enum McpSubcommand {
    /// Add an MCP server to a config file
    Add(McpAddArgs),
    /// List the MCP servers the config declares (nothing is connected)
    List,
    /// Remove an MCP server from a config file
    Remove(McpRemoveArgs),
}

/// `shuvarie mcp add [OPTIONS] <TRANSPORT>`.
#[derive(clap::Args)]
pub struct McpAddArgs {
    /// Write to the global config instead of this workspace's
    #[arg(long, global = true)]
    pub global: bool,
    /// Replace an existing server that has the same name
    #[arg(long, global = true)]
    pub force: bool,

    #[command(subcommand)]
    pub transport: McpAddTransport,
}

/// The transport of the server to add. Only `stdio` is offered today; an
/// `http` variant slots in beside it without touching the flags above.
#[derive(Subcommand)]
pub enum McpAddTransport {
    /// A server spawned as a child process (`tools { mcp { stdio … } }`)
    Stdio(McpAddStdioArgs),
}

/// `shuvarie mcp add stdio <NAME> --command <CMD> [--arg …] [--env NAME=VALUE]`.
#[derive(clap::Args)]
pub struct McpAddStdioArgs {
    /// The server name; its tools are named mcp__<NAME>__<tool>
    #[arg(value_name = "NAME")]
    pub name: String,
    /// The executable to spawn
    #[arg(long, value_name = "CMD")]
    pub command: String,
    /// An argument passed to the command (repeatable)
    #[arg(long = "arg", value_name = "ARG", allow_hyphen_values = true)]
    pub args: Vec<String>,
    /// An environment variable for the server, as NAME=VALUE (repeatable).
    /// The value may carry $VAR / ${VAR} placeholders, resolved when the
    /// server is spawned.
    #[arg(long = "env", value_name = "NAME=VALUE")]
    pub envs: Vec<String>,
    /// Do not pass this process's environment through to the server
    #[arg(long)]
    pub no_inherit_env: bool,
}

/// `shuvarie mcp remove <NAME>`.
#[derive(clap::Args)]
pub struct McpRemoveArgs {
    /// The server name to remove
    #[arg(value_name = "NAME")]
    pub name: String,
    /// Remove from the global config instead of this workspace's
    #[arg(long)]
    pub global: bool,
}

/// Runs the `mcp` subcommand. `config` is the top-level `--config` file, which
/// wins over both the global and the workspace config.
pub fn run(args: McpArgs, config: Option<&PathBuf>) -> color_eyre::Result<()> {
    match args.command {
        McpSubcommand::List => list(config),
        McpSubcommand::Add(McpAddArgs {
            global,
            force,
            transport,
        }) => match transport {
            McpAddTransport::Stdio(stdio) => add_stdio(&stdio, global, force, config),
        },
        McpSubcommand::Remove(remove) => remove_server(&remove, config),
    }
}

/// Prints the servers the config resolves to: the merged priority chain, or
/// the single `--config` file.
fn list(config: Option<&PathBuf>) -> color_eyre::Result<()> {
    let config = load_effective(config)?;
    let mcp = &config.tools.mcp;
    if mcp.is_empty() {
        println!("No MCP servers are configured.");
        println!("Add one with `shuvarie mcp add stdio <NAME> --command <CMD>`.");
        return Ok(());
    }

    let servers = mcp.servers();
    let width = servers
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or_default();
    println!("Configured MCP servers:");
    for (name, transport) in servers {
        let detail = match transport {
            McpTransport::Stdio => mcp.stdio.get(name).map(command_line).unwrap_or_default(),
            McpTransport::Http => mcp
                .http
                .get(name)
                .map(|server| server.url.clone())
                .unwrap_or_default(),
        };
        println!("  {name:<width$}  {:<5}  {detail}", transport.as_str());
    }
    Ok(())
}

/// The command line of a `stdio` server, as the config declares it.
fn command_line(server: &McpStdioConfig) -> String {
    std::iter::once(server.command.as_str())
        .chain(server.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The config a read acts on: the single `--config` file (which must exist),
/// else the merged priority chain.
fn load_effective(config: Option<&PathBuf>) -> color_eyre::Result<Config> {
    match config {
        Some(path) => Ok(Config::load_explicit(path)?),
        None => Ok(Config::load()?),
    }
}

/// The file a mutation writes: `--config` when given, the global config under
/// `--global`, else this workspace's local config.
fn target_file(config: Option<&PathBuf>, global: bool) -> color_eyre::Result<PathBuf> {
    if let Some(path) = config {
        return Ok(path.clone());
    }
    if global {
        return Ok(Config::config_path()?);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Ok(local_target(&cwd))
}

/// The workspace-local file: the local candidate that already exists takes
/// precedence, highest priority first. Writing a second local layer would
/// shadow the first one's `tools` section, since a chain merge takes a section
/// wholesale from the highest-priority file that defines it (in debug builds
/// the candidate names carry the `-dev` suffix).
fn local_target(cwd: &Path) -> PathBuf {
    let candidates = Config::local_config_candidates(cwd);
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// Parses `--env NAME=VALUE` pairs. `inherit` stays on unless
/// `--no-inherit-env` says otherwise; a repeated `NAME` keeps the last value.
fn env_spec(pairs: &[String], inherit: bool) -> color_eyre::Result<EnvsConfig> {
    let mut entries = BTreeMap::new();
    for pair in pairs {
        let Some((name, value)) = pair.split_once('=') else {
            bail!("`--env {pair}` is not NAME=VALUE");
        };
        let name = name.trim();
        if !valid_env_var_name(name) {
            bail!("`{name}` is not a valid environment variable name");
        }
        entries.insert(name.to_string(), value.to_string());
    }
    Ok(EnvsConfig { inherit, entries })
}

fn add_stdio(
    args: &McpAddStdioArgs,
    global: bool,
    force: bool,
    config: Option<&PathBuf>,
) -> color_eyre::Result<()> {
    if !valid_mcp_server_name(&args.name) {
        bail!(
            "`{}` is not a valid server name: use only letters, digits, `_`, and `-`",
            args.name
        );
    }
    if args.command.trim().is_empty() {
        bail!("`--command` must name an executable");
    }
    let envs = env_spec(&args.envs, !args.no_inherit_env)?;

    let path = target_file(config, global)?;
    let mut config = Config::load_from(&path)?;
    if let Some(transport) = config.tools.mcp.transport_of(&args.name) {
        if !force {
            bail!(
                "an MCP {} server named `{}` is already configured in {} (pass `--force` to replace it)",
                transport.as_str(),
                args.name,
                path.display()
            );
        }
        // The transports share one name space, so the old entry goes first.
        config.tools.mcp.remove(&args.name);
    }

    config.tools.mcp.insert_stdio(
        args.name.clone(),
        McpStdioConfig {
            command: args.command.clone(),
            args: args.args.clone(),
            envs,
        },
    );
    config.save_to(&path)?;
    println!(
        "Added stdio MCP server \"{}\" to {}",
        args.name,
        path.display()
    );
    println!("Restart shuvarie (or open a new tab) to connect it.");
    Ok(())
}

fn remove_server(args: &McpRemoveArgs, config: Option<&PathBuf>) -> color_eyre::Result<()> {
    let path = target_file(config, args.global)?;
    let mut config = Config::load_from(&path)?;
    let Some(transport) = config.tools.mcp.remove(&args.name) else {
        bail!(
            "no MCP server named `{}` in {}; `shuvarie mcp list` shows every server \
             the config layers add up to",
            args.name,
            path.display()
        );
    };
    config.save_to(&path)?;
    println!(
        "Removed {} MCP server \"{}\" from {}",
        transport.as_str(),
        args.name,
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Just the `mcp` subcommand, so the parse tests do not drag the root CLI
    /// (and its defaults) in.
    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        command: McpSubcommand,
    }

    fn parse(args: &[&str]) -> McpSubcommand {
        Wrapper::try_parse_from(args).unwrap().command
    }

    fn add_stdout_args(
        name: &str,
        command: &str,
        args: &[&str],
        envs: &[&str],
        no_inherit_env: bool,
    ) -> McpAddStdioArgs {
        McpAddStdioArgs {
            name: name.to_string(),
            command: command.to_string(),
            args: args.iter().map(|arg| arg.to_string()).collect(),
            envs: envs.iter().map(|env| env.to_string()).collect(),
            no_inherit_env,
        }
    }

    #[test]
    fn add_stdio_parses_its_flags() {
        let McpSubcommand::Add(add) = parse(&[
            "shuvarie",
            "add",
            "--global",
            "--force",
            "stdio",
            "github",
            "--command",
            "npx",
            "--arg",
            "-y",
            "--arg",
            "@modelcontextprotocol/server-github",
            "--env",
            "GITHUB_TOKEN=$GITHUB_TOKEN",
            "--no-inherit-env",
        ]) else {
            panic!("expected add");
        };
        assert!(add.global);
        assert!(add.force);
        let McpAddTransport::Stdio(stdio) = add.transport;
        assert_eq!(stdio.name, "github");
        assert_eq!(stdio.command, "npx");
        assert_eq!(
            stdio.args,
            ["-y", "@modelcontextprotocol/server-github"],
            "a hyphenated arg must not be read as a flag"
        );
        assert_eq!(stdio.envs, ["GITHUB_TOKEN=$GITHUB_TOKEN"]);
        assert!(stdio.no_inherit_env);
    }

    #[test]
    fn add_stdio_defaults() {
        let McpSubcommand::Add(add) =
            parse(&["shuvarie", "add", "stdio", "srv", "--command", "srv-mcp"])
        else {
            panic!("expected add");
        };
        assert!(!add.global);
        assert!(!add.force);
        let McpAddTransport::Stdio(stdio) = add.transport;
        assert!(stdio.args.is_empty());
        assert!(stdio.envs.is_empty());
        assert!(!stdio.no_inherit_env);
    }

    #[test]
    fn add_stdio_requires_a_name_and_a_command() {
        assert!(Wrapper::try_parse_from(["shuvarie", "add", "stdio"]).is_err());
        assert!(
            Wrapper::try_parse_from(["shuvarie", "add", "stdio", "srv"]).is_err(),
            "the command is required"
        );
    }

    #[test]
    fn remove_and_list_parse() {
        let McpSubcommand::Remove(remove) = parse(&["shuvarie", "remove", "github", "--global"])
        else {
            panic!("expected remove");
        };
        assert_eq!(remove.name, "github");
        assert!(remove.global);
        assert!(matches!(parse(&["shuvarie", "list"]), McpSubcommand::List));
    }

    #[test]
    fn env_spec_splits_name_value_pairs() {
        let envs = env_spec(
            &[
                "GITHUB_TOKEN=$GITHUB_TOKEN".to_string(),
                "EMPTY=".to_string(),
                "KEEP=a=b".to_string(),
            ],
            false,
        )
        .unwrap();
        assert!(!envs.inherit);
        assert_eq!(envs.entries.len(), 3);
        assert_eq!(envs.entries["GITHUB_TOKEN"], "$GITHUB_TOKEN");
        assert_eq!(envs.entries["EMPTY"], "");
        assert_eq!(envs.entries["KEEP"], "a=b");

        assert!(env_spec(&["novalue".to_string()], true).is_err());
        assert!(env_spec(&["1BAD=x".to_string()], true).is_err());
        assert!(env_spec(&["A-B=x".to_string()], true).is_err());
    }

    #[test]
    fn local_target_prefers_the_existing_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let [root, workspace] = Config::local_config_candidates(dir.path());
        assert_eq!(local_target(dir.path()), root, "nothing exists yet");

        std::fs::create_dir_all(workspace.parent().unwrap()).unwrap();
        std::fs::write(&workspace, "").unwrap();
        assert_eq!(local_target(dir.path()), workspace, "only the nested one");

        std::fs::write(&root, "").unwrap();
        assert_eq!(
            local_target(dir.path()),
            root,
            "the higher-priority file wins when both exist"
        );
    }

    #[test]
    fn add_stdio_writes_the_server_and_keeps_the_other_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        std::fs::write(&path, "ui { frame-rate 30 }\n").unwrap();

        let args = add_stdout_args(
            "github",
            "npx",
            &["-y", "@modelcontextprotocol/server-github"],
            &["GITHUB_TOKEN=$GITHUB_TOKEN"],
            true,
        );
        add_stdio(&args, false, false, Some(&path)).unwrap();

        let parsed = Config::load_from(&path).unwrap();
        assert_eq!(parsed.ui.frame_rate, 30, "the file's own sections stay");
        let server = parsed.tools.mcp.stdio.get("github").expect("github");
        assert_eq!(server.command, "npx");
        assert_eq!(server.args, ["-y", "@modelcontextprotocol/server-github"]);
        assert!(!server.envs.inherit);
        assert_eq!(server.envs.entries["GITHUB_TOKEN"], "$GITHUB_TOKEN");
    }

    #[test]
    fn add_stdio_needs_force_to_replace_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        std::fs::write(
            &path,
            r#"tools { mcp { http name="github" { url "https://example.test/mcp" } } }"#,
        )
        .unwrap();
        let args = add_stdout_args("github", "npx", &[], &[], false);

        let error = add_stdio(&args, false, false, Some(&path)).unwrap_err();
        assert!(
            error.to_string().contains("already configured"),
            "error: {error}"
        );
        // The refused add left the file alone.
        let parsed = Config::load_from(&path).unwrap();
        assert_eq!(
            parsed.tools.mcp.transport_of("github"),
            Some(McpTransport::Http)
        );

        // Forcing replaces the `http` entry rather than leaving both, which
        // would be a duplicate name the parser rejects on the next load.
        add_stdio(&args, false, true, Some(&path)).unwrap();
        let parsed = Config::load_from(&path).unwrap();
        assert_eq!(
            parsed.tools.mcp.transport_of("github"),
            Some(McpTransport::Stdio)
        );
        assert_eq!(parsed.tools.mcp.len(), 1);
    }

    #[test]
    fn add_stdio_rejects_an_invalid_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        let args = add_stdout_args("bad name", "npx", &[], &[], false);
        assert!(add_stdio(&args, false, false, Some(&path)).is_err());
        assert!(!path.exists(), "a rejected add writes nothing");
    }

    #[test]
    fn remove_server_takes_the_entry_out() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        let args = add_stdout_args("github", "npx", &[], &[], false);
        add_stdio(&args, false, false, Some(&path)).unwrap();

        let remove = McpRemoveArgs {
            name: "github".to_string(),
            global: false,
        };
        remove_server(&remove, Some(&path)).unwrap();
        let parsed = Config::load_from(&path).unwrap();
        assert!(parsed.tools.mcp.is_empty());

        let error = remove_server(&remove, Some(&path)).unwrap_err();
        assert!(error.to_string().contains("no MCP server named"), "{error}");
    }
}
