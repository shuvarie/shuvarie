use clap::Parser;
use tokio::sync::mpsc::channel;

mod cli;
mod tui;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let args = cli::Cli::parse();

    let store = shuvarie_db::Store::open(&shuvarie_db::Store::default_path()).await?;

    let (cmd_tx, cmd_rx) = channel::<shuvarie_core::Command>(64);
    let (event_tx, event_rx) = channel::<shuvarie_core::Event>(64);
    let core = tokio::spawn(shuvarie_core::run(
        shuvarie_core::Config::load()?,
        store,
        args.current,
        None,
        cmd_rx,
        event_tx,
    ));

    let res = tui::run_tui(cmd_tx.clone(), event_rx).await;
    drop(cmd_tx);
    let core_res = core.await;

    res?;
    core_res?;
    Ok(())
}
