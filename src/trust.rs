use std::path::PathBuf;

use shuvarie_core::{Config, TrustFile, TrustGrants, WorkspaceScan};

use crate::tui::trust::{TrustPromptOutcome, run_prompt};

pub struct Resolved {
    pub config: Config,
    pub grants: TrustGrants,
}

/// Resolves the workspace trust decision and loads the config under it: a
/// recorded workspace applies its grants silently, an undecided workspace
/// with trust candidates prompts, an undecided workspace with nothing to
/// trust proceeds fully granted, and a global `trust` block answers for
/// workspaces without a record. `None` means the prompt quit the program.
pub async fn resolve(explicit_config: Option<&PathBuf>) -> color_eyre::Result<Option<Resolved>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scan = WorkspaceScan::detect(&cwd, explicit_config.is_some());

    let mut file = TrustFile::load()?;
    let grants = if let Some(existing) = file.lookup(&cwd) {
        existing.clone()
    } else if scan.is_empty() {
        TrustGrants::all()
    } else if let Some(default) = file.default_grants.clone() {
        default
    } else {
        match run_prompt(&scan, &cwd).await? {
            TrustPromptOutcome::Quit => return Ok(None),
            TrustPromptOutcome::Skip => TrustGrants::none(),
            TrustPromptOutcome::Grant(grants) => {
                file.upsert(&cwd, grants.clone());
                file.save()?;
                grants
            }
        }
    };

    let config = match explicit_config {
        Some(path) => Config::load_explicit(path)?,
        None => Config::load_trusted(&cwd, &grants)?,
    };
    Ok(Some(Resolved { config, grants }))
}
