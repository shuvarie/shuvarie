use clap::{
    Parser,
    builder::{
        Styles,
        styling::{AnsiColor, Style},
    },
};
use std::path::PathBuf;
use uuid::Uuid;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().bold())
    .usage(Style::new().bold())
    .literal(AnsiColor::Cyan.on_default())
    .placeholder(AnsiColor::White.on_default());

#[derive(Parser)]
#[command(about, version, styles = STYLES)]
pub struct Cli {
    /// Resume the most recent session
    #[arg(short, long)]
    pub current: bool,
    /// Resume a session
    #[arg(short = 's', long, conflicts_with = "current", value_name = "UUID")]
    pub session: Option<uuid::Uuid>,
    /// Use a certain config file
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,
}

pub fn show_resume_hint(session_id: Uuid) {
    println!();
    println!("This session can be reopened with:");
    println!();
    println!("  shuvarie -s {session_id}");
}
