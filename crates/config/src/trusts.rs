use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    CONFIG_FILE_NAME, LOCAL_CONFIG_FILE_NAME, SCENE_DIR_NAME, THEMES_DIR_NAME, WORKSPACE_DIR_NAME,
};
use crate::Result;

pub const TRUSTS_FILE_NAME: &str = "trusts.kdl";

/// A workspace-trust category: what a granted workspace may load at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// `AGENTS.md`/`CLAUDE.md` context files and `.shuvarie/context/*`.
    Contexts,
    /// `.agents/skills`.
    Skills,
    /// `shuvarie.kdl` and `.shuvarie/config.kdl`.
    Configs,
}

pub const ALL_CATEGORIES: [Category; 3] = [Category::Contexts, Category::Skills, Category::Configs];

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Contexts => "contexts",
            Self::Skills => "skills",
            Self::Configs => "configs",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        ALL_CATEGORIES.into_iter().find(|c| c.as_str() == name)
    }
}

/// The categories a workspace was trusted for. `all` subsumes every category,
/// present and future; named entries list the granted ones.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustGrants {
    all: bool,
    categories: BTreeSet<Category>,
}

impl TrustGrants {
    pub fn all() -> Self {
        Self {
            all: true,
            categories: BTreeSet::new(),
        }
    }

    pub fn none() -> Self {
        Self::default()
    }

    pub fn from_categories(categories: impl IntoIterator<Item = Category>) -> Self {
        Self {
            all: false,
            categories: categories.into_iter().collect(),
        }
    }

    /// Whether the category is granted: `all` covers everything, otherwise
    /// the category must be named.
    pub fn allows(&self, category: Category) -> bool {
        self.all || self.categories.contains(&category)
    }

    pub fn is_all(&self) -> bool {
        self.all
    }

    pub fn is_empty(&self) -> bool {
        !self.all && self.categories.is_empty()
    }

    /// The named granted categories (empty when `all` is set).
    pub fn categories(&self) -> impl Iterator<Item = Category> + '_ {
        self.categories.iter().copied()
    }

    /// Whether every current category is granted, by flag or by enumeration.
    pub fn covers_all(&self) -> bool {
        self.all || ALL_CATEGORIES.iter().all(|c| self.categories.contains(c))
    }

    /// The union of two grant sets: `all` subsumes everything, otherwise the
    /// named categories of both.
    pub fn union(self, other: TrustGrants) -> TrustGrants {
        if self.all || other.all {
            Self::all()
        } else {
            Self::from_categories(self.categories.iter().copied().chain(other.categories))
        }
    }
}

/// One `path` record: the typed path literal from the file plus its grants.
/// An empty `trust` block (no grants) is a rejected workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceTrust {
    pub path: String,
    pub grants: TrustGrants,

    /// The categories a trust prompt has already offered for this workspace,
    /// granted or not: a follow-up prompt only asks about categories that are
    /// neither granted nor asked before, so candidates that appear after the
    /// recorded decision (new config files, scene dirs) prompt exactly once.
    pub asked: BTreeSet<Category>,
}

/// `trusts.kdl` — the workspace trust decisions: an optional global default
/// `trust` block plus one `path` record per decided workspace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustFile {
    /// The default for workspaces without a record.
    pub default_grants: Option<TrustGrants>,
    /// Records in file order; lookup takes the first match.
    pub workspaces: Vec<WorkspaceTrust>,
}

impl TrustFile {
    pub fn trusts_path() -> Result<PathBuf> {
        Ok(super::config_dir()?.join(TRUSTS_FILE_NAME))
    }

    pub fn load() -> Result<Self> {
        let path = Self::trusts_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(super::trusts_kdl::from_kdl(&contents)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(crate::ConfigError::Io(e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::trusts_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, super::trusts_kdl::to_kdl(self)?)?;
        Ok(())
    }

    /// The grants for `cwd`, or `None` when no record matches (the global
    /// default is not consulted here — callers decide what absence means).
    /// Record paths match by canonicalized target, so `~`, trailing slashes
    /// and `.` components do not affect the match.
    pub fn lookup(&self, cwd: &Path) -> Option<&TrustGrants> {
        Self::lookup_record(self, cwd).map(|record| &record.grants)
    }

    /// The full record for `cwd` (grants plus the asked categories), or
    /// `None` when no record matches.
    pub fn lookup_record<'a>(&'a self, cwd: &Path) -> Option<&'a WorkspaceTrust> {
        let cwd = canonical(cwd);
        self.workspaces
            .iter()
            .find(|workspace| resolve_record_path(&workspace.path) == Some(cwd.clone()))
    }

