use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024;

/// Context-file candidates for one directory, in priority order: an
/// `AGENTS.override.md` replaces the plain files in its directory, and
/// `CLAUDE.md` is the fallback for projects written for other agents.
const DIR_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

#[derive(Clone)]
pub struct LoadedContext {
    pub files: Vec<String>,
    pub content: String,
}

impl LoadedContext {
    fn new(files: Vec<String>, content: String) -> Self {
        Self { files, content }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn remaining_budget(&self) -> usize {
        MAX_TOTAL_BYTES.saturating_sub(self.content.len())
    }

    pub fn merged(mut self, other: LoadedContext) -> LoadedContext {
        if other.content.is_empty() {
            return self;
        }
        if !self.content.is_empty() {
            self.content.push('\n');
        }
        self.content.push_str(&other.content);
        self.files.extend(other.files);
        self
    }
}

/// Project context files: the global `AGENTS.md` from the app config dir,
/// then the context file of every ancestor directory of `root` up to the
/// filesystem root — at most one file per directory, outermost directory
/// first. Cheap enough to re-run per request, so edits apply without a
/// restart.
pub fn load_agents_md(root: &Path) -> LoadedContext {
    let global = crate::config::config_dir().ok();
    load_agents_md_from(root, global.as_deref())
}

fn load_agents_md_from(root: &Path, global: Option<&Path>) -> LoadedContext {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    load_from_dirs(&context_dirs(&root, global), &root)
}

/// The directories whose context files are loaded, in order: the global
/// config dir first, then the ancestors of `root` from the filesystem root
/// down to `root` itself.
fn context_dirs(root: &Path, global: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(global) = global {
        dirs.push(global.to_path_buf());
    }
    let mut walked: Vec<PathBuf> = Vec::new();
    let mut current = root.to_path_buf();
    loop {
        walked.push(current.clone());
        let Some(parent) = current.parent() else {
            break;
        };
        current = parent.to_path_buf();
    }
    walked.reverse();
    dirs.extend(walked);
    dirs
}

fn load_from_dirs(dirs: &[PathBuf], root: &Path) -> LoadedContext {
    let mut files: Vec<String> = Vec::new();
    let mut content = String::new();
    let mut remaining = MAX_TOTAL_BYTES;
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for dir in dirs {
        if remaining == 0 {
            break;
        }
        let Some(path) = DIR_CANDIDATES
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| candidate.is_file())
        else {
            continue;
        };
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical) {
            continue;
        }
        push_file(
            &label_for(&path, root),
            &path,
            &mut files,
            &mut content,
            &mut remaining,
        );
    }
    LoadedContext::new(files, content)
}

/// Label for a loaded file: the path relative to the workspace root when the
/// file is inside it, otherwise the absolute path.
fn label_for(path: &Path, root: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().into_owned(),
        Err(_) => path.to_string_lossy().into_owned(),
    }
}

pub fn load_context_dir(root: &Path, budget: usize) -> LoadedContext {
    let mut files: Vec<String> = Vec::new();
    let mut content = String::new();
    let mut remaining = budget;

    let context_dir = root
        .join(crate::config::LOCAL_CONFIG_DIR_NAME)
        .join("context");
    if context_dir.is_dir() && remaining > 0 {
        let mut paths: Vec<PathBuf> = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&context_dir) {
            for entry in entries.flatten() {
                if entry.path().is_file() {
                    paths.push(entry.path());
                }
            }
        }
        paths.sort();
        for path in paths {
            if remaining == 0 {
                break;
            }
            let rel = match path.strip_prefix(root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) => path.to_string_lossy().into_owned(),
            };
            push_file(&rel, &path, &mut files, &mut content, &mut remaining);
        }
    }

    LoadedContext::new(files, content)
}

pub fn load(root: &Path) -> LoadedContext {
    let agents = load_agents_md(root);
    let dir = load_context_dir(root, agents.remaining_budget());
    agents.merged(dir)
}

