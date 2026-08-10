use clap::Parser;
use tokio::sync::mpsc::channel;

mod cli;
mod tui;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    let _args = cli::Cli::parse();

    let (cmd_tx, cmd_rx) = channel::<shuvarie_core::Command>(64);
    let (event_tx, event_rx) = channel::<shuvarie_core::Event>(64);
    let _core = tokio::spawn(shuvarie_core::run(
        shuvarie_core::Config::load()?,
        cmd_rx,
        event_tx,
    ));

    tui::run_tui(cmd_tx, event_rx).await?;
    Ok(())
}