    /// Sets the grants for `cwd`, replacing the first matching record's
    /// grants (keeping its typed path and asked categories) or appending a
    /// new record.
    pub fn upsert(&mut self, cwd: &Path, grants: TrustGrants) {
        let cwd = canonical(cwd);
        for workspace in &mut self.workspaces {
            if resolve_record_path(&workspace.path) == Some(cwd.clone()) {
                workspace.grants = grants;
                return;
            }
        }
        self.workspaces.push(WorkspaceTrust {
            path: cwd.to_string_lossy().into_owned(),
            grants,
            asked: Default::default(),
        });
    }

    /// Records the categories a prompt offered for `cwd` (extending the
    /// existing record's asked set; a no-op without a record).
    pub fn mark_asked(&mut self, cwd: &Path, asked: impl IntoIterator<Item = Category>) {
        let cwd = canonical(cwd);
        if let Some(record) = self
            .workspaces
            .iter_mut()
            .find(|workspace| resolve_record_path(&workspace.path) == Some(cwd.clone()))
        {
            record.asked.extend(asked);
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Expands a leading `~` to the home directory; the rest is kept as written.
fn expand_tilde_in(path: &str, home: Option<&Path>) -> PathBuf {
    if path == "~" {
        home.map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(path))
    } else if let Some(rest) = path.strip_prefix("~/") {
        match home {
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        }
    } else {
        PathBuf::from(path)
    }
}

fn resolve_record_path(record: &str) -> Option<PathBuf> {
    resolve_record_path_in(record, dirs::home_dir().as_deref())
}

fn resolve_record_path_in(record: &str, home: Option<&Path>) -> Option<PathBuf> {
    let expanded = expand_tilde_in(record, home);
    Some(canonical(&expanded))
}

/// A workspace trust candidate found while scanning the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanItem {
    pub category: Category,
    /// What was found, for the prompt display (e.g. `AGENTS.md`,
    /// `.shuvarie/context (2 files)`, `3 skills`).
    pub detail: String,
}

/// What the working directory holds that needs a trust decision. Only the
/// workspace root is scanned; user-owned global locations are always trusted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceScan {
    pub items: Vec<ScanItem>,
}

