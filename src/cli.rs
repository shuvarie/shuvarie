use clap::Parser;

#[derive(Parser)]
pub struct Cli {
    /// Open the most recent session on startup instead of the home screen.
    #[arg(short, long)]
    pub current: bool,
}
