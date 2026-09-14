use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use regex::Regex;
use shuvarie_config::{
    Mode, PathRule as PathRuleConfig, PermissionsConfig, ShellPatternKind,
    ShellRule as ShellRuleConfig, Verb,
};
use tokio::sync::{mpsc, oneshot};

/// Hidden components at the top of the workspace that stay exempt from the
/// `except-hidden` filter: agent-owned skill dirs and the app's data dir.
const HIDDEN_ROOT_EXEMPT: [&str; 2] = [".agents", shuvarie_config::WORKSPACE_DIR_NAME];

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

/// Canonicalizes a read path (symlinks resolved). The permission engine, not
/// this function, decides whether the path may be read.
pub(crate) fn resolve_read(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(expand_home(path));
    joined.canonicalize().map_err(|e| format!("{path}: {e}"))
}

/// Canonicalizes a write path, supporting targets that do not exist yet (the
/// closest existing ancestor is canonicalized, missing components appended).
/// The permission engine decides whether the target may be written.
pub(crate) fn resolve_write(path: &str) -> Result<PathBuf, String> {
    let root = workspace_root()?;
    let joined = root.join(expand_home(path));
    if joined.exists() {
        return joined.canonicalize().map_err(|e| format!("{path}: {e}"));
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
    Ok(abs)
}

/// Which side of a file tool is being authorized. Only the app-owned read
/// exemption (global skill dirs) is kind-sensitive; path rules govern reads
/// and writes alike unless the rule sets `mode="ro"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Read,
    Write,
}

impl PathKind {
    fn action(self) -> &'static str {
        match self {
            Self::Read => "reading",
            Self::Write => "writing",
        }
    }
}

/// What a permission check decided, naming the rule that decided it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

/// A pending permission ask: what the tool wants to run plus how the core
/// task answers it.
pub struct PermissionRequest {
    pub description: String,
    pub respond: oneshot::Sender<bool>,
}

/// Signals that a permission denial should cut the agent turn: set by the
/// authorize layer on every rule deny and user rejection, consumed by the
/// stream task right after the denied tool's result is persisted, so the turn
/// ends like a user cancel instead of continuing past the denial.
#[derive(Clone, Default)]
pub struct DenyCut(std::sync::Arc<AtomicBool>);

impl DenyCut {
    pub fn trigger(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Consumes the signal, reporting whether it was set.
    pub fn take(&self) -> bool {
        self.0.swap(false, Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The channel the tools pause on for `ask` verdicts; the core task forwards
/// requests to the TUI as [`crate::Event::PermissionRequested`] and resolves
/// them with [`crate::Command::PermissionDecide`].
#[derive(Clone)]
pub struct PermissionGate {
    tx: mpsc::Sender<PermissionRequest>,
}

impl PermissionGate {
    pub fn new(tx: mpsc::Sender<PermissionRequest>) -> Self {
        Self { tx }
    }

    /// Blocks until the user allows or denies. `Ok(false)` is a user denial;
    /// a closed channel or a dropped responder is an error (the turn is being
    /// torn down already).
    pub async fn request(&self, description: String) -> Result<bool, String> {
        let (respond, rx) = oneshot::channel();
        self.tx
            .send(PermissionRequest {
                description,
                respond,
            })
            .await
            .map_err(|_| "permission channel closed".to_string())?;
        rx.await
            .map_err(|_| "permission responder dropped".to_string())
    }
}

/// One compiled `paths` rule.
struct CompiledPathRule {
    verb: Verb,
    raw: String,
    path: PathBuf,
    exact: bool,
    except_hidden: bool,
    mode: Mode,
}

/// One compiled `shell-patterns` rule.
struct CompiledShellRule {
    verb: Verb,
    raw: String,
    matcher: ShellMatcher,
}

#[derive(Clone)]
enum ShellMatcher {
    /// Literal text with word boundaries, matched against the command line
    /// with whitespace runs collapsed.
    Raw(String),
    /// A regular expression, matched as written against the command line.
    Regex(Regex),
}

/// Compiled `permissions` rules, built once at startup from config (or the
/// built-in defaults when the section is absent). Rules evaluate in
/// declaration order and the first match decides; unmatched requests fall
/// back to the scope's bare verb, then the top-level verb, then `ask`.
pub struct Permissions {
    default: Verb,
    paths_default: Option<Verb>,
    shell_default: Option<Verb>,
    paths: Vec<CompiledPathRule>,
    shell: Vec<CompiledShellRule>,
    /// App-owned skill directories outside the workspace that stay readable.
    read_exempt: Vec<PathBuf>,
}

impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permissions")
            .field("default", &self.default)
            .field("paths_default", &self.paths_default)
            .field("shell_default", &self.shell_default)
            .field("path_rules", &self.paths.len())
            .field("shell_rules", &self.shell.len())
            .finish()
    }
}

impl Permissions {
    /// Compiles the config's permission rules against `workspace_root`.
    /// Fails on an invalid `regex` pattern.
    pub fn build(config: &PermissionsConfig, workspace_root: &Path) -> Result<Self, String> {
        let paths = config
            .paths
            .rules
            .iter()
            .map(|rule| CompiledPathRule::build(rule, workspace_root))
            .collect::<Result<Vec<_>, String>>()?;
        let shell = config
            .shell
            .rules
            .iter()
            .map(CompiledShellRule::build)
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            default: config.default.unwrap_or(Verb::Ask),
            paths_default: config.paths.default,
            shell_default: config.shell.default,
            paths,
            shell,
            read_exempt: crate::skills::global_skill_dirs(
                dirs::home_dir().as_deref(),
                shuvarie_config::config_dir().ok().as_deref(),
            ),
        })
    }