impl WorkspaceScan {
    /// Detects trust candidates in `cwd`. With `explicit_config` the config
    /// category is skipped: the file was named on the command line, not
    /// discovered in the workspace.
    pub fn detect(cwd: &Path, explicit_config: bool) -> Self {
        let mut items: Vec<ScanItem> = Vec::new();

        let config_detail = config_detail(cwd);
        if let Some(detail) = config_detail.filter(|_| !explicit_config) {
            items.push(ScanItem {
                category: Category::Configs,
                detail,
            });
        }

        let context_detail = context_detail(cwd);
        if let Some(detail) = context_detail {
            items.push(ScanItem {
                category: Category::Contexts,
                detail,
            });
        }

        let skills_dir = cwd.join(".agents").join("skills");
        if let Some(detail) = entry_count(&skills_dir) {
            let detail = if detail == 1 {
                "1 skill".to_string()
            } else {
                format!("{detail} skills")
            };
            items.push(ScanItem {
                category: Category::Skills,
                detail,
            });
        }

        Self { items }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// The workspace config files present, joined for display, if any.
fn config_detail(cwd: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if cwd.join(LOCAL_CONFIG_FILE_NAME).exists() {
        parts.push(LOCAL_CONFIG_FILE_NAME.to_string());
    }
    if let Some(count) = kdl_file_count(&cwd.join(SCENE_DIR_NAME)) {
        let plural = if count == 1 { "file" } else { "files" };
        parts.push(format!("{SCENE_DIR_NAME} ({count} {plural})"));
    }
    if let Some(count) = kdl_file_count(&cwd.join(THEMES_DIR_NAME)) {
        let plural = if count == 1 { "file" } else { "files" };
        parts.push(format!("{THEMES_DIR_NAME} ({count} {plural})"));
    }
    let workspace_config = cwd.join(WORKSPACE_DIR_NAME).join(CONFIG_FILE_NAME);
    if workspace_config.exists() {
        parts.push(format!("{WORKSPACE_DIR_NAME}/{CONFIG_FILE_NAME}"));
    }
    if let Some(count) = kdl_file_count(&cwd.join(WORKSPACE_DIR_NAME).join(SCENE_DIR_NAME)) {
        let plural = if count == 1 { "file" } else { "files" };
        parts.push(format!(
            "{WORKSPACE_DIR_NAME}/{SCENE_DIR_NAME} ({count} {plural})"
        ));
    }
    if let Some(count) = kdl_file_count(&cwd.join(WORKSPACE_DIR_NAME).join(THEMES_DIR_NAME)) {
        let plural = if count == 1 { "file" } else { "files" };
        parts.push(format!(
            "{WORKSPACE_DIR_NAME}/{THEMES_DIR_NAME} ({count} {plural})"
        ));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The number of `*.kdl` file entries in `dir`, or `None` when there are none.
fn kdl_file_count(dir: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir).ok()?;
    let count = entries
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            path.is_file() && path.extension().is_some_and(|ext| ext == "kdl")
        })
        .count();
    (count > 0).then_some(count)
}

/// The workspace context file or context dir contents, if any.
fn context_detail(cwd: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(name) = crate::CONTEXT_FILE_CANDIDATES
        .iter()
        .map(|name| cwd.join(name))
        .find(|candidate| candidate.is_file())
    {
        parts.push(
            name.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    let context_dir = cwd.join(WORKSPACE_DIR_NAME).join("context");
    if let Some(count) = file_count(&context_dir) {
        let plural = if count == 1 { "file" } else { "files" };
        parts.push(format!("{WORKSPACE_DIR_NAME}/context ({count} {plural})"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The number of entries in `dir`, or `None` when it is not a non-empty dir.
fn entry_count(dir: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir).ok()?;
    let count = entries.count();
    (count > 0).then_some(count)
}

/// The number of file entries in `dir`, or `None` when there are none.
fn file_count(dir: &Path) -> Option<usize> {
    let entries = std::fs::read_dir(dir).ok()?;
    let count = entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .count();
    (count > 0).then_some(count)
}

#[allow(unused_imports)]
use super::CONFIG_DIR_NAME;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn item(scan: &WorkspaceScan, category: Category) -> &ScanItem {
        scan.items
            .iter()
            .find(|item| item.category == category)
            .unwrap_or_else(|| panic!("no {category:?} item in {:?}", scan.items))
    }

    #[test]
    fn scan_detects_each_category() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();

        assert!(WorkspaceScan::detect(cwd, false).is_empty());

        write(&cwd.join(LOCAL_CONFIG_FILE_NAME), "ui { frame-rate 30 }");
        assert_eq!(
            item(&WorkspaceScan::detect(cwd, false), Category::Configs).detail,
            LOCAL_CONFIG_FILE_NAME
        );

        write(&cwd.join("AGENTS.md"), "rules");
        assert_eq!(
            item(&WorkspaceScan::detect(cwd, false), Category::Contexts).detail,
            "AGENTS.md"
        );

        write(
            &cwd.join(".agents")
                .join("skills")
                .join("demo")
                .join("SKILL.md"),
            "demo",
        );
        assert_eq!(
            item(&WorkspaceScan::detect(cwd, false), Category::Skills).detail,
            "1 skill"
        );
    }

    #[test]
    fn scan_sees_workspace_config_and_context_dir() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();

        write(
            &cwd.join(WORKSPACE_DIR_NAME).join(CONFIG_FILE_NAME),
            "agent { max-turns 5 }",
        );
        write(
            &cwd.join(WORKSPACE_DIR_NAME)
                .join("context")
                .join("notes.md"),
            "notes",
        );
        write(&cwd.join(WORKSPACE_DIR_NAME).join("data.db"), "");

        let scan = WorkspaceScan::detect(cwd, false);
        assert_eq!(
            item(&scan, Category::Configs).detail,
            format!("{WORKSPACE_DIR_NAME}/{CONFIG_FILE_NAME}")
        );
        assert_eq!(
            item(&scan, Category::Contexts).detail,
            format!("{WORKSPACE_DIR_NAME}/context (1 file)")
        );
    }

    #[test]
    fn scan_lists_workspace_scene_drops_under_configs() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let scene_dir = cwd.join(WORKSPACE_DIR_NAME).join(SCENE_DIR_NAME);
        std::fs::create_dir_all(&scene_dir).unwrap();
        std::fs::write(scene_dir.join("plan.kdl"), "scenes {}").unwrap();
        std::fs::write(scene_dir.join("notes.txt"), "not kdl").unwrap();

        let scan = WorkspaceScan::detect(cwd, false);
        assert_eq!(
            item(&scan, Category::Configs).detail,
            format!("{WORKSPACE_DIR_NAME}/{SCENE_DIR_NAME} (1 file)")
        );

        std::fs::write(scene_dir.join("other.kdl"), "scenes {}").unwrap();
        let scan = WorkspaceScan::detect(cwd, false);
        assert_eq!(
            item(&scan, Category::Configs).detail,
            format!("{WORKSPACE_DIR_NAME}/{SCENE_DIR_NAME} (2 files)")
        );
    }

    #[test]
    fn scan_app_dir_with_only_data_is_not_a_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        write(&cwd.join(WORKSPACE_DIR_NAME).join("data.db"), "");

        assert!(WorkspaceScan::detect(cwd, false).is_empty());

        std::fs::create_dir_all(cwd.join(".agents").join("skills")).unwrap();
        assert!(WorkspaceScan::detect(cwd, false).is_empty());

        std::fs::create_dir_all(cwd.join(WORKSPACE_DIR_NAME).join("context")).unwrap();
        assert!(WorkspaceScan::detect(cwd, false).is_empty());
    }

    #[test]
    fn explicit_config_skips_the_config_category() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        write(&cwd.join(LOCAL_CONFIG_FILE_NAME), "ui { frame-rate 30 }");
        write(&cwd.join("CLAUDE.md"), "rules");

        let scan = WorkspaceScan::detect(cwd, true);
        assert_eq!(scan.items.len(), 1);
        assert_eq!(scan.items[0].category, Category::Contexts);
    }

