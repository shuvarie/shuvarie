use clap::{
    Parser, Subcommand,
    builder::{
        Styles,
        styling::{AnsiColor, Style},
    },
};
use shuvarie_core::{Category, PermissionAnswer, TrustGrants};
use std::path::PathBuf;
use uuid::Uuid;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().bold())
    .usage(Style::new().bold())
    .literal(AnsiColor::Cyan.on_default())
    .placeholder(AnsiColor::White.on_default());

#[derive(Parser)]
#[command(about, version, styles = STYLES)]
pub struct Cli {
    /// Resume the most recent session
    #[arg(short, long)]
    pub current: bool,
    /// Resume a session
    #[arg(short, long, conflicts_with = "current", value_name = "UUID")]
    pub session: Option<uuid::Uuid>,
    /// Use a certain config file
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Run in a certain directory
    #[arg(value_name = "WORK_DIR")]
    pub dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<CliSubcommand>,
}

#[derive(Subcommand)]
pub enum CliSubcommand {
    /// Import session
    Import {
        /// Path to session file
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Export session
    Export {
        /// Path to session file
        #[arg(value_name = "DEST")]
        dest: String,
    },
    /// Update to the latest release
    Update,
    /// Run a prompt without launching the TUI
    Run(RunArgs),
}

/// Arguments for the headless `run` subcommand.
#[derive(clap::Args)]
pub struct RunArgs {
    /// The prompt to run
    #[arg(value_name = "PROMPT", required = true, num_args = 1..)]
    pub prompt: Vec<String>,
    /// How permission asks are handled
    #[arg(
        long,
        value_enum,
        default_value_t = PermissionHandle::AllowAsk,
        value_name = "MODE"
    )]
    pub permission_handle: PermissionHandle,
    /// Use an in-memory DB for the session, making it transient
    #[arg(short = 't', long)]
    pub transient: bool,
    /// Workspace-trust grants for this run
    #[arg(long, value_name = "SPEC")]
    pub trust: Option<TrustSpec>,
}

/// How `run` answers a tool's `ask` permission gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PermissionHandle {
    /// Deny the ask, cutting the turn.
    DenyAsk,
    /// Allow the single call.
    AllowAsk,
    /// Allow the call and remember its scope for the rest of the run.
    AllowAll,
}

impl PermissionHandle {
    /// The answer to send for a permission ask. `allow_dir` reports whether
    /// the ask is a path ask that can additionally be widened to its folder.
    pub fn decision(self, allow_dir: bool) -> PermissionAnswer {
        match self {
            Self::DenyAsk => PermissionAnswer::Deny,
            Self::AllowAsk => PermissionAnswer::Allow,
            Self::AllowAll if allow_dir => PermissionAnswer::AllowDirSession,
            Self::AllowAll => PermissionAnswer::AllowSession,
        }
    }
}

/// The workspace-trust grants a headless `run` starts under, from `--trust`.
/// A CLI-declared decision applies to this invocation only: nothing is
/// recorded in `trusts.kdl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustSpec {
    /// Trust everything (`--trust all`).
    All,
    /// Trust nothing (`--trust none`).
    None,
    /// Trust exactly the named categories (`--trust contexts,skills`).
    Categories(Vec<Category>),
}

impl TrustSpec {
    pub fn grants(&self) -> TrustGrants {
        match self {
            Self::All => TrustGrants::all(),
            Self::None => TrustGrants::none(),
            Self::Categories(categories) => {
                TrustGrants::from_categories(categories.iter().copied())
            }
        }
    }
}

impl std::str::FromStr for TrustSpec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "all" => return Ok(Self::All),
            "none" => return Ok(Self::None),
            "" => return Err("empty trust spec".to_string()),
            _ => {}
        }
        let mut categories = Vec::new();
        for part in value.split(',') {
            let part = part.trim();
            match Category::parse(part) {
                Some(category) => categories.push(category),
                None => {
                    return Err(format!(
                        "unknown trust category '{part}' (expected contexts, skills, or configs)"
                    ));
                }
            }
        }
        Ok(Self::Categories(categories))
    }
}

/// The lines of the resume hint, shared by the TUI/import (stdout) and the
/// headless `run` (stderr, so its stdout stays pipeable).
pub fn resume_hint_lines(session_id: Uuid) -> [String; 4] {
    [
        String::new(),
        "This session can be reopened with:".to_string(),
        String::new(),
        format!("  shuvarie -s {session_id}"),
    ]
}

pub fn show_resume_hint(session_id: Uuid) {
    for line in resume_hint_lines(session_id) {
        println!("{line}");
    }
}

pub async fn init_default_store() -> shuvarie_db::Result<shuvarie_db::Store> {
    shuvarie_db::Store::open(&shuvarie_db::Store::default_path()).await
}

