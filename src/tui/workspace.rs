use std::path::{Component, Path, PathBuf};

use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use super::theme;
use crate::tui::utils::text::truncate_spans;

#[derive(Default)]
pub struct WorkspaceInfo {
    pub path: PathBuf,
    pub home: Option<PathBuf>,
    pub branch: Option<String>,
}

impl WorkspaceInfo {
    pub fn detect() -> Self {
        let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let home = dirs::home_dir();
        let branch = detect_branch(&path);
        Self { path, home, branch }
    }

    /// Re-read the git branch at the known workspace path. Sent when a turn
    /// starts, so checkouts made between turns (e.g. in another terminal) are
    /// picked up by the sidebar and the collapsed footer.
    pub fn refresh_branch(&mut self) {
        if self.path.as_os_str().is_empty() {
            return;
        }
        self.branch = detect_branch(&self.path);
    }

    /// The expanded-sidebar section above Context: the path line plus the
    /// branch line when the workspace is inside a git repository.
    pub fn section_lines(&self, path_cols: usize) -> Vec<Line<'static>> {
        if self.path.as_os_str().is_empty() {
            return Vec::new();
        }
        let mut lines = vec![Line::from(path_spans(
            &self.path,
            self.home.as_deref(),
            path_cols,
        ))];
        if let Some(branch) = &self.branch {
            lines.push(Line::from(branch_spans(branch)));
        }
        lines
    }

    /// Path and branch on one line, for the collapsed sidebar's footer.
    pub fn compact_spans(&self, max_cols: usize) -> Vec<Span<'static>> {
        if self.path.as_os_str().is_empty() {
            return Vec::new();
        }
        let branch = self.branch.as_ref().map(|b| branch_spans(b));
        let branch_cols = branch
            .as_ref()
            .map(|spans| 1 + spans.iter().map(Span::width).sum::<usize>())
            .unwrap_or(0);
        let mut spans = path_spans(
            &self.path,
            self.home.as_deref(),
            max_cols.saturating_sub(branch_cols),
        );
        if let Some(branch) = branch {
            spans.push(Span::raw(" ").fg(theme::text_muted()));
            spans.extend(branch);
        }
        truncate_spans(spans, max_cols)
    }
}

fn detect_branch(path: &Path) -> Option<String> {
    let repo = gix::discover(path).ok()?;
    let head = repo.head().ok()?;
    branch_name(&head)
}

fn branch_name(head: &gix::Head<'_>) -> Option<String> {
    if let Some(name) = head.referent_name() {
        return Some(String::from_utf8_lossy(name.shorten()).into_owned());
    }
    if head.is_detached() {
        return head.id().map(|id| id.shorten_or_id().to_string());
    }
    None
}

fn branch_spans(branch: &str) -> Vec<Span<'static>> {
    vec![
        Span::raw("⎇ ").fg(theme::text_muted()),
        Span::raw(branch.to_string()).fg(theme::text()),
    ]
}

fn path_spans(path: &Path, home: Option<&Path>, max_cols: usize) -> Vec<Span<'static>> {
    if max_cols == 0 {
        return Vec::new();
    }
    let display = DisplayPath::new(path, home);
    let full = display.spans(false);
    if spans_width(&full) <= max_cols {
        return full;
    }
    let compact = display.spans(true);
    if spans_width(&compact) <= max_cols {
        return compact;
    }
    truncate_spans(display.bare_spans(), max_cols)
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// How the path displays: an anchor text (root) that may be followed by one
/// separator (`sep_after_root`) and then the components.
struct DisplayPath {
    root: String,
    sep_after_root: bool,
    comps: Vec<String>,
}

impl DisplayPath {
    fn new(path: &Path, home: Option<&Path>) -> Self {
        if let Some(home) = home
            && home.file_name().is_some()
        {
            if path == home {
                return Self {
                    root: "~".into(),
                    sep_after_root: false,
                    comps: Vec::new(),
                };
            }
            if let Ok(rest) = path.strip_prefix(home) {
                return Self {
                    root: "~".into(),
                    sep_after_root: true,
                    comps: comps_of(rest),
                };
            }
        }
        let mut root = String::new();
        let mut sep_after_root = false;
        let mut comps = Vec::new();
        for comp in path.components() {
            match comp {
                Component::Prefix(prefix) => {
                    root = prefix.as_os_str().to_string_lossy().into_owned()
                }
                Component::RootDir => sep_after_root = true,
                Component::CurDir => comps.push(".".into()),
                _ => comps.push(comp.as_os_str().to_string_lossy().into_owned()),
            }
        }
        Self {
            root,
            sep_after_root,
            comps,
        }
    }

