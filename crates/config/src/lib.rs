use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

mod config_kdl;
mod connections;
mod connections_kdl;
mod error;
mod kdl_util;

pub use self::connections::{Active, Connections, ProviderConfig};
pub use error::{ConfigError, ConfigParseError, Result};

const CONFIG_DIR_NAME: &str = if cfg!(debug_assertions) {
    "shuvarie-dev"
} else {
    "shuvarie"
};
const CONFIG_FILE_NAME: &str = "config.kdl";
const LOCAL_CONFIG_FILE_NAME: &str = if cfg!(debug_assertions) {
    "shuvarie-dev.kdl"
} else {
    "shuvarie.kdl"
};
pub const WORKSPACE_DIR_NAME: &str = if cfg!(debug_assertions) {
    ".shuvarie-dev"
} else {
    ".shuvarie"
};

/// One parsed config file, plus the top-level section names its file
/// actually defines — absent sections keep their defaults, so the node
/// list is what tells the merge step apart "absent" from "defined".
#[derive(Debug)]
struct ConfigLayer {
    config: Config,
    sections: HashSet<String>,
}

fn parse_layer(contents: &str) -> Result<ConfigLayer> {
    let (config, sections) = config_kdl::from_kdl_with_sections(contents)?;
    Ok(ConfigLayer {
        config,
        sections: sections.into_iter().collect(),
    })
}

fn read_layer(path: &std::path::Path) -> Result<Option<ConfigLayer>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some(parse_layer(&contents)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io(e)),
    }
}

/// Merges `layer` into `config`. Per top-level section the layer either wins
/// wholesale (its file defines the node) or is ignored entirely (it does
/// not), so locally-set sections intentionally reset untouched fields of that
/// section to defaults. `lsp.servers` merges key-by-key so a file adding one
/// server doesn't shadow the rest; `registries` merges key-by-key per
/// registry name for the same reason. `permissions` is stacked separately by
/// [`stack_permissions`] once the whole chain has been read.
fn merge_layer(config: &mut Config, layer: &ConfigLayer) {
    for section in &layer.sections {
        match section.as_str() {
            "ui" => config.ui = layer.config.ui.clone(),
            "embedding" => config.embedding = layer.config.embedding.clone(),
            "agent" => config.agent = layer.config.agent.clone(),
            "skills" => config.skills = layer.config.skills.clone(),
            "context" => config.context = layer.config.context.clone(),
            "shell" => config.shell = layer.config.shell.clone(),
            "registries" => {
                for (name, entry) in &layer.config.registries.entries {
                    config.registries.entries.insert(name.clone(), *entry);
                }
            }
            "tools" => config.tools = layer.config.tools.clone(),
            "lsp" => {
                config.lsp.disabled = layer.config.lsp.disabled;
                for (lang, spec) in &layer.config.lsp.servers {
                    config.lsp.servers.insert(lang.clone(), spec.clone());
                }
            }
            _ => {}
        }
    }
}

/// Stacks the chain's `permissions` sections into one config, highest-priority
/// layer first: the top-level default verb and each scope's bare `-all` verb
/// come from the highest-priority layer that sets them, rule lists keep every
/// layer's rules in chain order so the highest-priority layer's rules match
/// first, and the built-in rules sit at the deepest end.
fn stack_permissions(layers: &[PermissionsConfig]) -> PermissionsConfig {
    let mut merged = PermissionsConfig {
        default: None,
        paths: RuleSet::default(),
        shell: RuleSet::default(),
    };
    for layer in layers {
        merged.default = merged.default.or(layer.default);
        merged.paths.default = merged.paths.default.or(layer.paths.default);
        merged.shell.default = merged.shell.default.or(layer.shell.default);
        merged.paths.rules.extend(layer.paths.rules.iter().cloned());
        merged.shell.rules.extend(layer.shell.rules.iter().cloned());
    }
    let builtin = PermissionsConfig::builtin();
    merged.paths.rules.extend(builtin.paths.rules);
    merged.default = merged.default.or(builtin.default);
    merged.shell.default = merged.shell.default.or(builtin.shell.default);
    merged
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub ui: UiPrefs,

    pub embedding: EmbeddingConfig,

    pub agent: AgentConfig,

    pub lsp: LspConfigRepr,

    pub skills: SkillsConfig,

    pub context: ContextConfig,

    pub shell: ShellConfig,

    pub retry: RetryConfig,

    pub registries: RegistriesConfig,

    pub tools: ToolsConfig,

    pub permissions: PermissionsConfig,
}

/// A permission verdict for file paths and shell commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Allow,
    Ask,
    Deny,
}

impl Verb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// A permission rule scope: an optional bare `-all` fallback verb (falling
/// back to the top-level verb when unset) plus the rules in declaration
/// order — the first matching rule decides.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleSet<R> {
    /// The scope's bare verb (`allow-all` / `ask-all` / `deny-all`).
    pub default: Option<Verb>,
    pub rules: Vec<R>,
}

impl<R> Default for RuleSet<R> {
    fn default() -> Self {
        Self {
            default: None,
            rules: Vec::new(),
        }
    }
}

/// `permissions { … }` — how tool actions are gated: a top-level fallback
/// verb, path rules for the file tools, and shell-pattern rules for
/// `run_shell`. The default (section absent everywhere) is [`Self::builtin`].
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionsConfig {
    /// The top-level bare verb; `None` falls back to `Ask`.
    pub default: Option<Verb>,

    /// `paths { … }` — rules matched against the canonicalized file path.
    pub paths: RuleSet<PathRule>,

    /// `shell-patterns { … }` — rules matched against `run_shell` commands.
    pub shell: RuleSet<ShellRule>,
}

impl Default for PermissionsConfig {
    fn default() -> Self {
        Self::builtin()
    }
}

impl PermissionsConfig {
    /// The built-in permission baseline, which sits at the deepest end of the
    /// merged rule chain: ask all, allow inside the working directory except
    /// hidden files, and allow all shell commands. Its verbs apply when no
    /// config layer sets them.
    pub fn builtin() -> Self {
        Self {
            default: Some(Verb::Ask),
            paths: RuleSet {
                default: None,
                rules: vec![PathRule {
                    verb: Verb::Allow,
                    path: ".".to_string(),
                    except_hidden: true,
                    exact: false,
                    mode: Mode::Rw,
                }],
            },
            shell: RuleSet {
                default: Some(Verb::Allow),
                rules: vec![],
            },
        }
    }
}