/// Opens the store the CLI subcommands act on: the config's `db.global-store`
/// decides between the shared global store and the workspace-local one. The
/// config loads non-interactively — no trust prompt runs here — from the
/// explicit file when given, else from the usual chain.
async fn open_session_store(
    config_path: Option<&PathBuf>,
) -> color_eyre::Result<shuvarie_db::Store> {
    let config = match config_path {
        Some(path) => shuvarie_core::Config::load_explicit(path)?,
        None => shuvarie_core::Config::load()?,
    };
    let store = if config.db.global_store {
        shuvarie_db::Store::open_global().await?
    } else {
        init_default_store().await?
    };
    Ok(store)
}

pub async fn import_session(
    path: &std::path::Path,
    config_path: Option<&PathBuf>,
) -> color_eyre::Result<()> {
    let mut store = open_session_store(config_path).await?;

    let json = std::fs::read_to_string(path)
        .map_err(|e| color_eyre::eyre::eyre!("read {}: {e}", path.display()))?;
    let file = shuvarie_db::SessionFile::from_json(&json)?;
    let id = store.import_session(&file).await?;
    println!("Imported session \"{}\"", file.session.title);
    show_resume_hint(id);
    Ok(())
}

pub async fn export_session(
    dest: &str,
    session: Option<Uuid>,
    config_path: Option<&PathBuf>,
) -> color_eyre::Result<()> {
    let mut store = open_session_store(config_path).await?;

    let stored = match session {
        Some(id) => Some(store.load_session(id).await?),
        None => store.most_recent_session().await?,
    };
    let Some(stored) = stored else {
        return Err(color_eyre::eyre::eyre!("no sessions to export"));
    };
    let explicit = (!dest.is_empty()).then(|| PathBuf::from(dest));
    let path = shuvarie_db::session_file::resolve_export_path(explicit.as_deref(), stored.id);
    let mut file = shuvarie_db::SessionFile::from_stored(&stored);
    store.hydrate_attachment_blobs(&mut file).await?;
    file.write_json(&path)?;
    println!(
        "Session \"{}\" exported to {}",
        stored.title,
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn run_args(args: &[&str]) -> RunArgs {
        let cli = Cli::try_parse_from(args).unwrap();
        match cli.command {
            Some(CliSubcommand::Run(run)) => run,
            _ => panic!("expected the run subcommand"),
        }
    }

    #[test]
    fn run_joins_the_prompt_words() {
        let run = run_args(&["shuvarie", "run", "fix", "the", "failing", "test"]);
        assert_eq!(run.prompt, ["fix", "the", "failing", "test"]);
        assert_eq!(run.permission_handle, PermissionHandle::AllowAsk);
        assert!(!run.transient);
        assert!(run.trust.is_none());
    }

    #[test]
    fn run_parses_its_flags() {
        let run = run_args(&[
            "shuvarie",
            "run",
            "-t",
            "--permission-handle",
            "deny-ask",
            "--trust",
            "contexts,skills",
            "hi",
        ]);
        assert!(run.transient);
        assert_eq!(run.permission_handle, PermissionHandle::DenyAsk);
        assert_eq!(
            run.trust,
            Some(TrustSpec::Categories(vec![
                Category::Contexts,
                Category::Skills
            ]))
        );
    }

    #[test]
    fn allow_all_is_kebab_case() {
        let run = run_args(&["shuvarie", "run", "--permission-handle", "allow-all", "x"]);
        assert_eq!(run.permission_handle, PermissionHandle::AllowAll);
    }

    #[test]
    fn run_requires_a_prompt() {
        assert!(Cli::try_parse_from(["shuvarie", "run"]).is_err());
    }

    #[test]
    fn trust_spec_parses_all_none_and_categories() {
        assert_eq!("all".parse::<TrustSpec>().unwrap(), TrustSpec::All);
        assert_eq!("none".parse::<TrustSpec>().unwrap(), TrustSpec::None);
        assert_eq!(
            "contexts, configs".parse::<TrustSpec>().unwrap(),
            TrustSpec::Categories(vec![Category::Contexts, Category::Configs])
        );
        assert!("bogus".parse::<TrustSpec>().is_err());
        assert!("".parse::<TrustSpec>().is_err());
    }

    #[test]
    fn trust_spec_grants_match_the_categories() {
        assert_eq!(TrustSpec::All.grants(), TrustGrants::all());
        assert_eq!(TrustSpec::None.grants(), TrustGrants::none());
        let partial = TrustSpec::Categories(vec![Category::Contexts]).grants();
        assert!(partial.allows(Category::Contexts));
        assert!(!partial.allows(Category::Skills));
    }

    #[test]
    fn resume_hint_lines_carry_the_session_id() {
        let lines = resume_hint_lines(Uuid::nil());
        assert_eq!(lines[1], "This session can be reopened with:");
        assert_eq!(
            lines[3],
            "  shuvarie -s 00000000-0000-0000-0000-000000000000"
        );
    }
}