fn push_file(
    rel: &str,
    path: &Path,
    files: &mut Vec<String>,
    content: &mut String,
    remaining: &mut usize,
) {
    if *remaining == 0 {
        return;
    }
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(_) => return,
    };
    if data.contains(&0) {
        return;
    }
    let truncated = data.len() > MAX_FILE_BYTES;
    let capped = &data[..data.len().min(MAX_FILE_BYTES)];
    let lossy = String::from_utf8_lossy(capped);
    let text = match lossy.strip_prefix('\u{FEFF}') {
        Some(stripped) => stripped,
        None => &lossy,
    };
    let chunk = if text.len() <= *remaining {
        text.to_string()
    } else {
        let mut end = *remaining;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    };
    if chunk.is_empty() {
        return;
    }
    *remaining -= chunk.len();
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str(&format!("=== {rel} ===\n{chunk}"));
    if truncated {
        content.push_str("\n… (file truncated)");
    }
    if *remaining == 0 {
        content.push_str("\n… (context budget exhausted)");
    }
    files.push(rel.to_string());
}

pub fn load_from_cwd() -> LoadedContext {
    match std::env::current_dir() {
        Ok(cwd) => load(&cwd),
        Err(_) => LoadedContext::new(Vec::new(), String::new()),
    }
}

