use std::path::{Path, PathBuf};

/// How a command line must be passed to the resolved shell executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// Bourne-style shells (`sh`, `bash`, `zsh`, …): `shell -c <command>`.
    Bourne,
    /// PowerShell (`powershell`, `pwsh`): `shell -NoProfile -Command <command>`.
    PowerShell,
}

/// A resolved shell for `run_shell`: the executable path plus how the command
/// line is handed to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shell {
    pub path: PathBuf,
    pub kind: ShellKind,
}

impl Shell {
    fn new(path: PathBuf) -> Self {
        Self {
            kind: kind_for(&path),
            path,
        }
    }

    /// Human-readable invocation used in the tool description.
    pub fn invocation(&self) -> String {
        match self.kind {
            ShellKind::Bourne => format!("{} -c", self.path.display()),
            ShellKind::PowerShell => format!("{} -NoProfile -Command", self.path.display()),
        }
    }

    /// Applies the command-line argument style of this shell to the builder.
    pub fn apply(&self, cmd: &mut tokio::process::Command, command: &str) {
        match self.kind {
            ShellKind::Bourne => {
                cmd.arg("-c").arg(command);
            }
            ShellKind::PowerShell => {
                cmd.arg("-NoProfile").arg("-Command").arg(command);
            }
        }
    }
}

fn kind_for(path: &Path) -> ShellKind {
    // Take the trailing component after *both* separators so Windows-style
    // paths (`C:\...\pwsh.exe`) are recognized on every platform, then strip
    // a trailing `.exe`.
    let name = path
        .as_os_str()
        .to_string_lossy()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let stem = name.strip_suffix(".exe").unwrap_or(&name);
    if stem == "pwsh" || stem.contains("powershell") {
        ShellKind::PowerShell
    } else {
        ShellKind::Bourne
    }
}

/// The outcome of resolving the shell for `run_shell`: the shell to use plus
/// a warning when a user-configured path had to be replaced by the default.
pub struct Resolution {
    pub shell: Shell,
    pub warning: Option<String>,
}

/// Resolves the shell to run commands with. A configured `path` wins when the
/// executable exists (directly, or as a bare name found in `PATH`); otherwise
/// the platform default is used and a warning reported.
pub fn resolve(configured: Option<&str>) -> Resolution {
    let configured = configured.filter(|p| !p.trim().is_empty());
    if let Some(path) = configured {
        if let Some(shell) = locate(path) {
            return Resolution {
                shell,
                warning: None,
            };
        }
        let default = default_shell();
        return Resolution {
            warning: Some(format!(
                "configured shell '{path}' was not found; falling back to the default shell ({}). \
                 Fix `shell.path` in config.kdl or remove it to use the default.",
                default.path.display()
            )),
            shell: default,
        };
    }
    Resolution {
        shell: default_shell(),
        warning: None,
    }
}

/// Finds a configured shell: an absolute/relative path used as-is, a bare name
/// searched in `PATH`.
fn locate(path: &str) -> Option<Shell> {
    let candidate = Path::new(path);
    if candidate.components().count() > 1 && is_executable_file(candidate) {
        return Some(Shell::new(candidate.to_path_buf()));
    }
    find_in_path(path).map(Shell::new)
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let dirs = std::env::var_os("PATH")?;
    find_in_dirs(name, std::env::split_paths(&dirs))
}

