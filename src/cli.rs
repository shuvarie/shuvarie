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
    /// Open the most recent session on startup instead of the home screen.
    #[arg(short, long)]
    pub current: bool,
}