    fn prefix(&self, sep: char) -> String {
        if !self.root.is_empty() {
            format!("{}{sep}", self.root)
        } else if self.sep_after_root {
            sep.to_string()
        } else {
            String::new()
        }
    }

    /// Spans for the whole path; `compress` collapses every non-final
    /// component to its first character.
    fn spans(&self, compress: bool) -> Vec<Span<'static>> {
        let sep = std::path::MAIN_SEPARATOR;
        let comps: Vec<String> = if compress && self.comps.len() > 1 {
            let (last, init) = self.comps.split_last().expect("non-empty");
            let mut comps: Vec<String> = init
                .iter()
                .map(|c| {
                    c.chars()
                        .next()
                        .map(String::from)
                        .unwrap_or_else(|| c.clone())
                })
                .collect();
            comps.push(last.clone());
            comps
        } else {
            self.comps.clone()
        };
        let Some(tail) = comps.last().cloned() else {
            let text = match self.sep_after_root {
                true if self.root.is_empty() => "/".into(),
                true => format!("{}{sep}", self.root),
                false => self.root.clone(),
            };
            return vec![Span::raw(text).fg(theme::text())];
        };
        let mut head = self.prefix(sep);
        for (i, comp) in comps.iter().enumerate().take(comps.len() - 1) {
            if i > 0 {
                head.push(sep);
            }
            head.push_str(comp);
        }
        if comps.len() > 1 {
            head.push(sep);
        }
        vec![
            Span::raw(head).fg(theme::text_dim()),
            Span::raw(tail).fg(theme::text()),
        ]
    }

    /// Last-resort spans: the anchor and the final component only.
    fn bare_spans(&self) -> Vec<Span<'static>> {
        let sep = std::path::MAIN_SEPARATOR;
        let tail = self
            .comps
            .last()
            .cloned()
            .unwrap_or_else(|| self.root.clone());
        vec![
            Span::raw(self.prefix(sep)).fg(theme::text_dim()),
            Span::raw(tail).fg(theme::text()),
        ]
    }
}

fn comps_of(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect()
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::Path;

    /// Seeds a minimal git worktree at `dir` whose `HEAD` points at
    /// `refs/heads/{branch}` (unborn: no commit is written). Enough for
    /// `gix::discover` + `head()` to resolve, without shelling out to git.
    pub(crate) fn seed_git_repo(dir: &Path, branch: &str) {
        let git_dir = dir.join(".git");
        std::fs::create_dir_all(git_dir.join("refs/heads")).expect("create .git/refs/heads");
        std::fs::create_dir_all(git_dir.join("objects")).expect("create .git/objects");
        std::fs::write(git_dir.join("HEAD"), format!("ref: refs/heads/{branch}\n"))
            .expect("write HEAD");
    }
}

#[cfg(test)]
mod tests {
    use ratatui::style::Color;

    use super::*;

    fn joined(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn fg(spans: &[Span<'static>]) -> Vec<Option<Color>> {
        spans.iter().map(|s| s.style.fg).collect()
    }

    #[test]
    fn absolute_path_is_full_with_bright_tail() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/other/repos/shuvarie"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 100);
        assert_eq!(joined(&spans), "/home/other/repos/shuvarie");
        assert_eq!(
            fg(&spans),
            vec![Some(theme::text_dim()), Some(theme::text())]
        );
    }

    #[test]
    fn home_prefix_becomes_tilde() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/projects/shuvarie"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 100);
        assert_eq!(joined(&spans), "~/projects/shuvarie");
        assert_eq!(
            fg(&spans),
            vec![Some(theme::text_dim()), Some(theme::text())]
        );
    }