    /// The decision for a canonicalized file path. `mode="ro"` rules match
    /// only read requests; writes skip them and fall through to later rules.
    pub fn check_path(&self, kind: PathKind, path: &Path) -> Decision {
        if kind == PathKind::Read && self.read_exempt.iter().any(|root| path.starts_with(root)) {
            return Decision::Allow;
        }
        for rule in &self.paths {
            if rule.mode == Mode::Ro && kind == PathKind::Write {
                continue;
            }
            if let Some(decision) = rule.check(path) {
                return decision;
            }
        }
        let (fallback, reason) = match self.paths_default {
            Some(verb) => (verb, format!("paths fallback: {}-all", verb.as_str())),
            None => (
                self.default,
                format!("permissions default: {}-all", self.default.as_str()),
            ),
        };
        verb_decision(fallback, reason)
    }

    /// The decision for a `run_shell` command line: only `shell-patterns`
    /// rules decide commands — path rules never gate shell text — then the
    /// fallback verbs.
    pub fn check_shell(&self, command: &str) -> Decision {
        let collapsed = collapse_whitespace(command);
        for rule in &self.shell {
            if rule.matches(command, &collapsed) {
                return verb_decision(
                    rule.verb,
                    format!("shell-patterns: {} \"{}\"", rule.verb.as_str(), rule.raw),
                );
            }
        }
        let (fallback, reason) = match self.shell_default {
            Some(verb) => (
                verb,
                format!("shell-patterns fallback: {}-all", verb.as_str()),
            ),
            None => (
                self.default,
                format!("permissions default: {}-all", self.default.as_str()),
            ),
        };
        verb_decision(fallback, reason)
    }

