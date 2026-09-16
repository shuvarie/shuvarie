use clap::Parser;
use tokio::sync::mpsc::channel;

mod cli;
mod trust;
mod tui;

use crate::cli::show_resume_hint;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = cli::Cli::parse();

    if let Some(dir) = args.dir.as_deref() {
        std::env::set_current_dir(dir).map_err(|e| {
            color_eyre::eyre::eyre!("failed to set working directory to {}: {e}", dir.display())
        })?;
    }

    let store = shuvarie_db::Store::open(&shuvarie_db::Store::default_path()).await?;

    let Some(trust::Resolved {
        config,
        grants,
        scenes,
        theme,
    }) = trust::resolve(args.config.as_ref()).await?
    else {
        return Ok(());
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

    let res = tui::run_tui(config, theme, cmd_tx.clone(), event_rx).await;
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
