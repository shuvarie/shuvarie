use std::path::{Path, PathBuf};

const HIDDEN_ROOT_EXEMPT: [&str; 2] = [".agents", crate::config::LOCAL_CONFIG_DIR_NAME];

pub(crate) fn workspace_root() -> Result<PathBuf, String> {
    std::env::current_dir().map_err(|e| format!("cwd: {e}"))
}

pub(crate) fn expand_home(path: &str) -> String {
    if (path == "~" || path.starts_with("~/") || path.starts_with("~\\"))
        && let Some(home) = dirs::home_dir()
    {
        let rest = path
            .strip_prefix("~/")
            .or_else(|| path.strip_prefix("~\\"))
            .unwrap_or("");
        return home.join(rest).to_string_lossy().into_owned();
    }
    path.to_string()
}

fn hidden_component(rel: &Path) -> Option<String> {
    for (i, comp) in rel.components().enumerate() {
        let s = comp.as_os_str().to_string_lossy();
        if s.starts_with('.') && s != "." && s != ".." {
            if i == 0 && HIDDEN_ROOT_EXEMPT.contains(&&*s) {
                continue;
            }
            return Some(s.into_owned());
        }
    }
    None
}

pub(crate) fn resolve_read(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(expand_home(path));
    let canonical = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
    let rel = canonical.strip_prefix(&root).unwrap_or(&canonical);
    let exempt_roots = crate::skills::global_skill_dirs(
        dirs::home_dir().as_deref(),
        crate::config::config_dir().ok().as_deref(),
    );
    if let Some(comp) = hidden_component(rel)
        && !under_global_skill_dirs(&canonical, &exempt_roots)
    {
        return Err(format!(
            "'{path}' is under the hidden path '{comp}'; hidden files and directories cannot be read"
        ));
    }
    Ok(canonical)
}

fn under_global_skill_dirs(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

pub(crate) fn resolve_write(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(expand_home(path));
    if joined.exists() {
        let abs = joined.canonicalize().map_err(|e| format!("{path}: {e}"))?;
        check_write_target(&abs, &root, path)?;
        return Ok(abs);
    }
    let mut existing = joined.clone();
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        let name = existing
            .file_name()
            .ok_or_else(|| format!("{path}: invalid path"))?
            .to_os_string();
        missing.push(name);
        existing = existing
            .parent()
            .ok_or_else(|| format!("{path}: invalid path"))?
            .to_path_buf();
    }
    let mut abs = existing
        .canonicalize()
        .map_err(|e| format!("{path}: {e}"))?;
    for name in missing.iter().rev() {
        abs.push(name);
    }
    check_write_target(&abs, &root, path)?;
    Ok(abs)
}

fn check_write_target(abs: &Path, root: &Path, display: &str) -> Result<(), String> {
    let Ok(rel) = abs.strip_prefix(root) else {
        return Err(format!(
            "'{display}' resolves outside the working directory; writes outside the workspace are not permitted"
        ));
    };
    if let Some(comp) = hidden_component(rel) {
        return Err(format!(
            "'{display}' is under the hidden path '{comp}'; hidden files and directories cannot be written"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::lock_cwd;
    use tempfile::TempDir;

    fn tempdir() -> (TempDir, crate::test_util::CwdGuard) {
        let guard = lock_cwd();
        let dir = TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    #[test]
    fn read_blocks_hidden_paths() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir_all(".git").unwrap();
        std::fs::write(".git/config", "x").unwrap();
        assert!(resolve_read(".git/config").unwrap_err().contains("hidden"));
        assert!(resolve_read(".").is_ok());
        drop(dir);
    }

    #[test]
    fn read_permits_outside_workspace() {
        let (dir, _guard) = tempdir();
        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-readable-{}", std::process::id()));
        std::fs::write(&outside, "x").unwrap();
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(resolve_read(&rel).is_ok());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }

    #[test]
    fn write_blocks_hidden_and_outside() {
        let (dir, _guard) = tempdir();
        assert!(resolve_write(".env").unwrap_err().contains("hidden"));
        assert!(
            resolve_write("src/.hidden/f.txt")
                .unwrap_err()
                .contains("hidden")
        );
        let outside = dir
            .path()
            .parent()
            .unwrap()
            .join(format!("shuvarie-protected-{}", std::process::id()));
        let rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(
            resolve_write(&rel)
                .unwrap_err()
                .contains("outside the working directory"),
            "{}",
            resolve_write(&rel).unwrap_err()
        );
        assert!(resolve_write("src/f.txt").is_ok());
        let _ = std::fs::remove_file(&outside);
        drop(dir);
    }

    #[test]
    fn global_skill_dirs_exempt_from_hidden_rule() {
        let dir = TempDir::new().unwrap();
        let home = dir.path().join("home");
        let global_config = home.join(".config/shuvarie");
        let roots = crate::skills::global_skill_dirs(Some(&home), Some(&global_config));
        assert_eq!(
            roots,
            vec![global_config.join("skills"), home.join(".agents/skills"),]
        );
        assert!(under_global_skill_dirs(
            &global_config.join("skills/ratatui/SKILL.md"),
            &roots
        ));
        assert!(under_global_skill_dirs(
            &home.join(".agents/skills/tokio/SKILL.md"),
            &roots
        ));
        assert!(!under_global_skill_dirs(
            &global_config.join("connections.kdl"),
            &roots
        ));
        assert!(!under_global_skill_dirs(&home.join(".env"), &roots));
    }

    #[test]
    fn dot_agents_and_app_dir_are_exempt() {
        let (dir, _guard) = tempdir();
        let app_dir = crate::config::LOCAL_CONFIG_DIR_NAME;
        std::fs::create_dir_all(".agents").unwrap();
        std::fs::write(".agents/NOTE.md", "x").unwrap();
        std::fs::create_dir_all(app_dir).unwrap();
        std::fs::write(format!("{app_dir}/notes.md"), "x").unwrap();
        assert!(resolve_read("./.agents/NOTE.md").is_ok());
        assert!(resolve_read(&format!("./{app_dir}/notes.md")).is_ok());
        assert!(resolve_write(".agents/skills/new/SKILL.md").is_ok());
        assert!(resolve_write(&format!("{app_dir}/context/extra.md")).is_ok());
        std::fs::create_dir_all(".agents/.secrets").unwrap();
        std::fs::write(".agents/.secrets/key", "x").unwrap();
        assert!(
            resolve_read(".agents/.secrets/key")
                .unwrap_err()
                .contains("hidden")
        );
        assert!(
            resolve_write(".agents/.secrets/key")
                .unwrap_err()
                .contains("hidden")
        );
        drop(dir);
    }
}