pub fn build_preamble(base: &str, context: &LoadedContext) -> String {
    if context.is_empty() {
        base.to_string()
    } else {
        format!(
            "{base}\n\nProject context files are loaded from the workspace. Read them carefully \
             and follow their instructions; they describe the project's structure, conventions, \
             and workflows.\n\n{}",
            context.content
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tempdir() -> (TempDir, crate::test_util::CwdGuard) {
        let guard = crate::test_util::lock_cwd();
        let dir = TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn override_beats_agents_and_claude_in_one_dir() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("AGENTS.md"), "agents");
        write(&dir.path().join("CLAUDE.md"), "claude");
        write(&dir.path().join("AGENTS.override.md"), "override");

        let ctx = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert_eq!(ctx.files, vec!["AGENTS.override.md"]);
        assert!(ctx.content.contains("=== AGENTS.override.md ===\noverride"));
        assert!(!ctx.content.contains("agents"));
        assert!(!ctx.content.contains("claude"));
    }

    #[test]
    fn claude_md_is_the_fallback() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("CLAUDE.md"), "claude");

        let ctx = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert_eq!(ctx.files, vec!["CLAUDE.md"]);
        assert!(ctx.content.contains("=== CLAUDE.md ===\nclaude"));
    }

    #[test]
    fn outermost_dir_loaded_first() {
        let base = TempDir::new().unwrap();
        let outer = base.path().join("outer");
        let inner = outer.join("inner");
        write(&outer.join("AGENTS.md"), "outer rules");
        write(&inner.join("AGENTS.md"), "inner rules");

        let ctx = load_from_dirs(&[outer.clone(), inner.clone()], base.path());
        assert_eq!(ctx.files, vec!["outer/AGENTS.md", "outer/inner/AGENTS.md"]);
        assert!(
            ctx.content.find("outer rules").unwrap() < ctx.content.find("inner rules").unwrap()
        );
    }

    #[test]
    fn context_dirs_global_first_then_ancestors() {
        let base = std::fs::canonicalize(TempDir::new().unwrap().path()).unwrap();
        let global = base.join("g");
        let root = base.join("a/b");
        std::fs::create_dir_all(&root).unwrap();

        let dirs = context_dirs(&root, Some(&global));
        assert_eq!(dirs.first(), Some(&global));
        assert!(dirs.ends_with(&[base.clone(), base.join("a"), root.clone()]));
    }

    #[test]
    fn global_loaded_before_workspace_file() {
        let base = TempDir::new().unwrap();
        let global = base.path().join("g");
        std::fs::create_dir_all(&global).unwrap();
        write(&global.join("AGENTS.md"), "global rules");
        let root = base.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        write(&root.join("AGENTS.md"), "project rules");

        let ctx = load_agents_md_from(&root, Some(&global));
        assert_eq!(
            ctx.files.first(),
            Some(&global.join("AGENTS.md").to_string_lossy().into_owned())
        );
        assert!(ctx.files.ends_with(&["AGENTS.md".to_string()]));
        assert!(
            ctx.content.find("global rules").unwrap() < ctx.content.find("project rules").unwrap()
        );
    }

    #[test]
    fn ancestor_walk_collects_outermost_first() {
        let base = std::fs::canonicalize(TempDir::new().unwrap().path()).unwrap();
        write(&base.join("AGENTS.md"), "base rules");
        let root = base.join("proj/sub");
        std::fs::create_dir_all(&root).unwrap();
        write(&base.join("proj/AGENTS.md"), "proj rules");

        let ctx = load_agents_md_from(&root, None);
        let base_label = base.join("AGENTS.md").to_string_lossy().into_owned();
        let proj_label = base.join("proj/AGENTS.md").to_string_lossy().into_owned();
        let base_pos = ctx.files.iter().position(|f| f == &base_label).unwrap();
        let proj_pos = ctx.files.iter().position(|f| f == &proj_label).unwrap();
        assert!(base_pos < proj_pos);
        assert!(ctx.content.find("base rules").unwrap() < ctx.content.find("proj rules").unwrap());
    }

    #[test]
    fn labels_relative_inside_root_absolute_outside() {
        let base = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        write(&base.path().join("inside/AGENTS.md"), "in");
        write(&outside.path().join("AGENTS.md"), "out");

        let ctx = load_from_dirs(
            &[base.path().join("inside"), outside.path().to_path_buf()],
            base.path(),
        );
        assert_eq!(
            ctx.files,
            vec![
                "inside/AGENTS.md".to_string(),
                outside
                    .path()
                    .join("AGENTS.md")
                    .to_string_lossy()
                    .into_owned(),
            ]
        );
    }

    #[test]
    fn repeated_dir_loads_once() {
        let dir = TempDir::new().unwrap();
        write(&dir.path().join("AGENTS.md"), "rules");

        let ctx = load_from_dirs(
            &[dir.path().to_path_buf(), dir.path().to_path_buf()],
            dir.path(),
        );
        assert_eq!(ctx.files, vec!["AGENTS.md"]);
    }

    #[test]
    fn bom_is_stripped() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "\u{FEFF}rules").unwrap();

        let ctx = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert!(!ctx.content.contains('\u{FEFF}'));
        assert!(ctx.content.contains("=== AGENTS.md ===\nrules"));
    }

    #[test]
    fn budget_slices_on_char_boundary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("AGENTS.md");
        std::fs::write(&path, "日本語").unwrap();

        let mut files = Vec::new();
        let mut content = String::new();
        let mut remaining = 3usize;
        push_file("AGENTS.md", &path, &mut files, &mut content, &mut remaining);
        assert_eq!(
            content,
            "=== AGENTS.md ===\n日\n… (context budget exhausted)"
        );
        assert_eq!(remaining, 0);

        let mut files = Vec::new();
        let mut content = String::new();
        let mut remaining = 5usize;
        push_file("AGENTS.md", &path, &mut files, &mut content, &mut remaining);
        assert_eq!(content, "=== AGENTS.md ===\n日");
        assert_eq!(remaining, 2);

        let mut files = Vec::new();
        let mut content = String::new();
        let mut remaining = 2usize;
        push_file("AGENTS.md", &path, &mut files, &mut content, &mut remaining);
        assert!(content.is_empty());
        assert!(files.is_empty());
        assert_eq!(remaining, 2);
    }

    #[test]
    fn oversized_file_truncated() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "x".repeat(MAX_FILE_BYTES + 10),
        )
        .unwrap();

        let ctx = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert!(ctx.content.contains("… (file truncated)"));
        assert!(!ctx.content.contains("… (context budget exhausted)"));
    }

    #[test]
    fn total_budget_exhausted_across_files() {
        let dir = TempDir::new().unwrap();
        let mut dirs = Vec::new();
        for i in 0..5 {
            let sub = dir.path().join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            std::fs::write(sub.join("AGENTS.md"), "x".repeat(60 * 1024)).unwrap();
            dirs.push(sub);
        }

        let ctx = load_from_dirs(&dirs, dir.path());
        assert_eq!(ctx.files.len(), 5);
        assert!(ctx.content.contains("… (context budget exhausted)"));
    }

    #[test]
    fn loads_agents_md_and_context_dir_sorted() {
        let (dir, _guard) = tempdir();
        let app_dir = crate::config::LOCAL_CONFIG_DIR_NAME;
        std::fs::write("AGENTS.md", "project rules").unwrap();
        std::fs::create_dir_all(format!("{app_dir}/context")).unwrap();
        std::fs::write(format!("{app_dir}/context/b.md"), "bee").unwrap();
        std::fs::write(format!("{app_dir}/context/a.md"), "aye").unwrap();
        std::fs::write(format!("{app_dir}/context/skip.bin"), "\0binary").unwrap();

        let ctx = load_from_cwd();
        assert!(ctx.files.ends_with(&[
            "AGENTS.md".to_string(),
            format!("{app_dir}/context/a.md"),
            format!("{app_dir}/context/b.md"),
        ]));
        assert!(ctx.content.contains("=== AGENTS.md ===\nproject rules"));
        assert!(
            ctx.content
                .contains(&format!("=== {app_dir}/context/a.md ===\naye"))
        );
        assert!(
            ctx.content
                .contains(&format!("=== {app_dir}/context/b.md ===\nbee"))
        );
        assert!(!ctx.content.contains("binary"));
        drop(dir);
    }

    #[test]
    fn missing_agents_only_context_dir() {
        let (dir, _guard) = tempdir();
        let app_dir = crate::config::LOCAL_CONFIG_DIR_NAME;
        std::fs::create_dir_all(format!("{app_dir}/context")).unwrap();
        std::fs::write(format!("{app_dir}/context/notes.md"), "notes").unwrap();
        let ctx = load_from_cwd();
        assert!(
            ctx.files
                .ends_with(&[format!("{app_dir}/context/notes.md")])
        );
        assert!(ctx.content.contains("notes"));
        drop(dir);
    }

    #[test]
    fn loads_agents_md_separately_from_context_dir() {
        let dir = TempDir::new().unwrap();
        let app_dir = crate::config::LOCAL_CONFIG_DIR_NAME;
        write(&dir.path().join("AGENTS.md"), "project rules");
        write(&dir.path().join(app_dir).join("context/a.md"), "aye");

        let agents = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert_eq!(agents.files, vec!["AGENTS.md"]);
        assert!(agents.content.contains("project rules"));
        assert!(!agents.content.contains("aye"));

        let dir_ctx = load_context_dir(dir.path(), agents.remaining_budget());
        assert_eq!(dir_ctx.files, vec![format!("{app_dir}/context/a.md")]);
        assert!(dir_ctx.content.contains("aye"));

        let merged = agents.merged(dir_ctx);
        assert_eq!(
            merged.files,
            vec!["AGENTS.md".to_string(), format!("{app_dir}/context/a.md")]
        );
        assert!(merged.content.contains("project rules"));
        assert!(merged.content.contains("aye"));
    }

    #[test]
    fn empty_without_files() {
        let dir = TempDir::new().unwrap();
        let ctx = load_from_dirs(&[dir.path().to_path_buf()], dir.path());
        assert!(ctx.is_empty());
        assert!(ctx.files.is_empty());
        assert!(ctx.content.is_empty());
    }

    #[test]
    fn build_preamble_appends_context() {
        let ctx = LoadedContext::new(vec!["a".into(), "b".into()], "content".into());
        let preamble = build_preamble("base", &ctx);
        assert!(preamble.contains("base"));
        assert!(preamble.contains("content"));

        let preamble = build_preamble("base", &LoadedContext::new(Vec::new(), String::new()));
        assert_eq!(preamble, "base");
    }
}