/// One `paths { … }` rule: `allow|ask|deny [props] "<path>"`. A rule node may
/// carry several path arguments, one rule per argument sharing the verb and
/// properties. Relative paths anchor at the working directory and `~` expands
/// to the home directory.
#[derive(Debug, Clone, PartialEq)]
pub struct PathRule {
    pub verb: Verb,
    pub path: String,

    /// Skip the rule when the matched path contains a hidden component below
    /// the rule path — workspace-root `.agents` and the app dir stay exempt.
    pub except_hidden: bool,

    /// `#true` matches the exact path only; the default covers the path and
    /// everything below it.
    pub exact: bool,

    /// `rw` (default) governs reads and writes alike; `ro` matches only read
    /// requests.
    pub mode: Mode,
}

/// Read/write scope of an `allow` path rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// The rule matches reads and writes.
    #[default]
    Rw,
    /// The rule matches only reads; writes skip it.
    Ro,
}

/// One `shell-patterns { … }` rule: `allow|ask|deny [props] "<pattern>"`. A
/// rule node may carry several pattern arguments, one rule per argument
/// sharing the verb and properties.
#[derive(Debug, Clone, PartialEq)]
pub struct ShellRule {
    pub verb: Verb,
    pub pattern: String,

    /// How the pattern matches a command: `raw` (default) or `regex`.
    pub kind: ShellPatternKind,
}

/// How a shell pattern matches a command.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellPatternKind {
    /// Literal text, flanked by non-alphanumeric characters (or the string
    /// edges); multi-word patterns tolerate any whitespace runs.
    #[default]
    Raw,
    /// A regular expression, matched as written against the command line.
    Regex,
}

fn default_max_turns() -> usize {
    0
}

/// Context-window management: bounds the input tokens sent to the LLM. One
/// forecast (anchored on the last call's real request size) drives three
/// layers — the per-call mechanical trim, the stop-before-call overflow
/// guard, and the pre-send LLM compaction — all sharing this budget.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextConfig {
    /// Stored inverted in the file as `disabled`; defaults to enabled.
    pub disabled: bool,

    /// Tokens reserved for the model's reply and a safety buffer. The input
    /// budget is `context_length - reserved`.
    pub reserved: u64,

    /// Tokens kept verbatim as the "tail" when trimming older messages (both
    /// the in-run hook trim and compaction's cut point use this budget).
    pub keep_recent_tokens: u64,

    /// Maximum chars of a tool result's text sent to the model. Larger outputs
    /// are truncated with a marker hinting the model to read ranges. `0`
    /// disables the cap.
    pub tool_output_max_chars: usize,

    /// Maximum bytes of a tool result's text sent to the model. Applies on
    /// top of `tool_output_max_chars` (whichever caps first). `0` disables.
    pub tool_output_max_bytes: usize,

    /// Default context length used when the catalog has no entry for the
    /// active model.
    pub fallback_context_length: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            reserved: 20_000,
            keep_recent_tokens: 20_000,
            tool_output_max_chars: 16_000,
            tool_output_max_bytes: 50_000,
            fallback_context_length: 128_000,
        }
    }
}

impl ContextConfig {
    /// Usable input-token budget for the given model context length.
    pub fn usable(&self, context_length: u64) -> u64 {
        context_length.saturating_sub(self.reserved)
    }
}

/// The shell `run_shell` executes through. `path` accepts an absolute or
/// relative executable path, or a bare command name looked up in `PATH`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShellConfig {
    pub path: Option<String>,
}

/// Opt-in agent tools. Every entry is absent from the file by default, and
/// absence means the tool is not offered to the model at all.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToolsConfig {
    /// `tools { web-search { … } }` — the user-configured web search backend.
    /// `None` (section absent) or `enabled #false` registers no tool.
    pub web_search: Option<WebSearchConfig>,
}

/// A user-defined web search backend: one endpoint plus how to send the query
/// (`params`) and how to read the response (`kind`).
#[derive(Debug, Clone, PartialEq)]
pub struct WebSearchConfig {
    /// Stored as `enabled #true`; omitted or `#false` registers no tool.
    pub enabled: bool,

    /// The search endpoint. Must start with `http://` or `https://`.
    pub url: String,

    /// How to read the response: structured JSON results or fetch-and-convert
    /// to markdown.
    pub kind: WebSearchKind,

    /// Request headers sent with every call. Values may carry `$VAR` /
    /// `${VAR}` placeholders resolved from the environment at tool build time.
    pub headers: BTreeMap<String, String>,

    /// How the query reaches the endpoint.
    pub params: WebSearchParams,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchKind {
    /// JSON search API: the response body is `{"results":[{title,url,content}]}`.
    Ollama,
    /// Fetch the URL and convert the response body to markdown.
    ToMarkdown,
}

impl WebSearchKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::ToMarkdown => "to_markdown",
        }
    }
}

/// How the search query is sent to the endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct WebSearchParams {
    /// `body-json` = POST a JSON body, `query` = GET with URL query parameters.
    pub kind: WebSearchParamKind,

    /// Tool argument name → remote parameter name (`query as="q"`).
    pub map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSearchParamKind {
    BodyJson,
    Query,
}

impl WebSearchParamKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::BodyJson => "body-json",
            Self::Query => "query",
        }
    }

    /// The transport matching the endpoint's backend when `params` is omitted.
    pub fn default_for(kind: WebSearchKind) -> Self {
        match kind {
            WebSearchKind::Ollama => Self::BodyJson,
            WebSearchKind::ToMarkdown => Self::Query,
        }
    }
}

impl WebSearchParams {
    /// The params matching the endpoint's backend when `params` is omitted.
    pub fn default_for(kind: WebSearchKind) -> Self {
        let remote = match kind {
            WebSearchKind::Ollama => "query",
            WebSearchKind::ToMarkdown => "q",
        };
        Self {
            kind: WebSearchParamKind::default_for(kind),
            map: BTreeMap::from([("query".to_string(), remote.to_string())]),
        }
    }
}

/// Registry sources for provider/model catalogs. `selune` is the built-in
/// registry; further entries reserve names for user-defined registries and
/// are preserved verbatim on rewrite.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistriesConfig {
    pub entries: BTreeMap<String, RegistryEntry>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RegistryEntry {
    /// Stored inverted in the file as `disabled #true`; omitted = enabled.
    pub disabled: bool,

    /// Prefer the remote (hosted) catalog over the embedded offline one.
    pub remote_first: bool,
}

impl RegistryEntry {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl RegistriesConfig {
    /// The entry for a registry name; unknown names get defaults.
    pub fn entry(&self, name: &str) -> RegistryEntry {
        self.entries.get(name).copied().unwrap_or_default()
    }

