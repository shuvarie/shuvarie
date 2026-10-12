use clap::Parser;
#[cfg(target_env = "musl")]
use mimalloc_safe::MiMalloc;
use tokio::sync::mpsc::channel;

mod cli;
mod run;
mod trust;
mod tui;
mod update;

use crate::cli::{init_default_store, show_resume_hint};

#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let mut args = cli::Cli::parse();

    if let Some(dir) = args.dir.as_deref() {
        std::env::set_current_dir(dir).map_err(|e| {
            color_eyre::eyre::eyre!("failed to set working directory to {}: {e}", dir.display())
        })?;
    }

    // The file-handling subcommands act on their own; `run` takes its args out
    // and falls through to the shared setup below, since it needs the resolved
    // config, trust, and a store exactly like the TUI does.
    let run_args = match args.command.take() {
        Some(cli::CliSubcommand::Run(run)) => Some(run),
        Some(cli::CliSubcommand::Import { file }) => {
            return cli::import_session(&file, args.config.as_ref()).await;
        }
        Some(cli::CliSubcommand::Export { dest }) => {
            return cli::export_session(&dest, args.session, args.config.as_ref()).await;
        }
        Some(cli::CliSubcommand::Update) => return update::run().await,
        None => None,
    };

    // An in-memory DB has no sessions to resume, so combining the two is a
    // usage error rather than a silent no-op.
    if let Some(run) = &run_args
        && run.transient
        && (args.session.is_some() || args.current)
    {
        return Err(color_eyre::eyre::eyre!(
            "--transient uses an in-memory DB, which has no session to resume; \
             drop --transient or the -s/-c/--current flag"
        ));
    }

    // Headless runs never prompt for trust: their grants come from `--trust`,
    // else from this workspace's recorded decision (or none).
    let resolved = match &run_args {
        Some(run) => {
            let grants = match &run.trust {
                Some(spec) => spec.grants(),
                None => trust::default_run_grants()?,
            };
            Some(trust::resolve_with_grants(args.config.as_ref(), grants)?)
        }
        None => trust::resolve(args.config.as_ref()).await?,
    };
    let Some(trust::Resolved {
        config,
        grants,
        scenes,
        theme_set,
    }) = resolved
    else {
        return Ok(());
    };

    // The store choice follows the config: the shared global data dir when
    // `db { global-store }` is set, else the workspace-local DB. Resolving
    // first means a store is never created for a run that quits at the trust
    // prompt. `--transient` overrides both with an in-memory DB.
    let transient = run_args.as_ref().is_some_and(|run| run.transient);
    let store = if transient {
        shuvarie_db::Store::open_in_memory().await?
    } else if config.db.global_store {
        shuvarie_db::Store::open_global().await?
    } else {
        init_default_store().await?
    };

    let (cmd_tx, cmd_rx) = channel::<shuvarie_core::Command>(64);
    let (event_tx, event_rx) = channel::<shuvarie_core::Event>(64);
    let permissions = std::sync::Arc::new(
        shuvarie_core::permissions::Permissions::build(
            &config.permissions,
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        )
        .map_err(|e| color_eyre::eyre::eyre!(e))?,
    );
    let startup = if let Some(id) = args.session {
        shuvarie_core::StartupSession::Session(id)
    } else if args.current {
        shuvarie_core::StartupSession::MostRecent
    } else {
        shuvarie_core::StartupSession::None
    };
    let core = tokio::spawn(shuvarie_core::run(
        config.clone(),
        shuvarie_core::Connections::load()?,
        store,
        startup,
        args.config,
        None,
        permissions,
        grants,
        scenes,
        cmd_rx,
        event_tx,
    ));

    if let Some(run) = run_args {
        return run::run_cli(run, cmd_tx, event_rx, core).await;
    }

    let res = tui::run_tui(config, theme_set, cmd_tx.clone(), event_rx).await;
    drop(cmd_tx);
    let core_res = core.await;

    let res = res?;
    core_res?;

    if let Some(res_enum) = res {
        match res_enum {
            tui::TuiResponse::SessionSaved { session_id } => {
                show_resume_hint(session_id);
            }
        }
    }

    Ok(())
}
