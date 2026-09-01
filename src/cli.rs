use clap::{
    builder::{
        styling::{AnsiColor, Style},
        Styles,
    },
    Parser,
};
use std::path::PathBuf;

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
    /// Resume the session with the given UUID on startup, opening the session
    /// screen. The UUID is printed when exiting a session.
    #[arg(short = 's', long, conflicts_with = "current", value_name = "UUID")]
    pub session: Option<uuid::Uuid>,
    /// Use only this config file, bypassing the config search path. Missing
    /// files hard-error, and config saves go to this file.
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}