fn find_in_dirs<I>(name: &str, dirs: I) -> Option<PathBuf>
where
    I: IntoIterator<Item = PathBuf>,
{
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Platform default: Linux `bash` → `sh`; macOS `bash` → `zsh` → `sh`;
/// Windows Git Bash → `bash.exe` in `PATH` → PowerShell (`pwsh.exe` →
/// `powershell.exe`).
#[cfg(unix)]
fn default_shell() -> Shell {
    #[cfg(target_os = "macos")]
    const CANDIDATES: &[&str] = &["bash", "zsh", "sh"];
    #[cfg(all(unix, not(target_os = "macos")))]
    const CANDIDATES: &[&str] = &["bash", "sh"];

    for name in CANDIDATES {
        if let Some(path) = find_in_path(name) {
            return Shell::new(path);
        }
    }
    Shell::new(PathBuf::from("sh"))
}

#[cfg(windows)]
fn default_shell() -> Shell {
    for root in git_bash_roots() {
        let candidate = root.join("bin").join("bash.exe");
        if is_executable_file(&candidate) {
            return Shell::new(candidate);
        }
    }
    for name in ["bash.exe", "pwsh.exe", "powershell.exe"] {
        if let Some(path) = find_in_path(name) {
            return Shell::new(path);
        }
    }
    Shell::new(PathBuf::from("powershell.exe"))
}

#[cfg(windows)]
fn git_bash_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from(r"C:\Program Files\Git"),
        PathBuf::from(r"C:\Program Files (x86)\Git"),
    ];
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("Programs").join("Git"));
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_detected_from_executable_name() {
        assert_eq!(kind_for(Path::new("/usr/bin/bash")), ShellKind::Bourne);
        assert_eq!(kind_for(Path::new("/bin/sh")), ShellKind::Bourne);
        assert_eq!(kind_for(Path::new("/usr/bin/zsh")), ShellKind::Bourne);
        assert_eq!(
            kind_for(Path::new(r"C:\Program Files\PowerShell\7\pwsh.exe")),
            ShellKind::PowerShell
        );
        assert_eq!(
            kind_for(Path::new(
                r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            ShellKind::PowerShell
        );
    }

    #[test]
    fn apply_uses_bourne_flags() {
        let shell = Shell::new(PathBuf::from("/bin/sh"));
        let mut cmd = tokio::process::Command::new(&shell.path);
        shell.apply(&mut cmd, "echo hi");
        assert_eq!(
            cmd.as_std().get_args().collect::<Vec<_>>(),
            ["-c", "echo hi"]
        );
    }

    #[test]
    fn apply_uses_powershell_flags() {
        let shell = Shell::new(PathBuf::from(r"C:\Windows\System32\pwsh.exe"));
        let mut cmd = tokio::process::Command::new(&shell.path);
        shell.apply(&mut cmd, "echo hi");
        assert_eq!(
            cmd.as_std().get_args().collect::<Vec<_>>(),
            ["-NoProfile", "-Command", "echo hi"]
        );
    }

    #[test]
    fn resolve_defaults_without_config() {
        let resolution = resolve(None);
        assert!(resolution.warning.is_none());
        assert!(!resolution.shell.path.as_os_str().is_empty());
    }

    #[test]
    fn resolve_blank_config_uses_default() {
        for configured in ["", "   "] {
            let resolution = resolve(Some(configured));
            assert!(resolution.warning.is_none(), "{configured}");
            assert!(!resolution.shell.path.as_os_str().is_empty());
        }
    }

    #[test]
    fn resolve_missing_configured_path_warns_and_falls_back() {
        let default = resolve(None).shell;
        let resolution = resolve(Some("/definitely/not/a/real/shell"));
        assert!(resolution.warning.is_some());
        assert_eq!(resolution.shell, default);
        let warning = resolution.warning.unwrap();
        assert!(
            warning.contains("/definitely/not/a/real/shell"),
            "{warning}"
        );
        assert!(
            warning.contains(default.path.to_string_lossy().as_ref()),
            "{warning}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_absolute_path_wins() {
        let resolution = resolve(Some("/bin/sh"));
        assert!(resolution.warning.is_none());
        assert_eq!(resolution.shell.path, PathBuf::from("/bin/sh"));
    }

    #[test]
    fn find_in_dirs_locates_executable() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("shuvarie-fake-shell");
        std::fs::write(&script, "").unwrap();
        set_executable(&script);
        let found = find_in_dirs("shuvarie-fake-shell", [dir.path().to_path_buf()]);
        assert_eq!(found, Some(script));
        assert_eq!(
            find_in_dirs("shuvarie-missing-shell", [dir.path().to_path_buf()]),
            None
        );
    }

    #[cfg(unix)]
    fn set_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    #[cfg(windows)]
    fn set_executable(_path: &Path) {}
}