    /// Runs a permission check and, for `ask` decisions, pauses on `gate`
    /// until the user answers. Every denial — a matched `deny` rule or a user
    /// rejection — triggers `cut` (the turn ends like a user cancel) and is
    /// an error naming the matched rule.
    pub async fn authorize_path(
        &self,
        gate: &PermissionGate,
        cut: &DenyCut,
        kind: PathKind,
        path: &Path,
        display: &str,
    ) -> Result<(), String> {
        match self.check_path(kind, path) {
            Decision::Allow => Ok(()),
            Decision::Deny { reason } => {
                cut.trigger();
                Err(format!("permission denied: {reason}"))
            }
            Decision::Ask { reason } => {
                let description = format!("Allow {} `{display}`?\n{reason}", kind.action());
                match gate.request(description).await {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        cut.trigger();
                        Err(format!("permission denied by the user: {reason}"))
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// The `ask`-aware version of [`Self::check_shell`].
    pub async fn authorize_shell(
        &self,
        gate: &PermissionGate,
        cut: &DenyCut,
        command: &str,
    ) -> Result<(), String> {
        match self.check_shell(command) {
            Decision::Allow => Ok(()),
            Decision::Deny { reason } => {
                cut.trigger();
                Err(format!("permission denied: {reason}"))
            }
            Decision::Ask { reason } => {
                let description = format!("Allow running this command?\n{command}\n{reason}");
                match gate.request(description).await {
                    Ok(true) => Ok(()),
                    Ok(false) => {
                        cut.trigger();
                        Err(format!("permission denied by the user: {reason}"))
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// The output watcher for `deny` shell rules: reports the first rule
    /// whose pattern matches captured command output. Any `deny` rule cuts
    /// the turn when its pattern trips the output, so every deny rule is
    /// watched.
    pub fn output_interrupt(&self) -> Option<OutputInterrupt> {
        let rules = self
            .shell
            .iter()
            .filter(|rule| rule.verb == Verb::Deny)
            .map(|rule| (rule.matcher.clone(), rule.raw.clone()))
            .collect::<Vec<_>>();
        (!rules.is_empty()).then_some(OutputInterrupt { rules })
    }
}

impl CompiledPathRule {
    fn build(rule: &PathRuleConfig, root: &Path) -> Result<Self, String> {
        let expanded = expand_home(&rule.path);
        let candidate = if Path::new(&expanded).is_absolute() {
            PathBuf::from(&expanded)
        } else {
            root.join(&expanded)
        };
        let normalized = normalize(&candidate);
        let path = std::fs::canonicalize(&normalized).unwrap_or(normalized);
        Ok(Self {
            verb: rule.verb,
            raw: rule.path.clone(),
            path,
            exact: rule.exact,
            except_hidden: rule.except_hidden,
            mode: rule.mode,
        })
    }

    fn check(&self, path: &Path) -> Option<Decision> {
        if !self.matches(path) {
            return None;
        }
        Some(verb_decision(
            self.verb,
            format!("paths: {} \"{}\"", self.verb.as_str(), self.raw),
        ))
    }

    fn matches(&self, path: &Path) -> bool {
        let matched = if self.exact {
            path == self.path
        } else {
            path.starts_with(&self.path)
        };
        if !matched {
            return false;
        }
        if !self.except_hidden {
            return true;
        }
        let rel = path.strip_prefix(&self.path).unwrap_or(path);
        rel.components().enumerate().all(|(i, comp)| {
            let s = comp.as_os_str().to_string_lossy();
            !(s.starts_with('.') && s != "." && s != "..")
                || (i == 0 && HIDDEN_ROOT_EXEMPT.contains(&&*s))
        })
    }
}

impl CompiledShellRule {
    fn build(rule: &ShellRuleConfig) -> Result<Self, String> {
        let matcher = match rule.kind {
            ShellPatternKind::Raw => ShellMatcher::Raw(collapse_whitespace(&rule.pattern)),
            ShellPatternKind::Regex => {
                let re = Regex::new(&rule.pattern).map_err(|e| {
                    format!(
                        "invalid regex in shell-patterns rule \"{}\": {e}",
                        rule.pattern
                    )
                })?;
                ShellMatcher::Regex(re)
            }
        };
        Ok(Self {
            verb: rule.verb,
            raw: rule.pattern.clone(),
            matcher,
        })
    }

    fn matches(&self, command: &str, collapsed: &str) -> bool {
        match &self.matcher {
            ShellMatcher::Raw(needle) => contains_word(collapsed, needle),
            ShellMatcher::Regex(re) => re.is_match(command),
        }
    }
}

impl ShellMatcher {
    fn matches_text(&self, text: &str) -> bool {
        match self {
            Self::Raw(needle) => text.contains(needle),
            Self::Regex(re) => re.is_match(text),
        }
    }
}

/// The `deny` output watcher: reports the first rule whose pattern matches
/// captured command output.
pub struct OutputInterrupt {
    rules: Vec<(ShellMatcher, String)>,
}

impl OutputInterrupt {
    /// The matched rule's description, when the captured output trips one.
    pub fn check(&self, text: &str) -> Option<String> {
        self.rules.iter().find_map(|(matcher, raw)| {
            matcher
                .matches_text(text)
                .then(|| format!("shell-patterns: deny \"{raw}\" (matched command output)"))
        })
    }
}

fn verb_decision(verb: Verb, reason: String) -> Decision {
    match verb {
        Verb::Allow => Decision::Allow,
        Verb::Ask => Decision::Ask { reason },
        Verb::Deny => Decision::Deny { reason },
    }
}

/// The permission engine plus its ask gate and deny-cut signal, cloned into
/// every gated tool as one handle.
#[derive(Clone)]
pub struct Access {
    permissions: std::sync::Arc<Permissions>,
    gate: PermissionGate,
    deny_cut: DenyCut,
}

impl Access {
    pub fn new(
        permissions: std::sync::Arc<Permissions>,
        gate: PermissionGate,
        deny_cut: DenyCut,
    ) -> Self {
        Self {
            permissions,
            gate,
            deny_cut,
        }
    }

    /// Authorizes a canonicalized file path (see
    /// [`Permissions::authorize_path`]).
    pub async fn authorize_path(
        &self,
        kind: PathKind,
        path: &Path,
        display: &str,
    ) -> Result<(), String> {
        self.permissions
            .authorize_path(&self.gate, &self.deny_cut, kind, path, display)
            .await
    }

    /// Authorizes a `run_shell` command line (see
    /// [`Permissions::authorize_shell`]).
    pub async fn authorize_shell(&self, command: &str) -> Result<(), String> {
        self.permissions
            .authorize_shell(&self.gate, &self.deny_cut, command)
            .await
    }

    /// The `deny` output watcher, if any deny rules are configured.
    pub fn output_interrupt(&self) -> Option<OutputInterrupt> {
        self.permissions.output_interrupt()
    }

    /// Flags the turn for cutting; `run_shell` calls this when a deny rule
    /// trips the captured output of a running command.
    pub fn trigger_cut(&self) {
        self.deny_cut.trigger();
    }

    /// The turn-cut signal, shared with the stream task that consumes it.
    pub fn turn_cut(&self) -> &DenyCut {
        &self.deny_cut
    }
}

/// Collapses whitespace runs to single spaces so multi-word raw patterns
/// tolerate the command's own spacing.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        }
    }
    out
}

/// Substring match flanked by non-alphanumeric ASCII (or the string edges),
/// so `rm` matches `rm -rf` and `x;rm` but not `firm` or `rmrf`.
fn contains_word(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let at = start + pos;
        let end = at + needle.len();
        let left_ok = at == 0 || !haystack[..at].ends_with(|c: char| c.is_ascii_alphanumeric());
        let right_ok = end == haystack.len()
            || !haystack[end..].starts_with(|c: char| c.is_ascii_alphanumeric());
        if left_ok && right_ok {
            return true;
        }
        start = end;
    }
    false
}

/// Lexically resolves `.` and `..` (keeping `..` chains that climb past a
/// relative path's start, and clamping at a filesystem root).
fn normalize(path: &Path) -> PathBuf {
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    let mut absolute = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => parts.push(prefix.as_os_str().to_os_string()),
            Component::RootDir => {
                absolute = true;
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let popped = !parts.is_empty()
                    && parts.last().is_some_and(|p| p != "..")
                    && parts.pop().is_some();
                if !popped && !absolute {
                    parts.push("..".into());
                }
            }
            Component::Normal(name) => parts.push(name.to_os_string()),
        }
    }
    let mut out = PathBuf::new();
    if absolute {
        out.push("/");
    }
    for part in parts {
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_config::{
        Mode, PathRule, PermissionsConfig, RuleSet, ShellPatternKind, ShellRule, Verb,
    };

    fn builtin() -> Permissions {
        Permissions::build(&PermissionsConfig::builtin(), Path::new("/ws")).unwrap()
    }

    fn config(default: Option<Verb>, paths: Vec<PathRule>, shell: Vec<ShellRule>) -> Permissions {
        config_scoped(default, None, paths, None, shell)
    }

    fn config_scoped(
        default: Option<Verb>,
        paths_default: Option<Verb>,
        paths: Vec<PathRule>,
        shell_default: Option<Verb>,
        shell: Vec<ShellRule>,
    ) -> Permissions {
        Permissions::build(
            &PermissionsConfig {
                default,
                paths: RuleSet {
                    default: paths_default,
                    rules: paths,
                },
                shell: RuleSet {
                    default: shell_default,
                    rules: shell,
                },
            },
            Path::new("/ws"),
        )
        .unwrap()
    }

    fn path_rule(verb: Verb, path: &str) -> PathRule {
        PathRule {
            verb,
            path: path.to_string(),
            except_hidden: false,
            exact: false,
            mode: Mode::Rw,
        }
    }

    fn shell_rule(verb: Verb, pattern: &str) -> ShellRule {
        ShellRule {
            verb,
            pattern: pattern.to_string(),
            kind: ShellPatternKind::Raw,
        }
    }

    #[test]
    fn builtin_paths_allow_workspace_but_ask_hidden_and_outside() {
        let perms = builtin();
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/src/main.rs")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Write, Path::new("/ws/src/main.rs")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/.env")),
            Decision::Ask {
                reason: "permissions default: ask-all".into()
            }
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/etc/hosts")),
            Decision::Ask {
                reason: "permissions default: ask-all".into()
            }
        );
    }

    #[test]
    fn builtin_keeps_root_exemptions_and_global_skill_reads() {
        let perms = builtin();
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/.agents/skills/x")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(
                PathKind::Write,
                Path::new(&format!(
                    "/ws/{}/data.db",
                    shuvarie_config::WORKSPACE_DIR_NAME
                ))
            ),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/.agents/.secrets/key")),
            Decision::Ask {
                reason: "permissions default: ask-all".into()
            }
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/sub/.agents/x")),
            Decision::Ask {
                reason: "permissions default: ask-all".into()
            }
        );
        let Some(home) = dirs::home_dir() else {
            return;
        };
        assert_eq!(
            perms.check_path(PathKind::Read, &home.join(".agents/skills/tokio/SKILL.md")),
            Decision::Allow
        );
    }

