use clap::{
    builder::{
        styling::{AnsiColor, Style},
        Styles,
    },
    Parser,
};

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().bold())
    .usage(Style::new().bold())
    .literal(AnsiColor::Cyan.on_default())
    .placeholder(AnsiColor::White.on_default());

#[derive(Parser)]
#[command(about, version, styles = STYLES)]
pub struct Cli {
    /// Resume the most recent session on startup, opening the session screen.
    #[arg(short, long)]
    pub current: bool,
}
