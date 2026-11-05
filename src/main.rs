use clap::Parser;

mod cli;
mod run;
mod spawn;
mod trust;
mod tui;
mod update;

use crate::cli::show_resume_hint;
use crate::spawn::CoreSpawner;

#[cfg(any(target_env = "musl", feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: mimalloc_safe::MiMalloc = mimalloc_safe::MiMalloc;

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
        theme_set,
    }) = resolved
    else {
        return Ok(());
    };

    // The process-wide inputs are settled, so the core is spawned through the
    // same path a later tab takes: the store choice (`db { global-store }`), the
    // permission rules, and the config chain are resolved against a workspace
    // root, never against the process. Resolving trust first means a store is
    // still never created for a run that quits at the prompt.
    let transient = run_args.as_ref().is_some_and(|run| run.transient);
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let spawner = CoreSpawner::new(args.config.clone(), grants.clone(), transient);
    let startup = if let Some(id) = args.session {
        shuvarie_core::StartupSession::Session(id)
    } else if args.current {
        shuvarie_core::StartupSession::MostRecent
    } else {
        shuvarie_core::StartupSession::None
    };
    let core = spawner
        .spawn(&workspace_root, startup)
        .await
        .map_err(|e| color_eyre::eyre::eyre!(e))?;

    if let Some(run) = run_args {
        return run::run_cli(run, core).await;
    }

    let res = tui::run_tui(config, theme_set, spawner, core).await;
    let res = res?;

    if let Some(res_enum) = res {
        match res_enum {
            tui::TuiResponse::SessionSaved { session_id } => {
                show_resume_hint(session_id);
            }
        }
    }

    Ok(())
}