    /// The built-in registry's entry.
    pub fn selune(&self) -> RegistryEntry {
        self.entry("selune")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillsConfig {
    pub disabled: bool,
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmbeddingConfig {
    pub disabled: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub dimensions: Option<u32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SidebarPref {
    /// Follow the terminal width: expanded at 80+ columns, collapsed below.
    #[default]
    Auto,
    Expanded,
    Collapsed,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UiPrefs {
    /// Target frames per second for the TUI render loop. `0` disables the cap
    /// (one draw per event, the original behavior). Defaults to 60.
    pub frame_rate: u32,

    /// Default sidebar expansion: `auto` (width-based), `expanded`, or
    /// `collapsed`. `auto` is omitted from the saved file.
    pub sidebar: SidebarPref,

    /// Copy the selection to the system clipboard when a chat drag finalizes.
    /// Off by default; copying stays available via Ctrl+Shift+C.
    pub copy_on_select: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            frame_rate: 60,
            sidebar: SidebarPref::Auto,
            copy_on_select: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentConfig {
    /// `0` = unlimited.
    pub max_turns: usize,

    /// `0` = unlimited.
    pub worker_max_turns: usize,
}

/// Auto-retry for provider connection failures (timeout, reset, HTTP
/// 408/429/5xx). The interval ladder is fixed: 3s, 5s, 10s, 20s, 30s, then
/// 60s for every further attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryConfig {
    /// Maximum auto-retry attempts per connection failure. `0` disables
    /// retrying (a connection failure errors out immediately).
    pub max_retries: usize,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self { max_retries: 10 }
    }
}

impl RetryConfig {
    pub fn effective_max_retries(&self) -> usize {
        self.max_retries
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: default_max_turns(),
            worker_max_turns: 0,
        }
    }
}

impl AgentConfig {
    pub fn effective_max_turns(&self) -> usize {
        effective(self.max_turns)
    }

    pub fn effective_worker_max_turns(&self) -> usize {
        effective(self.worker_max_turns)
    }
}

fn effective(value: usize) -> usize {
    if value == 0 { usize::MAX } else { value }
}

/// Config-file mirror of `shuvarie_lsp::LspConfig`; the `shuvarie-lsp`
/// crate itself stays config-format-free.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LspConfigRepr {
    pub disabled: bool,
    pub servers: BTreeRepr,
}

type BTreeRepr = std::collections::BTreeMap<String, LspServerSpecRepr>;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct LspServerSpecRepr {
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub no_auto_start: bool,
    pub root_markers: Vec<String>,
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| {
        ConfigError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory for this platform",
        ))
    })?;
    Ok(dir.join(CONFIG_DIR_NAME))
}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        Ok(config_dir()?.join(CONFIG_FILE_NAME))
    }

    /// `$cwd/shuvarie.kdl` and `$cwd/.shuvarie/config.kdl` (`-dev` suffixed
    /// in debug builds), in priority order above the global config.
    pub fn local_config_candidates(cwd: &std::path::Path) -> [PathBuf; 2] {
        [
            cwd.join(LOCAL_CONFIG_FILE_NAME),
            cwd.join(WORKSPACE_DIR_NAME).join(CONFIG_FILE_NAME),
        ]
    }

    /// Loads the config from the priority chain: `$cwd/shuvarie.kdl`, then
    /// `$cwd/.shuvarie/config.kdl`, then the global config. Every existing
    /// file is merged layer by layer: per top-level section the
    /// highest-priority file defining it wins wholesale (except
    /// `lsp.servers` and `registries`, which merge key-by-key, and
    /// `permissions`, whose verbs take the highest-priority layer that sets
    /// them while its rule lists stack highest-priority-first over the
    /// built-in rules). With no file present this returns `Default`. Debug
    /// builds read the `-dev` suffixed names (`shuvarie-dev.kdl`,
    /// `.shuvarie-dev`, `~/.config/shuvarie-dev`).
    pub fn load() -> Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut paths = Self::local_config_candidates(&cwd).to_vec();
        paths.push(Self::config_path()?);
        Self::load_chain(&paths)
    }

    fn load_chain(paths: &[PathBuf]) -> Result<Self> {
        let mut config = Self::default();
        let mut permissions = Vec::new();
        for path in paths {
            if let Some(layer) = read_layer(path)? {
                if layer.sections.contains("permissions") {
                    permissions.push(layer.config.permissions.clone());
                }
                merge_layer(&mut config, &layer);
            }
        }
        config.permissions = stack_permissions(&permissions);
        Ok(config)
    }

    /// Loads exactly the given file, ignoring the priority chain. Unlike the
    /// chain, a missing file is an error: the user named this file
    /// explicitly (`--config`).
    pub fn load_explicit(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => config_kdl::from_kdl(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Err(ConfigError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("config file not found: {}", path.display()),
                )))
            }
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => config_kdl::from_kdl(&contents),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(ConfigError::Io(e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, config_kdl::to_kdl(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let config = Config::default();
        let text = config_kdl::to_kdl(&config).unwrap();
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn empty_file_is_all_defaults() {
        let parsed = config_kdl::from_kdl("").unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn retry_config_defaults_to_ten() {
        let parsed = config_kdl::from_kdl("").unwrap();
        assert_eq!(parsed.retry.max_retries, 10);
    }

    #[test]
    fn retry_config_explicit_value() {
        let text = r#"
            retry {
                max-retries 3
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.retry.max_retries, 3);
    }

    #[test]
    fn retry_config_zero_round_trips() {
        let mut config = Config::default();
        config.retry.max_retries = 0;
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(text.contains("max-retries 0"), "0 must serialize: {text}");
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn partial_file_fills_defaults() {
        let text = r#"
            ui {
                frame-rate 30
            }
            skills {
                disabled #true
                dirs "a" "b"
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.ui.frame_rate, 30);
        assert!(parsed.skills.disabled);
        assert_eq!(parsed.skills.dirs, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parsed.embedding, EmbeddingConfig::default());
        assert_eq!(parsed.context, ContextConfig::default());
        assert_eq!(parsed.lsp, LspConfigRepr::default());
    }

    #[test]
    fn shell_path_round_trips() {
        let text = r#"
            shell {
                path "/usr/bin/zsh"
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.shell.path.as_deref(), Some("/usr/bin/zsh"));

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn shell_section_absent_is_none() {
        let parsed = config_kdl::from_kdl("ui { frame-rate 30 }").unwrap();
        assert_eq!(parsed.shell, ShellConfig::default());
        assert_eq!(parsed.shell.path, None);
    }

    #[test]
    fn disabled_bools_round_trip() {
        let text = r#"
            embedding {
                disabled #true
                provider "openai"
                dimensions 1536
            }
                        context {
                                disabled #true
                                reserved 5000
                                keep-recent-tokens 10000
                                tool-output-max-chars 1000
                                fallback-context-length 64000
                        }
                "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert!(parsed.embedding.disabled);
        assert_eq!(parsed.embedding.provider.as_deref(), Some("openai"));
        assert_eq!(parsed.embedding.dimensions, Some(1536));
        assert!(parsed.context.disabled);
        assert_eq!(parsed.context.reserved, 5000);
        assert_eq!(parsed.context.keep_recent_tokens, 10_000);
        assert_eq!(parsed.context.tool_output_max_chars, 1000);
        assert_eq!(parsed.context.fallback_context_length, 64_000);

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn lsp_servers_layout() {
        let text = r#"
            lsp {
                disabled #true
                servers {
                    rust {
                        command "rust-analyzer"
                        extensions ".rs"
                        no-auto-start #true
                        root-markers "Cargo.toml"
                    }
                    zig {
                        command "zls"
                    }
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert!(parsed.lsp.disabled);
        let rust = parsed.lsp.servers.get("rust").expect("rust server");
        assert_eq!(rust.command, vec!["rust-analyzer".to_string()]);
        assert_eq!(rust.extensions, vec![".rs".to_string()]);
        assert!(rust.no_auto_start);
        assert_eq!(rust.root_markers, vec!["Cargo.toml".to_string()]);
        let zig = parsed.lsp.servers.get("zig").expect("zig server");
        assert_eq!(zig.command, vec!["zls".to_string()]);
        assert!(!zig.no_auto_start);

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn bare_disabled_nodes_parse_as_defaults() {
        let text = r#"
            embedding {
                disabled
            }
            agent
            shell
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert!(!parsed.embedding.disabled);
        assert_eq!(parsed.embedding, EmbeddingConfig::default());
        assert_eq!(parsed.agent, AgentConfig::default());
        assert_eq!(parsed.shell, ShellConfig::default());
    }

    #[test]
    fn duplicate_field_is_an_error() {
        let text = r#"
            ui {
                frame-rate 30
                frame-rate 60
            }
        "#;
        let err = config_kdl::from_kdl(text).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert!(parse_err.message.contains("duplicate"), "{parse_err}");
    }

    #[test]
    fn out_of_range_integer_is_an_error() {
        let text = "ui {\n    frame-rate 99999999999\n}";
        let err = config_kdl::from_kdl(text).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
        assert!(parse_err.message.contains("out of range"), "{parse_err}");
    }

    #[test]
    fn invalid_sidebar_value_is_an_error() {
        let text = "ui {\n    sidebar sideways\n}";
        let err = config_kdl::from_kdl(text).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
        assert!(parse_err.message.contains("`sidebar`"), "{parse_err}");
    }

    #[test]
    fn unknown_fields_ignored() {
        let text = r#"
            ui {
                frame-rate 30
                bogus 1
            }
            unknown-section {
                whatever #true
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.ui.frame_rate, 30);
        assert_eq!(parsed.agent, AgentConfig::default());
    }

    #[test]
    fn type_error_surfaced_with_location() {
        let text = "ui {\n    frame-rate \"sixty\"\n}";
        let err = config_kdl::from_kdl(text).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
    }

    #[test]
    fn ui_sidebar_pref_round_trips() {
        for (text, expected) in [
            ("", SidebarPref::Auto),
            ("ui { sidebar expanded }", SidebarPref::Expanded),
            ("ui { sidebar collapsed }", SidebarPref::Collapsed),
        ] {
            let parsed = config_kdl::from_kdl(text).unwrap();
            assert_eq!(parsed.ui.sidebar, expected, "text: {text:?}");
        }

        let mut config = Config::default();
        config.ui.sidebar = SidebarPref::Expanded;
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(text.contains("sidebar expanded"), "body: {text}");
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);

        let config = Config::default();
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(!text.contains("sidebar"), "auto must be omitted: {text}");
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn ui_copy_on_select_round_trips() {
        let parsed = config_kdl::from_kdl("").unwrap();
        assert!(!parsed.ui.copy_on_select);
        let parsed = config_kdl::from_kdl("ui { copy-on-select #true }").unwrap();
        assert!(parsed.ui.copy_on_select);
        let parsed = config_kdl::from_kdl("ui { copy-on-select #false }").unwrap();
        assert!(!parsed.ui.copy_on_select);

        let mut config = Config::default();
        config.ui.copy_on_select = true;
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(text.contains("copy-on-select #true"), "body: {text}");
        assert!(
            !config_kdl::to_kdl(&Config::default())
                .unwrap()
                .contains("copy-on-select")
        );
    }

    #[test]
    fn save_and_reload_via_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.kdl");
        let mut config = Config::default();
        config.ui.frame_rate = 120;
        config.agent.max_turns = 8;
        config.skills.dirs = vec!["/tmp/skills".to_string()];
        config.lsp.disabled = true;
        config.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(config, loaded);
    }

    #[test]
    fn local_config_candidates_paths() {
        let candidates = Config::local_config_candidates(std::path::Path::new("/proj"));
        assert_eq!(
            candidates[0],
            PathBuf::from(format!("/proj/{LOCAL_CONFIG_FILE_NAME}"))
        );
        assert_eq!(
            candidates[1],
            PathBuf::from(format!("/proj/{WORKSPACE_DIR_NAME}/config.kdl"))
        );
    }

    #[test]
    fn profile_names_split_dev_and_release() {
        if cfg!(debug_assertions) {
            assert_eq!(CONFIG_DIR_NAME, "shuvarie-dev");
            assert_eq!(LOCAL_CONFIG_FILE_NAME, "shuvarie-dev.kdl");
            assert_eq!(WORKSPACE_DIR_NAME, ".shuvarie-dev");
        } else {
            assert_eq!(CONFIG_DIR_NAME, "shuvarie");
            assert_eq!(LOCAL_CONFIG_FILE_NAME, "shuvarie.kdl");
            assert_eq!(WORKSPACE_DIR_NAME, ".shuvarie");
        }
    }

    #[test]
    fn chain_merges_by_section() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let inner_dir = dir.path().join(".shuvarie");
        std::fs::create_dir_all(&inner_dir).unwrap();
        let inner = inner_dir.join("config.kdl");
        let top = dir.path().join("shuvarie.kdl");

        std::fs::write(
            &global,
            r#"
            agent {
                max-turns 4
            }
            skills {
                disabled #true
                dirs "g"
            }
            "#,
        )
        .unwrap();
        std::fs::write(
            &inner,
            r#"
            skills {
                dirs "inner"
            }
            context {
                reserved 9999
            }
            "#,
        )
        .unwrap();
        std::fs::write(
            &top,
            r#"
            ui {
                frame-rate 90
            }
            context {
                reserved 111
            }
            "#,
        )
        .unwrap();

        let config = Config::load_chain(&[global, inner, top]).unwrap();
        assert_eq!(config.ui.frame_rate, 90);
        assert_eq!(
            config.context.reserved, 111,
            "higher layer wins the section"
        );
        assert!(
            !config.skills.disabled,
            "whole skills section from the winning layer"
        );
        assert_eq!(config.skills.dirs, vec!["inner".to_string()]);
        assert_eq!(
            config.agent.max_turns, 4,
            "section only global defines survives"
        );
        assert_eq!(config.embedding, EmbeddingConfig::default());
    }

    #[test]
    fn chain_lsp_servers_merge_keywise() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");

        std::fs::write(
            &global,
            r#"
            lsp {
                servers {
                    rust {
                        command "rust-analyzer"
                        extensions ".rs"
                    }
                    zig {
                        command "zls"
                    }
                }
            }
            "#,
        )
        .unwrap();
        std::fs::write(
            &top,
            r#"
            lsp {
                disabled #true
                servers {
                    rust {
                        command "/custom/rust-analyzer"
                    }
                    go {
                        command "gopls"
                    }
                }
            }
            "#,
        )
        .unwrap();

        let config = Config::load_chain(&[global, top]).unwrap();
        assert!(
            config.lsp.disabled,
            "whole-section flag comes from the top file"
        );
        let rust = config.lsp.servers.get("rust").unwrap();
        assert_eq!(rust.command, vec!["/custom/rust-analyzer".to_string()]);
        assert!(
            rust.extensions.is_empty(),
            "spec replaced wholesale per language"
        );
        assert_eq!(
            config.lsp.servers.get("zig").unwrap().command,
            vec!["zls".to_string()],
            "unmentioned servers survive the merge"
        );
        assert_eq!(config.lsp.servers.len(), 3);
    }

    #[test]
    fn chain_missing_files_fall_through() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        std::fs::write(&global, "ui { frame-rate 24 }").unwrap();

        let config = Config::load_chain(&[dir.path().join("shuvarie.kdl"), global]).unwrap();
        assert_eq!(config.ui.frame_rate, 24);
    }

    #[test]
    fn chain_no_files_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            dir.path().join("shuvarie.kdl"),
            dir.path().join(".shuvarie/config.kdl"),
            dir.path().join("global.kdl"),
        ];
        assert_eq!(Config::load_chain(&paths).unwrap(), Config::default());
    }

    #[test]
    fn chain_parse_error_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(&top, "ui {\n    frame-rate \"sixty\"\n}").unwrap();

        let err = Config::load_chain(&[top]).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
    }

    #[test]
    fn load_explicit_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.kdl");

        let err = Config::load_explicit(&path).unwrap_err();
        let ConfigError::Io(io_err) = &err else {
            panic!("expected config io error");
        };
        assert_eq!(io_err.kind(), std::io::ErrorKind::NotFound);
        assert!(err.to_string().contains("nope.kdl"), "error names the file");
    }

    #[test]
    fn load_explicit_uses_only_the_given_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.kdl");
        std::fs::write(
            &path,
            r#"
            ui {
                frame-rate 17
            }
            "#,
        )
        .unwrap();

        let config = Config::load_explicit(&path).unwrap();
        assert_eq!(config.ui.frame_rate, 17);
        assert_eq!(config.embedding, EmbeddingConfig::default());
        assert_eq!(config.agent, AgentConfig::default());
    }

    #[test]
    fn load_explicit_parse_error_surfaced_with_location() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.kdl");
        std::fs::write(&path, "ui {\n    frame-rate \"sixty\"\n}").unwrap();

        let err = Config::load_explicit(&path).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
    }

    #[test]
    fn registries_section_parses() {
        let text = r#"
            registries {
                selune {
                    disabled #true
                    remote-first #true
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert!(parsed.registries.selune().disabled);
        assert!(parsed.registries.selune().remote_first);
        assert_eq!(parsed.registries.entries.len(), 1);
    }

    #[test]
    fn registries_section_absent_is_default() {
        let parsed = config_kdl::from_kdl("ui { frame-rate 30 }").unwrap();
        assert_eq!(parsed.registries, RegistriesConfig::default());
        assert!(!parsed.registries.entry("selune").disabled);
        assert!(!parsed.registries.entry("selune").remote_first);
    }

    #[test]
    fn registries_bare_nodes_parse_as_defaults() {
        let text = r#"
            registries {
                selune
                vendor-x {
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.registries.selune(), RegistryEntry::default());
        assert_eq!(
            parsed.registries.entry("vendor-x"),
            RegistryEntry::default()
        );
    }

    #[test]
    fn registries_unknown_names_round_trip() {
        let text = r#"
            registries {
                selune {
                    remote-first #true
                }
                vendor-x {
                    disabled #true
                }
                vendor-y
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.registries.entries.len(), 3);
        assert!(parsed.registries.entry("vendor-x").disabled);

        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("registries"), "body: {out}");
        assert!(out.contains("remote-first #true"), "body: {out}");
        assert!(out.contains("vendor-x"), "body: {out}");
        assert!(
            out.contains("vendor-y"),
            "default entry stays declared: {out}"
        );
        let reparsed = config_kdl::from_kdl(&out).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn registries_default_selune_omitted_on_save() {
        let text = config_kdl::to_kdl(&Config::default()).unwrap();
        assert!(!text.contains("registries"), "body: {text}");

        let mut config = Config::default();
        config.registries.entries.insert(
            "selune".to_string(),
            RegistryEntry {
                disabled: false,
                remote_first: true,
            },
        );
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(text.contains("selune {"), "body: {text}");
        assert!(text.contains("remote-first #true"), "body: {text}");
        assert!(!text.contains("disabled"), "default flag omitted: {text}");
    }

    #[test]
    fn registries_duplicate_entry_is_an_error() {
        for text in [
            r"registries {
                selune { disabled #true }
                selune
            }",
            r"registries {
                vendor-x
                vendor-x { disabled #true }
            }",
        ] {
            let err = config_kdl::from_kdl(text).unwrap_err();
            let ConfigError::Parse(parse_err) = err else {
                panic!("expected config parse error");
            };
            assert!(parse_err.message.contains("duplicate"), "{parse_err}");
        }
    }

    #[test]
    fn registries_chain_merges_keywise() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");

        std::fs::write(
            &global,
            r#"
            registries {
                selune {
                    remote-first #true
                }
                vendor-global {
                    disabled #true
                }
            }
            "#,
        )
        .unwrap();
        std::fs::write(
            &top,
            r#"
            registries {
                vendor-top
            }
            "#,
        )
        .unwrap();

        let config = Config::load_chain(&[global, top]).unwrap();
        assert!(
            config.registries.selune().remote_first,
            "selune only global defines survives"
        );
        assert!(
            config.registries.entries.contains_key("vendor-global"),
            "unknown registry only global defines survives"
        );
        assert!(
            config.registries.entries.contains_key("vendor-top"),
            "unknown registry from the winning layer lands"
        );
    }

    #[test]
    fn registries_entry_lookup() {
        let mut config = Config::default();
        config.registries.entries.insert(
            "selune".to_string(),
            RegistryEntry {
                disabled: true,
                remote_first: false,
            },
        );
        config.registries.entries.insert(
            "vendor-x".to_string(),
            RegistryEntry {
                disabled: false,
                remote_first: true,
            },
        );
        assert!(config.registries.selune().disabled);
        assert!(config.registries.entry("vendor-x").remote_first);
        assert_eq!(
            config.registries.entry("vendor-y"),
            RegistryEntry::default()
        );
    }

    #[test]
    fn web_search_absent_by_default() {
        let parsed = config_kdl::from_kdl("").unwrap();
        assert_eq!(parsed.tools.web_search, None);
        let text = config_kdl::to_kdl(&Config::default()).unwrap();
        assert!(!text.contains("tools"), "body: {text}");
    }

    #[test]
    fn web_search_ollama_example() {
        let text = r#"
            tools {
                web-search {
                    enabled #true

                    url "https://ollama.com/api/web_search"
                    type "ollama"

                    headers {
                        Authorization "Bearer $OLLAMA_API_KEY"
                    }

                    params type="body-json" {
                        query as="query"
                    }
                }

                /-web-search {
                    enabled #true

                    url "https://duckduckgo.com"
                    type "to_markdown"

                    params type="query" {
                        query as="q"
                    }
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let web = parsed
            .tools
            .web_search
            .expect("only one web-search survives");
        assert!(web.enabled);
        assert_eq!(web.url, "https://ollama.com/api/web_search");
        assert_eq!(web.kind, WebSearchKind::Ollama);
        assert_eq!(
            web.headers.get("Authorization").map(String::as_str),
            Some("Bearer $OLLAMA_API_KEY")
        );
        assert_eq!(web.params.kind, WebSearchParamKind::BodyJson);
        assert_eq!(
            web.params.map.get("query").map(String::as_str),
            Some("query")
        );
    }

    #[test]
    fn web_search_to_markdown_example() {
        let text = r#"
            tools {
                web-search {
                    enabled #true

                    url "https://html.duckduckgo.com/html/"
                    type "to_markdown"

                    params type="query" {
                        query as="q"
                    }
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let web = parsed.tools.web_search.as_ref().expect("web-search parsed");
        assert_eq!(web.kind, WebSearchKind::ToMarkdown);
        assert_eq!(web.params.kind, WebSearchParamKind::Query);
        assert_eq!(web.params.map.get("query").map(String::as_str), Some("q"));

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn web_search_params_defaults_per_kind() {
        let parsed = config_kdl::from_kdl(
            r#"
            tools {
                web-search {
                    url "https://ollama.com/api/web_search"
                    type "ollama"
                }
            }
        "#,
        )
        .unwrap();
        let web = parsed.tools.web_search.clone().unwrap();
        assert_eq!(
            web.params,
            WebSearchParams::default_for(WebSearchKind::Ollama)
        );
        assert!(!web.enabled, "omitted enabled means off");
        assert!(web.headers.is_empty());

        let parsed = config_kdl::from_kdl(
            r#"
            tools {
                web-search {
                    url "https://html.duckduckgo.com/html/"
                    type "to_markdown"
                }
            }
        "#,
        )
        .unwrap();
        let web = parsed.tools.web_search.clone().unwrap();
        assert_eq!(
            web.params,
            WebSearchParams::default_for(WebSearchKind::ToMarkdown)
        );

        let text = config_kdl::to_kdl(&parsed).unwrap();
        assert!(!text.contains("params"), "defaults omitted: {text}");
        assert!(!text.contains("enabled"), "off flag omitted: {text}");
    }

    #[test]
    fn web_search_duplicate_section_is_an_error() {
        let err = config_kdl::from_kdl(
            r#"
            tools {
                web-search {
                    url "https://a.example"
                    type "ollama"
                }
                web-search {
                    url "https://b.example"
                    type "to_markdown"
                }
            }
        "#,
        )
        .unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert!(parse_err.message.contains("duplicate"), "{parse_err}");
    }

    #[test]
    fn web_search_parse_errors() {
        let cases: &[(&str, &str)] = &[
            (
                "tools { web-search { type \"ollama\" } }",
                "requires a `url`",
            ),
            (
                "tools { web-search { url \"https://a.example\" } }",
                "requires a `type`",
            ),
            (
                "tools { web-search { url \"ftp://a\"; type \"ollama\" } }",
                "must start with http",
            ),
            (
                "tools { web-search { url \"https://a\"; type \"bing\" } }",
                "`ollama` or `to_markdown`",
            ),
            (
                "tools { web-search { url \"https://a\"; type \"ollama\"; params type=\"form\" { query as=\"q\" } } }",
                "`body-json` or `query`",
            ),
            (
                "tools { web-search { url \"https://a\"; type \"ollama\"; params { answer as=\"a\" } } }",
                "only `query`",
            ),
            (
                "tools { web-search { url \"https://a\"; type \"ollama\"; params { query } } }",
                "the remote parameter name",
            ),
            (
                "tools { web-search { url \"https://a\"; type \"ollama\"; headers { \"Bad Header\" \"v\" } } }",
                "header",
            ),
        ];
        for (text, needle) in cases {
            let err = config_kdl::from_kdl(text).unwrap_err();
            let ConfigError::Parse(parse_err) = err else {
                panic!("expected config parse error for {text}");
            };
            assert!(
                parse_err.message.contains(needle),
                "{needle:?} not in {parse_err}"
            );
        }
    }

    #[test]
    fn web_search_full_config_round_trips() {
        let mut config = Config::default();
        config.tools.web_search = Some(WebSearchConfig {
            enabled: true,
            url: "https://ollama.com/api/web_search".to_string(),
            kind: WebSearchKind::Ollama,
            headers: BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer ${OLLAMA_API_KEY}".to_string(),
            )]),
            params: WebSearchParams {
                kind: WebSearchParamKind::BodyJson,
                map: BTreeMap::from([("query".to_string(), "query".to_string())]),
            },
        });
        let text = config_kdl::to_kdl(&config).unwrap();
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);

        config.tools.web_search.as_mut().unwrap().enabled = false;
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(!text.contains("enabled"), "off flag omitted: {text}");
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn web_search_layer_replaces_wholesale() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(
            &global,
            r#"
            tools {
                web-search {
                    enabled #true
                    url "https://global.example"
                    type "ollama"
                }
            }
        "#,
        )
        .unwrap();
        std::fs::write(
            &top,
            r#"
            tools {
                web-search {
                    url "https://top.example"
                    type "to_markdown"
                }
            }
        "#,
        )
        .unwrap();
        let config = Config::load_chain(&[global, top]).unwrap();
        let web = config.tools.web_search.expect("top layer wins");
        assert_eq!(web.url, "https://top.example");
        assert_eq!(web.kind, WebSearchKind::ToMarkdown);
        assert!(!web.enabled);
    }

    #[test]
    fn permissions_section_absent_is_builtin() {
        let parsed = config_kdl::from_kdl("").unwrap();
        assert_eq!(parsed.permissions, PermissionsConfig::builtin());
        assert_eq!(parsed.permissions.default, Some(Verb::Ask));
        assert_eq!(parsed.permissions.paths.rules.len(), 1);
        assert_eq!(parsed.permissions.paths.rules[0].verb, Verb::Allow);
        assert_eq!(parsed.permissions.paths.rules[0].path, ".");
        assert!(parsed.permissions.paths.rules[0].except_hidden);
        assert_eq!(parsed.permissions.shell.rules.len(), 0);
        assert_eq!(parsed.permissions.shell.default, Some(Verb::Allow));
    }

    #[test]
    fn permissions_builtin_round_trips_as_absent() {
        let config = Config::default();
        let text = config_kdl::to_kdl(&config).unwrap();
        assert!(!text.contains("permissions"), "builtin omitted: {text}");
        let parsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn permissions_proposed_example_parses_and_round_trips() {
        let text = r#"
            permissions {
                /-deny-all
                /-ask-all

                paths {
                    ask-all
                    /-deny-all
                    /-allow-all
                    allow except-hidden=#true "."
                    ask ".env"
                    deny exact=#false "~/.ssh"
                }

                shell-patterns {
                    /-allow-all
                    /-deny-all
                    /-ask-all
                    ask "rm"
                    ask pattern="regex" "rm (-rf|-fr|--force --recursive)"
                    deny "sudo"
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let perms = &parsed.permissions;
        assert_eq!(perms.default, None, "no top-level bare verb declared");
        assert_eq!(perms.paths.default, Some(Verb::Ask));
        assert_eq!(perms.paths.rules.len(), 3);
        assert_eq!(perms.paths.rules[0].verb, Verb::Allow);
        assert!(perms.paths.rules[0].except_hidden);
        assert_eq!(perms.paths.rules[0].path, ".");
        assert_eq!(perms.paths.rules[1].verb, Verb::Ask);
        assert_eq!(perms.paths.rules[1].path, ".env");
        assert_eq!(perms.paths.rules[2].verb, Verb::Deny);
        assert_eq!(perms.paths.rules[2].path, "~/.ssh");
        assert!(!perms.paths.rules[2].exact);
        assert_eq!(perms.shell.default, None);
        assert_eq!(perms.shell.rules.len(), 3);
        assert_eq!(perms.shell.rules[0].pattern, "rm");
        assert_eq!(perms.shell.rules[0].kind, ShellPatternKind::Raw);
        assert_eq!(
            perms.shell.rules[1].pattern,
            "rm (-rf|-fr|--force --recursive)"
        );
        assert_eq!(perms.shell.rules[1].kind, ShellPatternKind::Regex);
        assert_eq!(perms.shell.rules[2].pattern, "sudo");

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn permissions_top_level_verb_and_scope_fallbacks() {
        let text = r#"
            permissions {
                deny-all
                paths {
                    allow-all
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let perms = &parsed.permissions;
        assert_eq!(perms.default, Some(Verb::Deny));
        assert_eq!(perms.paths.default, Some(Verb::Allow));
        assert!(perms.paths.rules.is_empty());
        assert!(perms.shell.rules.is_empty());
        assert_eq!(perms.shell.default, None, "shell inherits the top verb");

        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("deny-all"), "{out}");
        assert!(out.contains("allow-all"), "{out}");
        assert!(out.contains("paths"), "{out}");
        assert!(
            !out.contains("shell-patterns"),
            "empty scope omitted: {out}"
        );
    }

    #[test]
    fn permissions_parse_errors() {
        let cases: &[(&str, &str)] = &[
            ("permissions { allow-all; deny-all }", "duplicate"),
            ("permissions { paths { allow-all; ask-all } }", "duplicate"),
            ("permissions { nonsense }", "unknown node"),
            ("permissions { paths { nonsense } }", "unknown node"),
            (
                "permissions { paths { deny exact=5 \"x\" } }",
                "must be a boolean",
            ),
            (
                "permissions { paths { deny wat=#true \"x\" } }",
                "unknown property",
            ),
            (
                "permissions { shell-patterns { deny pattern=\"bogus\" \"x\" } }",
                "`raw` or `regex`",
            ),
            (
                "permissions { paths { deny } }",
                "requires at least one path",
            ),
            (
                "permissions { paths { allow 1 2 } }",
                "list of string arguments",
            ),
            (
                "permissions { paths { allow \"a\" { nested } } }",
                "takes no children",
            ),
            (
                "permissions { shell-patterns { ask } }",
                "requires at least one pattern",
            ),
            ("permissions { allow-all \"junk\" }", "takes no arguments"),
            (
                "permissions { paths { allow-all { nested } } }",
                "takes no children",
            ),
            (
                "permissions { paths { deny mode=\"ro\" \"x\" } }",
                "only valid on `allow`",
            ),
            (
                "permissions { paths { ask mode=\"rw\" \"x\" } }",
                "only valid on `allow`",
            ),
            (
                "permissions { paths { allow mode=\"bogus\" \"x\" } }",
                "`ro` or `rw`",
            ),
            (
                "permissions { shell-patterns { ask mode=\"ro\" \"x\" } }",
                "unknown property",
            ),
            (
                "permissions { shell-patterns { deny interrupt=#true \"x\" } }",
                "unknown property",
            ),
        ];
        for (text, needle) in cases {
            let err = config_kdl::from_kdl(text).unwrap_err();
            let ConfigError::Parse(parse_err) = err else {
                panic!("expected config parse error for {text}");
            };
            assert!(
                parse_err.message.contains(needle),
                "{needle:?} not in {parse_err}"
            );
        }
    }

    #[test]
    fn permissions_multi_argument_rules_expand_in_order() {
        let text = r#"
            permissions {
                paths {
                    allow except-hidden=#true "." "../some_dir" "/some/other/dir"
                    ask ".env" "~/.ssh"
                }

                shell-patterns {
                    deny pattern="regex" "rm (-rf|-fr)" "git push --force"
                    ask "rm" "sudo"
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let perms = &parsed.permissions;
        assert_eq!(perms.paths.rules.len(), 5);
        assert_eq!(perms.paths.rules[0].path, ".");
        assert_eq!(perms.paths.rules[1].path, "../some_dir");
        assert_eq!(perms.paths.rules[2].path, "/some/other/dir");
        for rule in &perms.paths.rules[..3] {
            assert_eq!(rule.verb, Verb::Allow);
            assert!(rule.except_hidden);
            assert!(!rule.exact);
        }
        for rule in &perms.paths.rules[3..] {
            assert_eq!(rule.verb, Verb::Ask);
            assert!(!rule.except_hidden);
        }
        assert_eq!(perms.shell.rules.len(), 4);
        for rule in &perms.shell.rules[..2] {
            assert_eq!(rule.verb, Verb::Deny);
            assert_eq!(rule.kind, ShellPatternKind::Regex);
        }
        assert_eq!(perms.shell.rules[0].pattern, "rm (-rf|-fr)");
        assert_eq!(perms.shell.rules[1].pattern, "git push --force");
        for rule in &perms.shell.rules[2..] {
            assert_eq!(rule.verb, Verb::Ask);
            assert_eq!(rule.kind, ShellPatternKind::Raw);
        }
        assert_eq!(perms.shell.rules[2].pattern, "rm");
        assert_eq!(perms.shell.rules[3].pattern, "sudo");

        let text = config_kdl::to_kdl(&parsed).unwrap();
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn permissions_mode_parses_defaults_and_round_trips() {
        let text = r#"
            permissions {
                paths {
                    allow mode="ro" "~/.ssh"
                    allow mode="rw" "a"
                    allow "b"
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let rules = &parsed.permissions.paths.rules;
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].mode, Mode::Ro);
        assert_eq!(rules[1].mode, Mode::Rw, "explicit rw");
        assert_eq!(rules[2].mode, Mode::Rw, "omitted mode defaults to rw");

        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("allow mode=ro \"~/.ssh\""), "ro kept: {out}");
        assert_eq!(out.matches("mode=").count(), 1, "rw omitted: {out}");
        let reparsed = config_kdl::from_kdl(&out).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn permissions_mode_splits_serializer_groups() {
        let text = r#"
            permissions {
                paths {
                    allow mode="ro" "a" "b"
                    allow "c"
                    allow mode="ro" "d"
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("allow mode=ro a b"), "ro run grouped: {out}");
        assert!(out.contains("allow c"), "rw stays its own node: {out}");
        assert!(out.contains("allow mode=ro d"), "ro run split: {out}");
        let reparsed = config_kdl::from_kdl(&out).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn permissions_serializer_groups_consecutive_rules() {
        let text = r#"
            permissions {
                paths {
                    allow "a" "b"
                    ask "c"
                    ask ".env" "d"
                    allow "f"
                    allow "g"
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("allow a b"), "grouped run: {out}");
        assert!(out.contains("ask c .env d"), "mixed quoting: {out}");
        assert!(
            out.contains("allow f g"),
            "second run stays separate: {out}"
        );
        let reparsed = config_kdl::from_kdl(&out).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn permissions_layers_stack_rules_most_specific_first() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(
            &global,
            "permissions { paths { allow except-hidden=#true \".\" } }",
        )
        .unwrap();
        std::fs::write(&top, "permissions { paths { deny \"secrets/\" } }").unwrap();
        let config = Config::load_chain(&[top, global]).unwrap();
        let perms = &config.permissions;
        assert_eq!(perms.default, Some(Verb::Ask), "builtin verb survives");
        let paths: Vec<(Verb, &str)> = perms
            .paths
            .rules
            .iter()
            .map(|rule| (rule.verb, rule.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![
                (Verb::Deny, "secrets/"),
                (Verb::Allow, "."),
                (Verb::Allow, ".")
            ],
            "local rules first, global second, builtin deepest"
        );
        assert_eq!(perms.paths.default, None);
        assert_eq!(perms.shell.default, Some(Verb::Allow));
    }

    #[test]
    fn permissions_local_section_follows_global_rules() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(
            &global,
            "permissions { paths { allow except-hidden=#true \".\" } }",
        )
        .unwrap();
        std::fs::write(&top, "permissions { ask-all }").unwrap();
        let config = Config::load_chain(&[top, global]).unwrap();
        let perms = &config.permissions;
        assert_eq!(perms.default, Some(Verb::Ask));
        let paths: Vec<(Verb, &str)> = perms
            .paths
            .rules
            .iter()
            .map(|rule| (rule.verb, rule.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![(Verb::Allow, "."), (Verb::Allow, ".")],
            "the global allow rules survive a local section without rules"
        );
        assert_eq!(perms.shell.default, Some(Verb::Allow));
    }

    #[test]
    fn permissions_local_rules_keep_the_builtin_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(&top, "permissions { paths { deny \"secrets/\" } }").unwrap();
        let config = Config::load_chain(&[top]).unwrap();
        let paths: Vec<(Verb, &str)> = config
            .permissions
            .paths
            .rules
            .iter()
            .map(|rule| (rule.verb, rule.path.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![(Verb::Deny, "secrets/"), (Verb::Allow, ".")],
            "the builtin cwd allow stays beneath the local rules"
        );
        assert_eq!(config.permissions.shell.default, Some(Verb::Allow));
    }

    #[test]
    fn permissions_verbs_take_the_most_specific_layer() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(&global, "permissions { paths { allow-all } }").unwrap();
        std::fs::write(&top, "permissions { paths { ask \"x\" } }").unwrap();
        let config = Config::load_chain(&[top.clone(), global.clone()]).unwrap();
        assert_eq!(
            config.permissions.paths.default,
            Some(Verb::Allow),
            "the top file set no bare verb, the global one survives"
        );
        assert_eq!(config.permissions.paths.rules.len(), 2);

        std::fs::write(&top, "permissions { paths { ask-all } }").unwrap();
        let config = Config::load_chain(&[top, global]).unwrap();
        assert_eq!(
            config.permissions.paths.default,
            Some(Verb::Ask),
            "the top file's bare verb wins"
        );
    }
}