    #[test]
    fn first_match_wins_with_scope_fallback() {
        let perms = config(
            Some(Verb::Allow),
            vec![
                path_rule(Verb::Ask, "/ws/secrets/public"),
                path_rule(Verb::Deny, "/ws/secrets"),
            ],
            vec![shell_rule(Verb::Ask, "make")],
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/secrets/key")),
            Decision::Deny {
                reason: "paths: deny \"/ws/secrets\"".into()
            }
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/secrets/public/x")),
            Decision::Ask {
                reason: "paths: ask \"/ws/secrets/public\"".into()
            }
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/other")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_shell("make build"),
            Decision::Ask {
                reason: "shell-patterns: ask \"make\"".into()
            }
        );
        assert_eq!(perms.check_shell("cargo test"), Decision::Allow);
    }

    #[test]
    fn exact_rules_match_only_the_path_itself() {
        let perms = config_scoped(
            Some(Verb::Deny),
            Some(Verb::Deny),
            vec![PathRule {
                verb: Verb::Allow,
                path: "/ws/file".into(),
                except_hidden: false,
                exact: true,
                mode: Mode::Rw,
            }],
            None,
            Vec::new(),
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/file")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/file/sub")),
            Decision::Deny {
                reason: "paths fallback: deny-all".into()
            }
        );
    }

    #[test]
    fn except_hidden_skips_rules_for_hidden_targets() {
        let perms = config_scoped(
            Some(Verb::Deny),
            Some(Verb::Deny),
            vec![PathRule {
                verb: Verb::Allow,
                path: "/ws".into(),
                except_hidden: true,
                exact: false,
                mode: Mode::Rw,
            }],
            None,
            Vec::new(),
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/x")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/.env")),
            Decision::Deny {
                reason: "paths fallback: deny-all".into()
            }
        );
    }

    #[test]
    fn tilde_expands_at_build_time() {
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let perms = config_scoped(
            Some(Verb::Deny),
            Some(Verb::Deny),
            vec![path_rule(Verb::Allow, "~/.ssh")],
            None,
            Vec::new(),
        );
        assert_eq!(
            perms.check_path(PathKind::Read, &home.join(".ssh/id_rsa")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, &home.join(".ssh/hosts/d")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Read, &home.join(".sshrc")),
            Decision::Deny {
                reason: "paths fallback: deny-all".into()
            }
        );
    }

    #[test]
    fn raw_shell_patterns_match_with_word_boundaries() {
        let perms = config(
            Some(Verb::Allow),
            Vec::new(),
            vec![
                shell_rule(Verb::Ask, "rm"),
                shell_rule(Verb::Deny, "git push --force"),
            ],
        );
        for command in ["rm -rf /", "echo x;rm y", "sudo rm x"] {
            assert_eq!(
                perms.check_shell(command),
                Decision::Ask {
                    reason: "shell-patterns: ask \"rm\"".into()
                },
                "{command}"
            );
        }
        assert_eq!(perms.check_shell("firm -rf /"), Decision::Allow);
        assert_eq!(perms.check_shell("echo rmrf"), Decision::Allow);
        assert_eq!(
            perms.check_shell("git push --force origin main"),
            Decision::Deny {
                reason: "shell-patterns: deny \"git push --force\"".into()
            }
        );
        assert_eq!(
            perms.check_shell("git\tpush\n--force"),
            Decision::Deny {
                reason: "shell-patterns: deny \"git push --force\"".into()
            }
        );
        assert_eq!(perms.check_shell("git push"), Decision::Allow);
    }

    #[test]
    fn regex_rules_match_as_written() {
        let perms = Permissions::build(
            &PermissionsConfig {
                default: Some(Verb::Allow),
                paths: RuleSet::default(),
                shell: RuleSet {
                    default: None,
                    rules: vec![ShellRule {
                        verb: Verb::Deny,
                        pattern: "rm (-rf|-fr|--force --recursive)".to_string(),
                        kind: ShellPatternKind::Regex,
                    }],
                },
            },
            Path::new("/ws"),
        )
        .unwrap();
        for command in ["rm -rf /tmp", "rm --force --recursive /tmp"] {
            assert_eq!(
                perms.check_shell(command),
                Decision::Deny {
                    reason: "shell-patterns: deny \"rm (-rf|-fr|--force --recursive)\"".into()
                },
                "{command}"
            );
        }
        assert_eq!(perms.check_shell("rm /tmp"), Decision::Allow);
    }

    #[test]
    fn invalid_regex_fails_build() {
        let err = Permissions::build(
            &PermissionsConfig {
                default: None,
                paths: RuleSet::default(),
                shell: RuleSet {
                    default: None,
                    rules: vec![ShellRule {
                        verb: Verb::Deny,
                        pattern: "rm (".to_string(),
                        kind: ShellPatternKind::Regex,
                    }],
                },
            },
            Path::new("/ws"),
        )
        .unwrap_err();
        assert!(err.contains("invalid regex"), "{err}");
    }

    #[test]
    fn path_rules_never_gate_shell_commands() {
        let perms = config_scoped(
            Some(Verb::Ask),
            None,
            vec![
                PathRule {
                    verb: Verb::Deny,
                    path: "~/.ssh".into(),
                    except_hidden: false,
                    exact: false,
                    mode: Mode::Rw,
                },
                PathRule {
                    verb: Verb::Allow,
                    path: "/ws".into(),
                    except_hidden: false,
                    exact: false,
                    mode: Mode::Ro,
                },
            ],
            Some(Verb::Deny),
            Vec::new(),
        );
        assert_eq!(
            perms.check_shell("cat ~/.ssh/id_rsa"),
            Decision::Deny {
                reason: "shell-patterns fallback: deny-all".into()
            },
            "a path deny never bridges into shell text"
        );
        assert_eq!(
            perms.check_shell("cat /ws/file"),
            Decision::Deny {
                reason: "shell-patterns fallback: deny-all".into()
            },
            "a ro file allowance never grants a command"
        );
    }

    #[test]
    fn mode_ro_rules_match_reads_but_skip_writes() {
        let perms = config_scoped(
            Some(Verb::Deny),
            Some(Verb::Deny),
            vec![PathRule {
                verb: Verb::Allow,
                path: "/ws".into(),
                except_hidden: false,
                exact: false,
                mode: Mode::Ro,
            }],
            None,
            Vec::new(),
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/file")),
            Decision::Allow
        );
        assert_eq!(
            perms.check_path(PathKind::Write, Path::new("/ws/file")),
            Decision::Deny {
                reason: "paths fallback: deny-all".into()
            },
            "ro rules never decide writes"
        );
    }

    #[test]
    fn mode_ro_rules_fall_through_to_later_rules() {
        let perms = config_scoped(
            Some(Verb::Deny),
            None,
            vec![
                PathRule {
                    verb: Verb::Allow,
                    path: "/ws".into(),
                    except_hidden: false,
                    exact: false,
                    mode: Mode::Ro,
                },
                path_rule(Verb::Ask, "/ws"),
            ],
            None,
            Vec::new(),
        );
        assert_eq!(
            perms.check_path(PathKind::Read, Path::new("/ws/file")),
            Decision::Allow,
            "first matching ro rule wins for reads"
        );
        assert_eq!(
            perms.check_path(PathKind::Write, Path::new("/ws/file")),
            Decision::Ask {
                reason: "paths: ask \"/ws\"".into()
            },
            "writes reach the next rule"
        );
    }

    #[test]
    fn builtin_shell_allows_all_commands() {
        let perms = builtin();
        assert_eq!(perms.check_shell("rm -rf target"), Decision::Allow);
        assert_eq!(perms.check_shell("sudo apt install"), Decision::Allow);
        assert_eq!(perms.check_shell("cargo build"), Decision::Allow);
    }

    #[tokio::test]
    async fn authorize_triggers_the_cut_on_every_denial() {
        let (tx, rx) = mpsc::channel::<PermissionRequest>(8);
        let gate = PermissionGate::new(tx);
        let cut = DenyCut::default();
        let perms = std::sync::Arc::new(config_scoped(
            Some(Verb::Allow),
            Some(Verb::Ask),
            vec![],
            None,
            vec![shell_rule(Verb::Deny, "sudo")],
        ));
        let rx = std::sync::Arc::new(std::sync::Mutex::new(rx));
        let answer = |allow: bool| {
            let rx = rx.clone();
            async move {
                rx.lock()
                    .unwrap()
                    .recv()
                    .await
                    .unwrap()
                    .respond
                    .send(allow)
                    .ok()
            }
        };

        let cut1 = cut.clone();
        let err = perms
            .authorize_shell(&gate, &cut1, "sudo apt install")
            .await
            .unwrap_err();
        assert!(err.contains("shell-patterns: deny"), "{err}");
        assert!(cut1.is_set(), "rule deny triggers the cut");
        assert!(cut.take());

        let cut2 = cut.clone();
        let perms2 = perms.clone();
        let gate2 = gate.clone();
        let allow = answer(false);
        let denied = tokio::spawn(async move {
            perms2
                .authorize_path(
                    &gate2,
                    &cut2,
                    PathKind::Read,
                    Path::new("/etc/hosts"),
                    "/etc/hosts",
                )
                .await
        });
        allow.await.unwrap();
        let err = denied.await.unwrap().unwrap_err();
        assert!(err.contains("denied by the user"), "{err}");
        assert!(cut.is_set(), "user denial triggers the cut");
        assert!(cut.take());

        let cut3 = cut.clone();
        let perms3 = perms.clone();
        let gate3 = gate.clone();
        let allow = answer(true);
        let granted = tokio::spawn(async move {
            perms3
                .authorize_path(
                    &gate3,
                    &cut3,
                    PathKind::Read,
                    Path::new("/etc/hosts"),
                    "/etc/hosts",
                )
                .await
        });
        allow.await.unwrap();
        assert!(granted.await.unwrap().is_ok());
        assert!(!cut.is_set(), "allowing never triggers the cut");
    }

    #[test]
    fn output_interrupt_watches_every_deny_rule() {
        let perms = Permissions::build(
            &PermissionsConfig {
                default: Some(Verb::Allow),
                paths: RuleSet::default(),
                shell: RuleSet {
                    default: None,
                    rules: vec![
                        ShellRule {
                            verb: Verb::Deny,
                            pattern: "sudo".to_string(),
                            kind: ShellPatternKind::Raw,
                        },
                        ShellRule {
                            verb: Verb::Ask,
                            pattern: "secret".to_string(),
                            kind: ShellPatternKind::Raw,
                        },
                        ShellRule {
                            verb: Verb::Deny,
                            pattern: "rm (-rf|-fr)".to_string(),
                            kind: ShellPatternKind::Regex,
                        },
                    ],
                },
            },
            Path::new("/ws"),
        )
        .unwrap();
        let interrupt = perms.output_interrupt().expect("watcher built");
        assert_eq!(
            interrupt.check("Password: \nsudo: permission denied"),
            Some("shell-patterns: deny \"sudo\" (matched command output)".into())
        );
        assert_eq!(
            interrupt.check("error: rm -rf refused"),
            Some("shell-patterns: deny \"rm (-rf|-fr)\" (matched command output)".into())
        );
        assert_eq!(interrupt.check("plain output"), None);
        assert!(builtin().output_interrupt().is_none());
    }

    #[test]
    fn normalize_resolves_dot_components() {
        assert_eq!(normalize(Path::new("/ws/./a/../b")), PathBuf::from("/ws/b"));
        assert_eq!(normalize(Path::new("a/../b")), PathBuf::from("b"));
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
        assert_eq!(normalize(Path::new("../a")), PathBuf::from("../a"));
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
    }
}
