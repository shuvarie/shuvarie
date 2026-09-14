use clap::Parser;
use tokio::sync::mpsc::channel;

use crate::cli::show_resume_hint;

mod cli;
mod tui;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = cli::Cli::parse();

    let store = shuvarie_db::Store::open(&shuvarie_db::Store::default_path()).await?;

    let (cmd_tx, cmd_rx) = channel::<shuvarie_core::Command>(64);
    let (event_tx, event_rx) = channel::<shuvarie_core::Event>(64);
    let config = match args.config.as_deref() {
        Some(path) => shuvarie_core::Config::load_explicit(path)?,
        None => shuvarie_core::Config::load()?,
    };
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
        cmd_rx,
        event_tx,
    ));

    let res = tui::run_tui(config, cmd_tx.clone(), event_rx).await;
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
