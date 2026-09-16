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
    /// Run in a certain directory
    #[arg(short, long, value_name = "DIR")]
    pub dir: Option<PathBuf>,
    /// Import a session from a JSON file (as written by --export-session)
    #[arg(long, value_name = "FILE", conflicts_with = "export_session")]
    pub import_session: Option<PathBuf>,
    /// Export a session as JSON ([FILE]); with -s exports that session,
    /// otherwise the most recent one. Without a value the file is named
    /// `<session-id>-<timestamp>.json` in the working directory
    #[arg(
        long,
        value_name = "FILE",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub export_session: Option<String>,
}

pub fn show_resume_hint(session_id: Uuid) {
    println!();
    println!("This session can be reopened with:");
    println!();
    println!("  shuvarie -s {session_id}");
}

pub async fn import_session(
    store: &mut shuvarie_db::Store,
    path: &std::path::Path,
) -> color_eyre::Result<()> {
    let json = std::fs::read_to_string(path)
        .map_err(|e| color_eyre::eyre::eyre!("read {}: {e}", path.display()))?;
    let file = shuvarie_db::SessionFile::from_json(&json)?;
    let id = store.import_session(&file).await?;
    println!("Imported session \"{}\"", file.session.title);
    show_resume_hint(id);
    Ok(())
}

pub async fn export_session(
    store: &mut shuvarie_db::Store,
    dest: &str,
    session: Option<Uuid>,
) -> color_eyre::Result<()> {
    let stored = match session {
        Some(id) => Some(store.load_session(id).await?),
        None => store.most_recent_session().await?,
    };
    let Some(stored) = stored else {
        return Err(color_eyre::eyre::eyre!("no sessions to export"));
    };
    let explicit = (!dest.is_empty()).then(|| PathBuf::from(dest));
    let path = shuvarie_db::session_file::resolve_export_path(explicit.as_deref(), stored.id);
    shuvarie_db::SessionFile::from_stored(&stored).write_json(&path)?;
    println!(
        "Session \"{}\" exported to {}",
        stored.title,
        path.display()
    );
    Ok(())
}