    #[test]
    fn home_itself_is_bare_tilde() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 100);
        assert_eq!(joined(&spans), "~");
        assert_eq!(fg(&spans), vec![Some(theme::text())]);
    }

    #[test]
    fn sibling_of_home_is_not_tilde() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/userx/projects/shuvarie"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 100);
        assert_eq!(joined(&spans), "/home/userx/projects/shuvarie");
    }

    #[test]
    fn long_path_compresses_intermediates_to_initials() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/source/repos/shuvarie"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 20);
        assert_eq!(joined(&spans), "~/s/r/shuvarie");
        assert_eq!(
            fg(&spans),
            vec![Some(theme::text_dim()), Some(theme::text())]
        );
    }

    #[test]
    fn long_relative_path_compresses_with_absolute_prefix() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/mnt/data/workdrive/verylongproject"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 25);
        assert_eq!(joined(&spans), "/m/d/w/verylongproject");
    }

    #[test]
    fn long_final_component_drops_intermediates_and_ellipsizes() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/averyveryverylongprojectname"),
            home: Some(PathBuf::from("/home/user")),
            branch: None,
        };
        let spans = path_spans(&info.path, info.home.as_deref(), 10);
        assert_eq!(joined(&spans), "~/averyve…");
        assert_eq!(
            fg(&spans),
            vec![Some(theme::text_dim()), Some(theme::text())]
        );
    }

    #[test]
    fn zero_budget_is_empty() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/project"),
            home: None,
            branch: None,
        };
        assert!(path_spans(&info.path, info.home.as_deref(), 0).is_empty());
    }

    #[test]
    fn windows_style_segments_keep_the_drive_prefix() {
        let sep = std::path::MAIN_SEPARATOR;
        let display = DisplayPath {
            root: "C:".into(),
            sep_after_root: true,
            comps: vec!["Users".into(), "user".into(), "project".into()],
        };
        assert_eq!(
            joined(&display.spans(false)),
            format!("C:{sep}Users{sep}user{sep}project")
        );
        assert_eq!(
            joined(&display.spans(true)),
            format!("C:{sep}U{sep}u{sep}project")
        );
    }

    #[test]
    fn windows_style_home_becomes_tilde() {
        let sep = std::path::MAIN_SEPARATOR;
        let display = DisplayPath {
            root: "~".into(),
            sep_after_root: true,
            comps: vec!["project".into()],
        };
        assert_eq!(joined(&display.spans(false)), format!("~{sep}project"));
    }

    #[test]
    fn section_lines_show_path_then_branch() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/project"),
            home: Some(PathBuf::from("/home/user")),
            branch: Some("main".into()),
        };
        let lines = info.section_lines(26);
        let text = |l: &Line<'static>| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(&lines[0]), "~/project");
        assert_eq!(text(&lines[1]), "⎇ main");
    }

    #[test]
    fn compact_spans_join_path_and_branch() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/project"),
            home: Some(PathBuf::from("/home/user")),
            branch: Some("main".into()),
        };
        let spans = info.compact_spans(100);
        assert_eq!(joined(&spans), "~/project ⎇ main");
    }

    #[test]
    fn compact_spans_reserve_branch_width() {
        let info = WorkspaceInfo {
            path: PathBuf::from("/home/user/reallylongprojectname"),
            home: Some(PathBuf::from("/home/user")),
            branch: Some("main".into()),
        };
        let spans = info.compact_spans(24);
        assert_eq!(joined(&spans), "~/reallylongproj… ⎇ main");
    }

    #[test]
    fn compact_spans_skip_empty_workspace() {
        assert!(WorkspaceInfo::default().compact_spans(80).is_empty());
        assert!(WorkspaceInfo::default().section_lines(26).is_empty());
    }

    #[test]
    fn detect_reports_current_dir() {
        let info = WorkspaceInfo::detect();
        assert!(!info.path.as_os_str().is_empty());
    }

    #[test]
    fn branch_marker_spans_use_theme_colors() {
        let spans = branch_spans("main");
        assert_eq!(joined(&spans), "⎇ main");
        assert_eq!(
            fg(&spans),
            vec![Some(theme::text_muted()), Some(theme::text())]
        );
    }

    #[test]
    fn refresh_branch_reads_head_at_workspace_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        testing::seed_git_repo(dir.path(), "feature-x");
        let mut info = WorkspaceInfo {
            path: dir.path().to_path_buf(),
            home: None,
            branch: None,
        };

        info.refresh_branch();
        assert_eq!(info.branch.as_deref(), Some("feature-x"));

        // A checkout between turns is picked up by the next refresh.
        testing::seed_git_repo(dir.path(), "main");
        info.refresh_branch();
        assert_eq!(info.branch.as_deref(), Some("main"));
    }

    #[test]
    fn refresh_branch_clears_when_not_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut info = WorkspaceInfo {
            path: dir.path().to_path_buf(),
            home: None,
            branch: Some("stale".into()),
        };
        info.refresh_branch();
        assert_eq!(info.branch, None, "a vanished repo clears the branch");
    }

    #[test]
    fn refresh_branch_keeps_default_workspace_untouched() {
        let mut info = WorkspaceInfo::default();
        info.refresh_branch();
        assert_eq!(info.branch, None);
        assert!(info.path.as_os_str().is_empty());
    }
}