    #[test]
    fn lookup_expands_and_canonicalizes_record_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();

        let mut file = TrustFile::default();
        file.workspaces.push(WorkspaceTrust {
            path: format!("{}/./", cwd.to_string_lossy()),
            grants: TrustGrants::from_categories([Category::Skills]),
            asked: Default::default(),
        });
        assert!(file.lookup(&cwd).unwrap().allows(Category::Skills));
        assert!(!file.lookup(&cwd).unwrap().allows(Category::Configs));

        let other = tempfile::tempdir().unwrap();
        assert!(file.lookup(other.path()).is_none());
    }

    #[test]
    fn upsert_replaces_the_matching_record() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();

        let mut file = TrustFile::default();
        file.upsert(&cwd, TrustGrants::all());
        file.upsert(&cwd, TrustGrants::none());
        assert_eq!(file.workspaces.len(), 1);
        assert_eq!(file.workspaces[0].path, cwd.to_string_lossy().into_owned());
        assert!(file.workspaces[0].grants.is_empty());
        assert!(file.lookup(&cwd).unwrap().is_empty());
    }

    #[test]
    fn save_and_reload_via_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusts.kdl");
        let file = TrustFile {
            default_grants: Some(TrustGrants::all()),
            workspaces: vec![
                WorkspaceTrust {
                    path: "/home/user/proj".into(),
                    grants: TrustGrants::from_categories([Category::Contexts, Category::Configs]),
                    asked: Default::default(),
                },
                WorkspaceTrust {
                    path: "/home/user/other".into(),
                    grants: TrustGrants::none(),
                    asked: Default::default(),
                },
            ],
        };
        file.save_to(&path).unwrap();

        let reloaded = TrustFile::load_from(&path).unwrap();
        assert_eq!(file, reloaded);
        assert!(
            reloaded
                .lookup(Path::new("/home/user/proj"))
                .unwrap()
                .allows(Category::Contexts)
        );
        assert!(
            !reloaded
                .lookup(Path::new("/home/user/proj"))
                .unwrap()
                .allows(Category::Skills)
        );
        assert!(
            reloaded
                .lookup(Path::new("/home/user/other"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn tilde_record_expands_to_the_home_directory() {
        let home = tempfile::tempdir().unwrap();
        let cwd = home.path().join("source/my-project");
        std::fs::create_dir_all(&cwd).unwrap();

        assert_eq!(
            resolve_record_path_in("~/source/my-project/", Some(home.path())),
            Some(cwd.clone())
        );
        assert_eq!(
            resolve_record_path_in("~", Some(home.path())),
            Some(home.path().to_path_buf())
        );
        assert_eq!(
            resolve_record_path_in("/plain/path", Some(home.path())),
            Some(PathBuf::from("/plain/path"))
        );
    }
}
