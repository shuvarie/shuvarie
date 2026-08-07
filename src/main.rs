use clap::Parser;

mod cli;
mod tui;

fn main() -> color_eyre::Result<()> {
    let _args = cli::Cli::parse();

    tui::run_tui()?;
    Ok(())
}
