//! `shuvarie provider …`: register the providers this environment holds keys
//! for.
//!
//! A provider's catalog entry names the variable its key is read from
//! (`$OPENAI_API_KEY`), so an exported variable is all a connection needs — the
//! secret never lands in `connections.kdl`. `scan` lists what the environment
//! offers and adds it, confirming first unless `--yes` says not to. The TUI's
//! `/scan-providers` runs the same scan and the same registration, so the two
//! cannot drift.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

use clap::Subcommand;
use shuvarie_core::{Config, Connections};

/// `shuvarie provider <COMMAND>`.
#[derive(clap::Args)]
pub struct ProviderArgs {
    #[command(subcommand)]
    pub command: ProviderSubcommand,
}

#[derive(Subcommand)]
pub enum ProviderSubcommand {
    /// Add every provider whose API-key variable is set and unregistered
    Scan(ProviderScanArgs),
}

/// `shuvarie provider scan [--yes]`.
#[derive(clap::Args)]
pub struct ProviderScanArgs {
    /// Add the detected providers without asking
    #[arg(short = 'y', long)]
    pub yes: bool,
}

/// Runs the `provider` subcommand. `config` is the top-level `--config` file,
/// which wins over the discovered config chain — it decides which registries
/// the scan reads, not which connections the run writes.
pub fn run(args: ProviderArgs, config: Option<&PathBuf>) -> color_eyre::Result<()> {
    match args.command {
        ProviderSubcommand::Scan(scan) => scan_env(&scan, config),
    }
}

/// The `scan` command: report the unregistered providers the environment holds
/// keys for, confirm, then write them to `connections.kdl`.
fn scan_env(args: &ProviderScanArgs, config: Option<&PathBuf>) -> color_eyre::Result<()> {
    // The catalog is process-global and the scan reads it, so the configured
    // registries have to be registered first. The core does that at startup; a
    // CLI invocation has no core, so it does it here.
    let config = match config {
        Some(path) => Config::load_explicit(path)?,
        None => Config::load()?,
    };
    shuvarie_core::catalog::init(&config.registries);

    let mut connections = Connections::load()?;
    let detected = shuvarie_core::provider_env::scan(&connections);
    if detected.is_empty() {
        println!("No unregistered provider API key is set in this environment.");
        return Ok(());
    }

    let width = detected
        .iter()
        .map(|provider| provider.name.chars().count())
        .max()
        .unwrap_or_default();
    println!(
        "Found {} provider API key{} in this environment:",
        detected.len(),
        if detected.len() == 1 { "" } else { "s" }
    );
    for provider in &detected {
        println!("  {:<width$}  ${}", provider.name, provider.env);
    }

    if !args.yes {
        // A pipe or a file has no one to answer the prompt, and an unanswered
        // prompt must never be read as consent: say what would have been added
        // and leave the file alone.
        if !io::stdin().is_terminal() {
            println!("Nothing added: stdin is not a terminal. Pass `--yes` to add them.");
            return Ok(());
        }
        if !confirmed()? {
            println!("Nothing added.");
            return Ok(());
        }
    }

    let was_active = connections.active.is_some();
    let rows = shuvarie_core::provider_env::plan(&detected);
    shuvarie_core::provider_env::apply(&mut connections, &rows);
    connections.save()?;

    println!(
        "Added {} provider{} to {}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" },
        Connections::connections_path()?.display()
    );
    // The connections carry the catalog's key names, so nothing was copied out
    // of the environment — only pointed at.
    if !was_active && let Some((_, provider)) = rows.first() {
        println!(
            "\"{}\" is now the active provider. Pick its model in the TUI with `/model`.",
            provider.name
        );
    }
    Ok(())
}

/// Ask whether to add the detected providers. The prompt defaults to yes, so an
/// empty line (or `y`/`yes` in any case) proceeds.
fn confirmed() -> color_eyre::Result<bool> {
    print!("Add them? [Y/n] ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(accepts(&answer))
}

/// Whether an answer to the confirmation prompt means "add them".
fn accepts(answer: &str) -> bool {
    matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "" | "y" | "yes"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Just the `provider` subcommand, so the parse tests do not drag the root
    /// CLI (and its defaults) in.
    #[derive(Parser)]
    struct Wrapper {
        #[command(subcommand)]
        command: ProviderSubcommand,
    }

    fn parse(args: &[&str]) -> ProviderSubcommand {
        Wrapper::try_parse_from(args).unwrap().command
    }

    #[test]
    fn scan_confirms_by_default() {
        match parse(&["shuvarie", "scan"]) {
            ProviderSubcommand::Scan(scan) => assert!(!scan.yes),
        }
    }

    #[test]
    fn scan_takes_the_short_and_long_yes_flags() {
        for flag in ["-y", "--yes"] {
            match parse(&["shuvarie", "scan", flag]) {
                ProviderSubcommand::Scan(scan) => assert!(scan.yes, "{flag}"),
            }
        }
    }

    #[test]
    fn an_empty_answer_takes_the_default() {
        assert!(accepts(""));
        assert!(accepts("\n"));
        assert!(accepts("y"));
        assert!(accepts(" YES \n"));
        assert!(!accepts("n"));
        assert!(!accepts("no"));
        assert!(!accepts("nope"));
    }
}
