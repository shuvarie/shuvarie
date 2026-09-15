use std::path::PathBuf;

use shuvarie_core::{Category, Config, ScanItem, SceneSet, TrustFile, TrustGrants, WorkspaceScan};

use crate::tui::trust::{TrustPromptOutcome, run_prompt};

pub struct Resolved {
    pub config: Config,
    pub grants: TrustGrants,
    /// The merged scene set for the run: the config chain's `scenes`
    /// sections plus the `scene.d` drop-in dirs under their trust rules,
    /// with a warning per same-level conflict.
    pub scenes: SceneSet,
}

/// Resolves the workspace trust decision and loads the config under it: a
/// recorded workspace applies its grants silently — except when the scan
/// finds candidates in categories that were neither granted nor asked before,
/// which prompts once more for just those items (new trust candidates that
/// appeared after the recorded decision). An undecided workspace with trust
/// candidates prompts in full, an undecided workspace with nothing to trust
/// proceeds fully granted, and a global `trust` block answers for workspaces
/// without a record. `None` means the prompt quit the program.
pub async fn resolve(explicit_config: Option<&PathBuf>) -> color_eyre::Result<Option<Resolved>> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let scan = WorkspaceScan::detect(&cwd, explicit_config.is_some());

    let mut file = TrustFile::load()?;
    let grants = if let Some(existing) = file.lookup_record(&cwd) {
        let grants = existing.grants.clone();
        if existing.grants.is_empty() {
            grants
        } else {
            match follow_up_scan(&scan, &grants, &existing.asked) {
                None => grants,
                Some(followup) => {
                    match run_prompt(&followup_scan(&followup), &cwd, true).await? {
                        TrustPromptOutcome::Quit => return Ok(None),
                        // Rejected for this session only: nothing recorded, so
                        // the same items ask again on the next startup.
                        TrustPromptOutcome::Skip => grants,
                        TrustPromptOutcome::Grant(new) => {
                            let merged = grants.union(new);
                            file.upsert(&cwd, merged.clone());
                            file.mark_asked(&cwd, followup.iter().map(|item| item.category));
                            file.save()?;
                            merged
                        }
                    }
                }
            }
        }
    } else if scan.is_empty() {
        TrustGrants::all()
    } else if let Some(default) = file.default_grants.clone() {
        match follow_up_scan(&scan, &default, &Default::default()) {
            None => default,
            Some(followup) => match run_prompt(&followup_scan(&followup), &cwd, true).await? {
                TrustPromptOutcome::Quit => return Ok(None),
                TrustPromptOutcome::Skip => default,
                TrustPromptOutcome::Grant(new) => {
                    let merged = default.union(new);
                    file.upsert(&cwd, merged.clone());
                    file.mark_asked(&cwd, followup.iter().map(|item| item.category));
                    file.save()?;
                    merged
                }
            },
        }
    } else {
        match run_prompt(&scan, &cwd, false).await? {
            TrustPromptOutcome::Quit => return Ok(None),
            TrustPromptOutcome::Skip => TrustGrants::none(),
            TrustPromptOutcome::Grant(grants) => {
                file.upsert(&cwd, grants.clone());
                file.mark_asked(&cwd, scan.items.iter().map(|item| item.category));
                file.save()?;
                grants
            }
        }
    };

    let config = match explicit_config {
        Some(path) => Config::load_explicit(path)?,
        None => Config::load_trusted(&cwd, &grants)?,
    };
    let scenes = Config::load_scenes(
        &config,
        &cwd,
        &grants,
        explicit_config.as_ref().map(|p| p.as_path()),
    )?;
    Ok(Some(Resolved {
        config,
        grants,
        scenes,
    }))
}

/// The scan items a recorded workspace still needs a decision for: categories
/// neither granted nor asked before. `None` when nothing new asks.
fn follow_up_scan(
    scan: &WorkspaceScan,
    grants: &TrustGrants,
    asked: &std::collections::BTreeSet<Category>,
) -> Option<Vec<ScanItem>> {
    let items: Vec<ScanItem> = scan
        .items
        .iter()
        .filter(|item| !grants.allows(item.category) && !asked.contains(&item.category))
        .cloned()
        .collect();
    (!items.is_empty()).then_some(items)
}

fn followup_scan(items: &[ScanItem]) -> WorkspaceScan {
    WorkspaceScan {
        items: items.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_of(categories: &[Category]) -> WorkspaceScan {
        WorkspaceScan {
            items: categories
                .iter()
                .map(|category| ScanItem {
                    category: *category,
                    detail: "x".to_string(),
                })
                .collect(),
        }
    }

    #[test]
    fn follow_up_asks_only_ungranted_and_unasked_categories() {
        let scan = scan_of(&[Category::Contexts, Category::Skills, Category::Configs]);
        let grants = TrustGrants::from_categories([Category::Contexts, Category::Skills]);
        let asked = [Category::Contexts, Category::Skills].into_iter().collect();

        let followup = follow_up_scan(&scan, &grants, &asked).unwrap();
        assert_eq!(followup.len(), 1);
        assert_eq!(followup[0].category, Category::Configs);

        // Everything already decided: no follow-up.
        assert!(
            follow_up_scan(
                &scan,
                &grants,
                &asked
                    .iter()
                    .copied()
                    .chain([Category::Configs].into_iter())
                    .collect()
            )
            .is_none()
        );
        assert!(follow_up_scan(&scan, &TrustGrants::all(), &asked).is_none());
    }

    #[test]
    fn a_fresh_scan_without_asks_everything() {
        let scan = scan_of(&[Category::Contexts, Category::Configs]);
        let grants = TrustGrants::from_categories([Category::Contexts]);
        let followup = follow_up_scan(&scan, &grants, &Default::default()).unwrap();
        assert_eq!(followup.len(), 1);
        assert_eq!(followup[0].category, Category::Configs);
    }
}
