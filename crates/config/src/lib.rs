use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

mod config_kdl;
mod connections;
mod connections_kdl;
mod error;
mod kdl_util;
mod trusts;
mod trusts_kdl;

pub use self::connections::{Active, Connections, ProviderConfig};
pub use self::trusts::{Category, ScanItem, TrustFile, TrustGrants, WorkspaceScan, WorkspaceTrust};
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

/// The `scene.d` drop-in dir name, next to each config directory: a global
/// `<config_dir>/scene.d` and a workspace `<WORKSPACE_DIR_NAME>/scene.d`.
pub const SCENE_DIR_NAME: &str = "scene.d";

/// The `themes.d` drop-in dir name, next to each config directory: a global
/// `<config_dir>/themes.d` and a workspace `<WORKSPACE_DIR_NAME>/themes.d`.
pub const THEMES_DIR_NAME: &str = "themes.d";

/// Context-file candidates for one directory, in priority order: an
/// `AGENTS.override.md` replaces the plain files in its directory, and
/// `CLAUDE.md` is the fallback for projects written for other agents.
pub const CONTEXT_FILE_CANDIDATES: [&str; 5] = [
    "AGENTS.override.md",
    "AGENTS.md",
    "AGENTS.MD",
    "CLAUDE.md",
    "CLAUDE.MD",
];

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
/// registry name for the same reason. `permissions`, `scenes`, and `themes`
/// are stacked separately by [`stack_permissions`] / [`stack_scenes`] /
/// [`stack_themes`] once the whole chain has been read.
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

/// Stacks the chain's `scenes` sections into one config, lowest-priority
/// layer first: `default` comes from the highest layer that sets it, and each
/// scene merges field-wise per scene name so a layer can extend a scene
/// defined elsewhere without hiding it. This is the save-faithful merge for
/// [`Config::scenes`]; the runtime scene set ([`Config::load_scenes`])
/// instead resolves the chain's layers through `scene_sources`, where
/// same-level duplicates conflict.
fn stack_scenes(layers: &[ScenesConfig]) -> ScenesConfig {
    let mut merged = ScenesConfig::default();
    for layer in layers.iter().rev() {
        merged.stack(layer.clone());
    }
    merged
}

/// Stacks the chain's `themes` sections into one config, lowest-priority
/// layer first: each theme definition merges its color overrides key-wise
/// per name and variant so a layer can extend a theme defined elsewhere
/// without hiding it.
/// This is the save-faithful merge for [`Config::themes`]; the runtime theme
/// set ([`Config::load_themes`]) instead resolves the chain's layers through
/// `theme_sources`, where same-level duplicates conflict.
fn stack_themes(layers: &[ThemesConfig]) -> ThemesConfig {
    let mut merged = ThemesConfig::default();
    for layer in layers.iter().rev() {
        merged.stack(layer.clone());
    }
    merged
}

/// Merges one level's sources (highest priority first): `default` comes from
/// the highest-priority source that sets it, and a scene name defined by more
/// than one source of the level is a conflict — the scene is dropped
/// entirely and a warning names every defining source.
fn merge_level(level: &str, sources: &[SceneSource]) -> (ScenesConfig, Vec<String>) {
    let mut merged = ScenesConfig::default();
    let mut defined: std::collections::BTreeMap<String, String> = Default::default();
    let mut conflicts: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for source in sources {
        if merged.default.is_none() {
            merged.default = source.scenes.default.clone();
        }
        for (name, scene) in &source.scenes.scenes {
            match defined.get(name) {
                None => {
                    defined.insert(name.clone(), source.label.clone());
                    merged.scenes.insert(name.clone(), scene.clone());
                }
                Some(first) => {
                    merged.scenes.remove(name);
                    let labels = conflicts.entry(name.clone()).or_default();
                    if labels.is_empty() {
                        labels.push(first.clone());
                    }
                    labels.push(source.label.clone());
                }
            }
        }
    }
    let warnings = conflicts
        .into_iter()
        .map(|(name, labels)| {
            format!(
                "scene `{name}` is defined multiple times in the {level} ({labels}); loading none of them",
                labels = labels.join(", ")
            )
        })
        .collect();
    (merged, warnings)
}

/// The display label of a config file in same-level conflict warnings: the
/// file name, prefixed with the workspace dir when it lives inside one.
fn source_label(path: &std::path::Path) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    match path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|parent| parent.to_str())
    {
        Some(parent) if parent == WORKSPACE_DIR_NAME => format!("{parent}/{name}"),
        _ => name.to_string(),
    }
}

/// Merges the two levels into the runtime scene set: the global config layer
/// plus the global drop-ins form the global level, the local config layers
/// plus the workspace drop-ins form the local level, and the local level
/// then overrides the global one field-wise per scene name.
fn scene_set_from_levels(
    global_layer: Option<SceneSource>,
    local_layers: Vec<SceneSource>,
    global_dir: &std::path::Path,
    dropin_dirs: &[PathBuf],
) -> SceneSet {
    let mut global_sources = Vec::new();
    if let Some(layer) = global_layer {
        global_sources.push(layer);
    }
    let (dir_sources, mut warnings) = load_scene_dir_in(global_dir, false);
    global_sources.extend(dir_sources);
    let mut local_sources = local_layers;
    for dir in dropin_dirs {
        let (dir_sources, dir_warnings) = load_scene_dir_in(dir, true);
        local_sources.extend(dir_sources);
        warnings.extend(dir_warnings);
    }
    let (global, global_warnings) = merge_level("global config", &global_sources);
    warnings.extend(global_warnings);
    let (local, local_warnings) = merge_level("local config", &local_sources);
    warnings.extend(local_warnings);
    let mut scenes = global;
    scenes.stack(local);
    SceneSet { scenes, warnings }
}

/// Merges one level's theme sources (highest priority first): a theme
/// definition (a name, plus its optional variant) defined by more than one
/// source of the level is a conflict — the definition is dropped entirely
/// and a warning names every defining source.
fn merge_theme_level(level: &str, sources: &[ThemeSource]) -> (ThemesConfig, Vec<String>) {
    let mut merged = ThemesConfig::default();
    let mut defined: std::collections::BTreeMap<(String, Option<String>), String> =
        Default::default();
    let mut conflicts: std::collections::BTreeMap<(String, Option<String>), Vec<String>> =
        Default::default();
    for source in sources {
        for (key, theme) in &source.themes.themes {
            match defined.get(key) {
                None => {
                    defined.insert(key.clone(), source.label.clone());
                    merged.themes.insert(key.clone(), theme.clone());
                }
                Some(first) => {
                    merged.themes.remove(key);
                    let labels = conflicts.entry(key.clone()).or_default();
                    if labels.is_empty() {
                        labels.push(first.clone());
                    }
                    labels.push(source.label.clone());
                }
            }
        }
    }
    let warnings = conflicts
        .into_iter()
        .map(|(key, labels)| {
            format!(
                "theme `{}` is defined multiple times in the {level} ({labels}); loading none of them",
                theme_key_label(&key),
                labels = labels.join(", ")
            )
        })
        .collect();
    (merged, warnings)
}

/// The display form of a theme definition key: `name`, or `name:variant`
/// when the definition targets one variant.
pub(crate) fn theme_key_label(key: &(String, Option<String>)) -> String {
    match &key.1 {
        Some(variant) => format!("{}:{variant}", key.0),
        None => key.0.clone(),
    }
}

/// Merges the two levels into the runtime theme set: the global config layer
/// plus the global drop-ins form the global level, the local config layers
/// plus the workspace drop-ins form the local level, and the local level
/// then overrides the global one key-wise per name and variant.
fn theme_set_from_levels(
    global_layer: Option<ThemeSource>,
    local_layers: Vec<ThemeSource>,
    global_dir: &std::path::Path,
    dropin_dirs: &[PathBuf],
) -> ThemeSet {
    let mut global_sources = Vec::new();
    if let Some(layer) = global_layer {
        global_sources.push(layer);
    }
    let (dir_sources, mut warnings) = load_theme_dir_in(global_dir, false);
    global_sources.extend(dir_sources);
    let mut local_sources = local_layers;
    for dir in dropin_dirs {
        let (dir_sources, dir_warnings) = load_theme_dir_in(dir, true);
        local_sources.extend(dir_sources);
        warnings.extend(dir_warnings);
    }
    let (global, global_warnings) = merge_theme_level("global config", &global_sources);
    warnings.extend(global_warnings);
    let (local, local_warnings) = merge_theme_level("local config", &local_sources);
    warnings.extend(local_warnings);
    let mut themes = global;
    themes.stack(local);
    ThemeSet { themes, warnings }
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

    /// `scenes { … }` — the named scenes defined by the config chain,
    /// already stacked. The runtime scene set additionally layers the
    /// `scene.d` drop-in dirs on top and resolves same-level conflicts
    /// (see [`Self::load_scenes`]).
    pub scenes: ScenesConfig,

    /// The config chain's `scenes` per layer, highest priority first (the
    /// local layers, then the global one; an explicit `--config` file is the
    /// single layer). [`Self::load_scenes`] needs the layers to tell the
    /// global level from the local one and to detect same-level conflicts;
    /// `scenes` above stays the plain chain merge for save fidelity.
    pub scene_sources: Vec<SceneSource>,

    /// `themes { … }` — the named themes defined by the config chain,
    /// already stacked. The runtime theme set additionally layers the
    /// `themes.d` drop-in dirs on top and resolves same-level conflicts
    /// (see [`Self::load_themes`]).
    pub themes: ThemesConfig,

    /// The config chain's `themes` per layer, highest priority first (the
    /// local layers, then the global one; an explicit `--config` file is the
    /// single layer). [`Self::load_themes`] needs the layers to tell the
    /// global level from the local one and to detect same-level conflicts;
    /// `themes` above stays the plain chain merge for save fidelity.
    pub theme_sources: Vec<ThemeSource>,
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

/// `scenes { … }` — the named scenes: per-scene system prompts, injected
/// wrapper prompts, and the tool roster. The built-in default scene is code,
/// not config; this section only defines named scenes on top of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScenesConfig {
    /// The scene for new sessions (`default "Plan"`). `None` keeps the
    /// built-in default scene.
    pub default: Option<String>,

    /// The defined scenes, keyed by name.
    pub scenes: BTreeMap<String, SceneConfig>,
}

impl ScenesConfig {
    /// Parses a standalone `scenes` document (the same format as the config
    /// section, as used by `scene.d` drop-ins). Top-level `scenes` nodes
    /// stack field-wise; same-level conflicts are only decided across
    /// sources, by [`Config::load_scenes`].
    pub fn from_kdl(contents: &str) -> crate::Result<Self> {
        let mut scenes = Self::default();
        for source in crate::config_kdl::scenes_from_document(contents)? {
            scenes.stack(source);
        }
        Ok(scenes)
    }

    /// Stacks `higher` over `self`: `default` comes from the highest layer
    /// that sets it, and each scene merges field-wise per scene name so a
    /// layer can extend a scene defined elsewhere without hiding it.
    pub fn stack(&mut self, higher: ScenesConfig) {
        if higher.default.is_some() {
            self.default = higher.default;
        }
        for (name, scene) in higher.scenes {
            match self.scenes.get_mut(&name) {
                Some(lower) => lower.merge(scene),
                None => {
                    self.scenes.insert(name, scene);
                }
            }
        }
    }

    /// The scene with the given name.
    pub fn scene(&self, name: &str) -> Option<&SceneConfig> {
        self.scenes.get(name)
    }
}

/// One config source's `scenes` section, with the display label used in
/// same-level conflict warnings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneSource {
    /// Display label: `shuvarie.kdl`, `.shuvarie/config.kdl`, `config.kdl`,
    /// or `scene.d/<file>`.
    pub label: String,

    pub scenes: ScenesConfig,

    /// Whether the source belongs to the workspace (local) level; `false` is
    /// the global config's layer. Levels decide override order and same-level
    /// conflicts, so the flag is recorded at load time — deriving it from the
    /// chain position would misclassify a local layer as the global one when
    /// the global config file does not exist.
    pub local: bool,
}

/// The runtime scene set [`Config::load_scenes`] builds: the merged scene
/// config plus one warning per same-level conflict (a scene name defined by
/// more than one source of the same level loads neither copy).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneSet {
    pub scenes: ScenesConfig,

    pub warnings: Vec<String>,
}

/// One named scene.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneConfig {
    /// Free-text shown in the scene picker.
    pub description: Option<String>,

    /// Per-worker overrides.
    pub subagents: SubagentsConfig,

    /// System-prompt pieces injected around the conversation.
    pub system_prompts: SystemPromptsConfig,

    /// Reserved for the per-provider thinking toggle (not wired yet).
    pub thinking: Option<bool>,

    /// Tool availability inside the scene.
    pub tools: SceneToolsConfig,
}

impl SceneConfig {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn merge(&mut self, higher: SceneConfig) {
        if higher.description.is_some() {
            self.description = higher.description;
        }
        if higher.thinking.is_some() {
            self.thinking = higher.thinking;
        }
        self.subagents.merge(higher.subagents);
        self.system_prompts.merge(higher.system_prompts);
        self.tools.merge(higher.tools);
    }
}

/// `subagents { … }` inside a scene: a whole-roster kill switch plus
/// per-worker overrides keyed by worker name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubagentsConfig {
    pub disabled: bool,
    pub workers: BTreeMap<String, SubagentConfig>,
}

impl SubagentsConfig {
    fn is_default(&self) -> bool {
        !self.disabled && self.workers.is_empty()
    }

    fn merge(&mut self, higher: SubagentsConfig) {
        self.disabled |= higher.disabled;
        for (name, worker) in higher.workers {
            self.workers.entry(name).or_default().merge(worker);
        }
    }
}

/// One `subagents { <name> { … } }` entry: per-worker prompt, thinking, and
/// tool overrides, or the worker dropped from the roster entirely.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SubagentConfig {
    pub disabled: bool,

    /// Reserved for the per-provider thinking toggle (not wired yet).
    pub thinking: Option<bool>,

    pub system_prompts: SystemPromptsConfig,

    pub tools: SceneToolsConfig,
}

impl SubagentConfig {
    fn merge(&mut self, higher: SubagentConfig) {
        self.disabled |= higher.disabled;
        if higher.thinking.is_some() {
            self.thinking = higher.thinking;
        }
        self.system_prompts.merge(higher.system_prompts);
        self.tools.merge(higher.tools);
    }
}

/// `system-prompts { … }`: the scene's system-prompt pieces. `prelude`
/// replaces the built-in agent preamble, `interlude` is injected at a
/// mid-session scene switch, and the `before-each`/`after-each` hooks wrap
/// every user prompt / assistant reply / turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SystemPromptsConfig {
    pub prelude: Option<String>,
    pub interlude: Option<String>,
    pub before_each: Option<Hooks>,
    pub after_each: Option<Hooks>,
}

impl SystemPromptsConfig {
    fn is_default(&self) -> bool {
        self.prelude.is_none()
            && self.interlude.is_none()
            && self.before_each.is_none()
            && self.after_each.is_none()
    }

    fn merge(&mut self, higher: SystemPromptsConfig) {
        if higher.prelude.is_some() {
            self.prelude = higher.prelude;
        }
        if higher.interlude.is_some() {
            self.interlude = higher.interlude;
        }
        match (self.before_each.as_mut(), higher.before_each) {
            (Some(lower), Some(higher)) => lower.merge(higher),
            (None, higher) => self.before_each = higher,
            _ => {}
        }
        match (self.after_each.as_mut(), higher.after_each) {
            (Some(lower), Some(higher)) => lower.merge(higher),
            (None, higher) => self.after_each = higher,
            _ => {}
        }
    }
}

/// `before-each` / `after-each` hook texts: a system message around each user
/// prompt, each assistant reply, or each turn as a whole.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hooks {
    pub user_prompt: Option<String>,
    pub assistant_prompt: Option<String>,
    pub turn: Option<String>,
}

impl Hooks {
    fn is_default(&self) -> bool {
        self.user_prompt.is_none() && self.assistant_prompt.is_none() && self.turn.is_none()
    }

    fn merge(&mut self, higher: Hooks) {
        if higher.user_prompt.is_some() {
            self.user_prompt = higher.user_prompt;
        }
        if higher.assistant_prompt.is_some() {
            self.assistant_prompt = higher.assistant_prompt;
        }
        if higher.turn.is_some() {
            self.turn = higher.turn;
        }
    }
}

/// `tools { … }` inside a scene: an at-most-one `-all` verb plus per-tool
/// overrides.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SceneToolsConfig {
    /// `enable-all` / `ask-all` / `disable-all`; `None` keeps the built-in
    /// behavior (everything enabled, gated by the global permissions).
    pub verb: Option<SceneToolVerb>,

    /// Overrides keyed by tool name.
    pub tools: BTreeMap<String, ToolOverride>,
}

impl SceneToolsConfig {
    fn is_default(&self) -> bool {
        self.verb.is_none() && self.tools.is_empty()
    }

    fn merge(&mut self, higher: SceneToolsConfig) {
        if higher.verb.is_some() {
            self.verb = higher.verb;
        }
        for (name, tool) in higher.tools {
            match self.tools.get_mut(&name) {
                Some(lower) => lower.merge(tool),
                None => {
                    self.tools.insert(name, tool);
                }
            }
        }
    }
}

/// The scene-wide tool verb: `enable-all` (everything enabled), `ask-all`
/// (everything asks), or `disable-all` (only explicitly re-enabled tools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneToolVerb {
    EnableAll,
    AskAll,
    DisableAll,
}

impl SceneToolVerb {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EnableAll => "enable-all",
            Self::AskAll => "ask-all",
            Self::DisableAll => "disable-all",
        }
    }
}

/// One `tool "name" { … }` override: drop the tool from the roster and/or
/// force an ask verdict for it. Both fields are three-valued so an omitted
/// field means "not specified by this entry" (layer merging keeps the lower
/// layer's value).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolOverride {
    /// `None` = unspecified, `Some(false)` = explicitly enabled (re-enabling
    /// under `disable-all`), `Some(true)` = disabled.
    pub disabled: Option<bool>,

    /// `None` = unspecified, `Some(true)` = the tool always asks first.
    pub ask: Option<bool>,
}

impl ToolOverride {
    fn merge(&mut self, higher: ToolOverride) {
        if higher.disabled.is_some() {
            self.disabled = higher.disabled;
        }
        if higher.ask.is_some() {
            self.ask = higher.ask;
        }
    }
}

/// An RGB color: `(red, green, blue)`, each channel `0..=255`.
pub type Rgb = (u8, u8, u8);

/// The palette role names a `theme` node may set, in the order the TUI uses
/// them. A role a theme does not name keeps the built-in Faerun value.
pub const THEME_ROLES: [&str; 22] = [
    "bg",
    "surface",
    "surface-focused",
    "overlay",
    "accent",
    "accent-bg",
    "selection",
    "text",
    "text-dim",
    "text-muted",
    "prompt-bg",
    "running-bg",
    "success-bg",
    "warning-bg",
    "error-bg",
    "diff-add-bg",
    "diff-add-emph-bg",
    "diff-del-bg",
    "diff-del-emph-bg",
    "success",
    "warning",
    "error",
];

/// The terminal appearance a theme definition targets — the `mode` value of
/// a `theme` node and the terminal background state detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThemeVariant {
    #[default]
    Dark,
    Light,
}

impl ThemeVariant {
    /// The kebab-case `mode` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Parses a `mode` value.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            _ => None,
        }
    }
}

impl std::fmt::Display for ThemeVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The resolved palette the TUI paints with, one field per palette role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeColors {
    pub bg: Rgb,
    pub surface: Rgb,
    pub surface_focused: Rgb,
    pub overlay: Rgb,
    pub accent: Rgb,
    pub accent_bg: Rgb,
    pub selection: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub text_muted: Rgb,
    pub prompt_bg: Rgb,
    pub running_bg: Rgb,
    pub success_bg: Rgb,
    pub warning_bg: Rgb,
    pub error_bg: Rgb,
    pub diff_add_bg: Rgb,
    pub diff_add_emph_bg: Rgb,
    pub diff_del_bg: Rgb,
    pub diff_del_emph_bg: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
}

impl ThemeColors {
    /// The built-in Faerun palette — the default theme, defined in code and
    /// never serialized.
    pub const fn faerun() -> Self {
        Self {
            bg: (18, 18, 22),
            surface: (28, 28, 34),
            surface_focused: (38, 38, 46),
            overlay: (34, 34, 42),
            accent: (212, 175, 95),
            accent_bg: (52, 48, 42),
            selection: (78, 60, 28),
            text: (224, 216, 196),
            text_dim: (128, 120, 104),
            text_muted: (92, 86, 74),
            prompt_bg: (44, 38, 30),
            running_bg: (38, 38, 46),
            success_bg: (26, 40, 30),
            warning_bg: (45, 37, 23),
            error_bg: (46, 26, 24),
            diff_add_bg: (34, 58, 42),
            diff_add_emph_bg: (50, 84, 58),
            diff_del_bg: (62, 34, 30),
            diff_del_emph_bg: (94, 50, 44),
            success: (138, 146, 90),
            warning: (192, 152, 72),
            error: (186, 88, 72),
        }
    }

    /// The built-in Faerun palette's light variant — parchment and warm
    /// amber, defined in code and never serialized.
    pub const fn faerun_light() -> Self {
        Self {
            bg: (247, 243, 234),
            surface: (239, 233, 220),
            surface_focused: (231, 223, 207),
            overlay: (250, 247, 240),
            accent: (160, 94, 3),
            accent_bg: (245, 227, 192),
            selection: (240, 224, 184),
            text: (47, 42, 36),
            text_dim: (107, 99, 87),
            text_muted: (138, 129, 116),
            prompt_bg: (238, 231, 216),
            running_bg: (228, 236, 220),
            success_bg: (217, 234, 211),
            warning_bg: (247, 232, 195),
            error_bg: (246, 215, 211),
            diff_add_bg: (220, 240, 216),
            diff_add_emph_bg: (191, 230, 184),
            diff_del_bg: (248, 220, 216),
            diff_del_emph_bg: (245, 192, 186),
            success: (61, 122, 55),
            warning: (163, 98, 10),
            error: (168, 50, 50),
        }
    }

    /// Overwrites every role the theme defines.
    fn apply(&mut self, colors: &BTreeMap<String, Rgb>) {
        for (role, value) in colors {
            match role.as_str() {
                "bg" => self.bg = *value,
                "surface" => self.surface = *value,
                "surface-focused" => self.surface_focused = *value,
                "overlay" => self.overlay = *value,
                "accent" => self.accent = *value,
                "accent-bg" => self.accent_bg = *value,
                "selection" => self.selection = *value,
                "text" => self.text = *value,
                "text-dim" => self.text_dim = *value,
                "text-muted" => self.text_muted = *value,
                "prompt-bg" => self.prompt_bg = *value,
                "running-bg" => self.running_bg = *value,
                "success-bg" => self.success_bg = *value,
                "warning-bg" => self.warning_bg = *value,
                "error-bg" => self.error_bg = *value,
                "diff-add-bg" => self.diff_add_bg = *value,
                "diff-add-emph-bg" => self.diff_add_emph_bg = *value,
                "diff-del-bg" => self.diff_del_bg = *value,
                "diff-del-emph-bg" => self.diff_del_emph_bg = *value,
                "success" => self.success = *value,
                "warning" => self.warning = *value,
                "error" => self.error = *value,
                _ => {}
            }
        }
    }
}

/// `themes { … }` — the named themes: per-theme palette overrides over the
/// built-in Faerun colors. The built-in Faerun theme (dark base plus light
/// variant) is code, not config; this section only defines named themes on
/// top of it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemesConfig {
    /// The defined themes, keyed by name and optional variant name.
    pub themes: BTreeMap<(String, Option<String>), ThemeDef>,
}

impl ThemesConfig {
    /// Parses a standalone `themes` document (the same format as the config
    /// section, as used by `themes.d` drop-ins). Top-level `themes` nodes
    /// stack key-wise; same-level conflicts are only decided across sources,
    /// by [`Config::load_themes`].
    pub fn from_kdl(contents: &str) -> crate::Result<Self> {
        let mut themes = Self::default();
        for source in crate::config_kdl::themes_from_document(contents)? {
            themes.stack(source);
        }
        Ok(themes)
    }

    /// Stacks `higher` over `self`: each theme definition merges its colors
    /// key-wise so a layer can extend a theme defined elsewhere without
    /// hiding it.
    pub fn stack(&mut self, higher: ThemesConfig) {
        for (key, theme) in higher.themes {
            match self.themes.get_mut(&key) {
                Some(lower) => lower.merge(theme),
                None => {
                    self.themes.insert(key, theme);
                }
            }
        }
    }

    /// The theme with the given name: its base definition (no variant).
    pub fn theme(&self, name: &str) -> Option<&ThemeDef> {
        self.themes.get(&(name.to_string(), None))
    }

    /// Every definition of the theme with the given name, base definition
    /// first (the map orders `None` before `Some`, so variants follow in
    /// alphabetical order).
    pub fn defs(&self, name: &str) -> Vec<&ThemeDef> {
        self.themes
            .iter()
            .filter(|(key, _)| key.0 == name)
            .map(|(_, def)| def)
            .collect()
    }
}

/// One named theme definition: palette overrides keyed by role (`accent`,
/// `bg`, …) plus the optional variant it targets and the terminal mode it
/// paints for. Unset roles keep the built-in Faerun values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThemeDef {
    pub colors: BTreeMap<String, Rgb>,

    /// The variant this definition targets; `None` is the theme's base
    /// definition. Part of the definition's key, never overridden by merges.
    pub variant: Option<String>,

    /// The terminal mode this definition paints for — how auto-detection
    /// picks among a theme's definitions when no explicit variant is set.
    pub mode: ThemeVariant,
}

impl ThemeDef {
    fn merge(&mut self, higher: ThemeDef) {
        self.colors.extend(higher.colors);
        self.mode = higher.mode;
    }

    /// The built-in theme definitions, consulted when no user definition
    /// shadows them per name and variant: the Faerun dark base plus its
    /// light variant.
    pub fn builtins() -> Vec<ThemeDef> {
        vec![
            ThemeDef {
                colors: all_roles(ThemeColors::faerun()),
                variant: None,
                mode: ThemeVariant::Dark,
            },
            ThemeDef {
                colors: all_roles(ThemeColors::faerun_light()),
                variant: Some("light".to_string()),
                mode: ThemeVariant::Light,
            },
        ]
    }
}

/// Every palette role mapped to its color, for materializing a built-in
/// palette as a [`ThemeDef`].
fn all_roles(colors: ThemeColors) -> BTreeMap<String, Rgb> {
    let mut roles = BTreeMap::new();
    roles.insert("bg".to_string(), colors.bg);
    roles.insert("surface".to_string(), colors.surface);
    roles.insert("surface-focused".to_string(), colors.surface_focused);
    roles.insert("overlay".to_string(), colors.overlay);
    roles.insert("accent".to_string(), colors.accent);
    roles.insert("accent-bg".to_string(), colors.accent_bg);
    roles.insert("selection".to_string(), colors.selection);
    roles.insert("text".to_string(), colors.text);
    roles.insert("text-dim".to_string(), colors.text_dim);
    roles.insert("text-muted".to_string(), colors.text_muted);
    roles.insert("prompt-bg".to_string(), colors.prompt_bg);
    roles.insert("running-bg".to_string(), colors.running_bg);
    roles.insert("success-bg".to_string(), colors.success_bg);
    roles.insert("warning-bg".to_string(), colors.warning_bg);
    roles.insert("error-bg".to_string(), colors.error_bg);
    roles.insert("diff-add-bg".to_string(), colors.diff_add_bg);
    roles.insert("diff-add-emph-bg".to_string(), colors.diff_add_emph_bg);
    roles.insert("diff-del-bg".to_string(), colors.diff_del_bg);
    roles.insert("diff-del-emph-bg".to_string(), colors.diff_del_emph_bg);
    roles.insert("success".to_string(), colors.success);
    roles.insert("warning".to_string(), colors.warning);
    roles.insert("error".to_string(), colors.error);
    roles
}

/// One config source's `themes` section, with the display label used in
/// same-level conflict warnings.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeSource {
    /// Display label: `shuvarie.kdl`, `.shuvarie/config.kdl`, `config.kdl`,
    /// or `themes.d/<file>`.
    pub label: String,

    pub themes: ThemesConfig,

    /// Whether the source belongs to the workspace (local) level; `false` is
    /// the global config's layer. Levels decide override order and same-level
    /// conflicts, so the flag is recorded at load time — deriving it from the
    /// chain position would misclassify a local layer as the global one when
    /// the global config file does not exist.
    pub local: bool,
}

/// The runtime theme set [`Config::load_themes`] builds: the merged theme
/// config plus one warning per same-level conflict (a definition defined by
/// more than one source of the same level loads neither copy).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ThemeSet {
    pub themes: ThemesConfig,

    pub warnings: Vec<String>,
}

/// The palette a run paints with plus the warnings collected while resolving
/// it: same-level conflicts from [`ThemeSet`], and one warning when `ui.theme`
/// names a theme nothing defines.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTheme {
    pub colors: ThemeColors,

    pub warnings: Vec<String>,
}

impl ResolvedTheme {
    /// The built-in Faerun palette, no warnings.
    pub fn faerun() -> Self {
        Self {
            colors: ThemeColors::faerun(),
            warnings: Vec::new(),
        }
    }
}

impl ThemeSet {
    /// Resolves the palette for the run: `pref` (`ui.theme`) selects a theme
    /// as `name` or `name:variant`. The exact string names a theme first, so
    /// names containing `:` keep working; otherwise the last `:` separates an
    /// explicit variant. With no explicit variant the theme's definitions are
    /// picked by `detected` (the terminal background's mode): definitions
    /// whose `mode` match win — the base definition first, then alphabetical
    /// variants — and with no match the base definition, else the first
    /// remaining one, is used so a defined theme still wins over the
    /// built-in. `Faerun` resolves against its built-in dark and light
    /// palettes; user definitions shadow built-ins per name and variant.
    /// Unknown names and unknown explicit variants warn and fall back to the
    /// built-in Faerun.
    pub fn resolve(&self, pref: Option<&str>, detected: ThemeVariant) -> ResolvedTheme {
        let mut colors = builtin_colors(detected);
        let mut warnings = self.warnings.clone();
        if let Some(pref) = pref {
            let (name, variant) = self.split_pref(pref);
            let defs = self.candidates(&name);
            if defs.is_empty() {
                if name != BUILTIN_FAERUN {
                    warnings.push(format!(
                        "theme `{pref}` is not defined by `themes {{ … }}` or a `themes.d` drop-in; using the built-in Faerun theme"
                    ));
                }
            } else if let Some(variant) = variant {
                match defs
                    .iter()
                    .find(|def| def.variant.as_deref() == Some(variant.as_str()))
                {
                    Some(def) => colors.apply(&def.colors),
                    None => warnings.push(format!(
                        "theme `{pref}` has no `{variant}` variant defined by `themes {{ … }}` or a `themes.d` drop-in; using the built-in Faerun theme"
                    )),
                }
            } else {
                colors.apply(&pick_by_mode(&defs, detected).colors);
            }
        }
        ResolvedTheme { colors, warnings }
    }

    /// Splits `ui.theme` into a theme name and an optional explicit variant:
    /// the exact whole string names a defined theme first (so names
    /// containing `:` keep working), otherwise the last `:` separates a
    /// variant.
    fn split_pref(&self, pref: &str) -> (String, Option<String>) {
        if !self.candidates(pref).is_empty() {
            return (pref.to_string(), None);
        }
        match pref.rsplit_once(':') {
            Some((name, variant)) if !name.is_empty() && !variant.is_empty() => {
                (name.to_string(), Some(variant.to_string()))
            }
            _ => (pref.to_string(), None),
        }
    }

    /// Every definition of the theme with the given name: the merged user
    /// definitions plus the unshadowed built-ins (only `Faerun` has those),
    /// base definition first, then alphabetical variants.
    fn candidates(&self, name: &str) -> Vec<ThemeDef> {
        let mut defs: Vec<ThemeDef> = self
            .themes
            .themes
            .iter()
            .filter(|(key, _)| key.0 == name)
            .map(|(_, def)| def.clone())
            .collect();
        if name == BUILTIN_FAERUN {
            for builtin in ThemeDef::builtins() {
                let key = (BUILTIN_FAERUN.to_string(), builtin.variant.clone());
                if !self.themes.themes.contains_key(&key) {
                    defs.push(builtin);
                }
            }
        }
        defs
    }
}

/// The built-in theme's name; `themes` definitions with this name shadow the
/// built-in palettes per variant.
const BUILTIN_FAERUN: &str = "Faerun";

/// The built-in Faerun palette for the detected terminal mode.
fn builtin_colors(detected: ThemeVariant) -> ThemeColors {
    match detected {
        ThemeVariant::Dark => ThemeColors::faerun(),
        ThemeVariant::Light => ThemeColors::faerun_light(),
    }
}

/// Picks the definition painting for the detected terminal mode:
/// definitions whose `mode` match win — base first, then alphabetical
/// variants (the candidate order) — and with no match the base definition
/// if present, else the first remaining one, keeps the theme alive.
fn pick_by_mode(defs: &[ThemeDef], detected: ThemeVariant) -> &ThemeDef {
    defs.iter()
        .find(|def| def.mode == detected)
        .unwrap_or(&defs[0])
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

/// Cap on the characters taken from the first user prompt for a new
/// session's provisional title (`ui.title` `first-user-prompt`, and the
/// stand-in title while an LLM-drafted one is pending).
pub(crate) const DEFAULT_TITLE_PROMPT_CHARS: usize = 30;

/// How a new session's title is drafted (`ui.title`). Defaults to
/// [`TitleConfig::FirstUserPrompt`], and the three alternatives are mutually
/// exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleConfig {
    /// The title stays part of the first user prompt: trimmed, capped at
    /// `max_chars` (the `max-chars` property). No LLM call is made.
    FirstUserPrompt {
        /// Cap on the characters taken from the prompt.
        max_chars: usize,
    },
    /// The title is drafted by an LLM right after the first user prompt
    /// creates the session, replacing the provisional prompt prefix once it
    /// arrives (`by-llm { … }`). Each field defaults: `provider` to the
    /// active provider connection, `model` to that provider's catalog
    /// default small model, and `system_prompt` to the built-in title
    /// prompt.
    ByLlm {
        /// Provider connection name the title call runs on.
        provider: Option<String>,
        /// Model id the title call runs on.
        model: Option<String>,
        /// System prompt for the title call.
        system_prompt: Option<String>,
    },
    /// No automatic titling: sessions start as `Untitled session` until
    /// renamed by hand (`disabled #true`).
    Disabled,
}

impl Default for TitleConfig {
    fn default() -> Self {
        Self::FirstUserPrompt {
            max_chars: DEFAULT_TITLE_PROMPT_CHARS,
        }
    }
}

impl TitleConfig {
    /// Cap on the characters taken from the first user prompt for the
    /// provisional title: the `first-user-prompt` cap when that policy is
    /// selected, the built-in default otherwise (the stand-in title while an
    /// LLM-drafted one is pending).
    pub fn prompt_max_chars(&self) -> usize {
        match self {
            Self::FirstUserPrompt { max_chars } => *max_chars,
            Self::ByLlm { .. } | Self::Disabled => DEFAULT_TITLE_PROMPT_CHARS,
        }
    }
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

    /// The theme the TUI paints with: `theme "name"` selects a theme defined
    /// by `themes { … }` or a `themes.d` drop-in. `None` keeps the built-in
    /// Faerun theme; an unknown name warns and keeps Faerun.
    pub theme: Option<String>,

    /// How a new session's title is drafted. Defaults to deriving it from
    /// part of the first user prompt; see [`TitleConfig`].
    pub title: TitleConfig,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            frame_rate: 60,
            sidebar: SidebarPref::Auto,
            copy_on_select: false,
            theme: None,
            title: TitleConfig::default(),
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

/// The sorted `*.kdl` drop-ins of `dir` (a missing or non-directory `dir`
/// yields none).
fn dropin_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "kdl"))
        .collect();
    files.sort();
    files
}

/// Loads the sorted `*.kdl` scene drop-ins of `dir` as level sources, one per
/// top-level `scenes` node (a missing or non-directory `dir` yields none). A
/// file that fails to read or parse is skipped with a warning instead of
/// failing the load.
fn load_scene_dir_in(dir: &std::path::Path, local: bool) -> (Vec<SceneSource>, Vec<String>) {
    let mut sources = Vec::new();
    let mut warnings = Vec::new();
    for file in dropin_files(dir) {
        let base = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("scene.kdl");
        let skip = |error: String| format!("{SCENE_DIR_NAME}/{base}: {error} (file skipped)");
        let contents = match std::fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(error) => {
                warnings.push(skip(error.to_string()));
                continue;
            }
        };
        let blocks = match config_kdl::scenes_from_document(&contents) {
            Ok(blocks) => blocks,
            Err(error) => {
                warnings.push(skip(error.to_string()));
                continue;
            }
        };
        let numbered = blocks.len() > 1;
        for (idx, scenes) in blocks.into_iter().enumerate() {
            let label = if numbered {
                format!("{SCENE_DIR_NAME}/{base} (block {})", idx + 1)
            } else {
                format!("{SCENE_DIR_NAME}/{base}")
            };
            sources.push(SceneSource {
                label,
                scenes,
                local,
            });
        }
    }
    (sources, warnings)
}

/// Loads the sorted `*.kdl` theme drop-ins of `dir` as level sources, one per
/// top-level `themes` node (a missing or non-directory `dir` yields none). A
/// file that fails to read or parse is skipped with a warning instead of
/// failing the load.
fn load_theme_dir_in(dir: &std::path::Path, local: bool) -> (Vec<ThemeSource>, Vec<String>) {
    let mut sources = Vec::new();
    let mut warnings = Vec::new();
    for file in dropin_files(dir) {
        let base = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("theme.kdl");
        let skip = |error: String| format!("{THEMES_DIR_NAME}/{base}: {error} (file skipped)");
        let contents = match std::fs::read_to_string(&file) {
            Ok(contents) => contents,
            Err(error) => {
                warnings.push(skip(error.to_string()));
                continue;
            }
        };
        let blocks = match config_kdl::themes_from_document(&contents) {
            Ok(blocks) => blocks,
            Err(error) => {
                warnings.push(skip(error.to_string()));
                continue;
            }
        };
        let numbered = blocks.len() > 1;
        for (idx, themes) in blocks.into_iter().enumerate() {
            let label = if numbered {
                format!("{THEMES_DIR_NAME}/{base} (block {})", idx + 1)
            } else {
                format!("{THEMES_DIR_NAME}/{base}")
            };
            sources.push(ThemeSource {
                label,
                themes,
                local,
            });
        }
    }
    (sources, warnings)
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
        let mut paths = Self::local_config_candidates(&cwd)
            .into_iter()
            .map(|path| (path, true))
            .collect::<Vec<_>>();
        paths.push((Self::config_path()?, false));
        Self::load_chain(&paths)
    }

    /// Like [`Self::load`], but the workspace config layers only load when
    /// the `configs` trust category is granted; the global config always
    /// loads. Used with the workspace-trust decision made at startup.
    pub fn load_trusted(
        cwd: &std::path::Path,
        grants: &crate::trusts::TrustGrants,
    ) -> Result<Self> {
        Self::load_chain(&Self::trusted_chain_paths(
            cwd,
            grants,
            &Self::config_path()?,
        ))
    }

    /// The priority chain for a trust decision: the workspace config layers
    /// only when `configs` is granted, then the global config.
    fn trusted_chain_paths(
        cwd: &std::path::Path,
        grants: &crate::trusts::TrustGrants,
        global: &std::path::Path,
    ) -> Vec<(PathBuf, bool)> {
        let mut paths = Vec::new();
        if grants.allows(crate::trusts::Category::Configs) {
            paths.extend(
                Self::local_config_candidates(cwd)
                    .into_iter()
                    .map(|path| (path, true)),
            );
        }
        paths.push((global.to_path_buf(), false));
        paths
    }

    /// The global `scene.d` drop-in dir.
    pub fn global_scene_dir() -> Result<PathBuf> {
        Ok(config_dir()?.join(SCENE_DIR_NAME))
    }

    /// The workspace-root `scene.d` drop-in dir (next to `shuvarie.kdl`).
    pub fn root_scene_dir(cwd: &std::path::Path) -> PathBuf {
        cwd.join(SCENE_DIR_NAME)
    }

    /// The workspace `scene.d` drop-in dir.
    pub fn workspace_scene_dir(cwd: &std::path::Path) -> PathBuf {
        cwd.join(WORKSPACE_DIR_NAME).join(SCENE_DIR_NAME)
    }

    /// The global `themes.d` drop-in dir.
    pub fn global_themes_dir() -> Result<PathBuf> {
        Ok(config_dir()?.join(THEMES_DIR_NAME))
    }

    /// The workspace-root `themes.d` drop-in dir (next to `shuvarie.kdl`).
    pub fn root_themes_dir(cwd: &std::path::Path) -> PathBuf {
        cwd.join(THEMES_DIR_NAME)
    }

    /// The workspace `themes.d` drop-in dir.
    pub fn workspace_themes_dir(cwd: &std::path::Path) -> PathBuf {
        cwd.join(WORKSPACE_DIR_NAME).join(THEMES_DIR_NAME)
    }

    /// Loads the `*.kdl` theme drop-ins of `dir` as one level (sorted by
    /// filename; a missing or empty dir yields an empty set; a file that
    /// fails to read or parse is skipped with a warning).
    pub fn load_theme_dir(dir: &std::path::Path) -> ThemeSet {
        let (sources, mut warnings) = load_theme_dir_in(dir, false);
        let (themes, merge_warnings) = merge_theme_level("theme dir", &sources);
        warnings.extend(merge_warnings);
        ThemeSet { themes, warnings }
    }

    /// Loads the `*.kdl` scene drop-ins of `dir` as one level (sorted by
    /// filename; a missing or empty dir yields an empty set; a file that
    /// fails to read or parse is skipped with a warning).
    pub fn load_scene_dir(dir: &std::path::Path) -> SceneSet {
        let (sources, mut warnings) = load_scene_dir_in(dir, false);
        let (scenes, merge_warnings) = merge_level("scene dir", &sources);
        warnings.extend(merge_warnings);
        SceneSet { scenes, warnings }
    }

    /// The runtime scene set for an app run, built from two levels. The
    /// global level is the chain's global config layer (recorded per source
    /// at load time) plus the global `scene.d` drop-ins. The local level is
    /// the chain's local layers — or the explicit `--config` file as the
    /// single layer, replacing the global config layer — plus the workspace
    /// `scene.d` drop-ins (the workspace-root `./scene.d` first, then the
    /// nested `<WORKSPACE_DIR_NAME>/scene.d`), which load only when the
    /// `configs` trust category is granted and no explicit config was named;
    /// with an explicit config the drop-in dir next to that file is used.
    /// A drop-in file that fails to read or parse is skipped with a warning.
    /// Within a level a scene name must be unique: a name defined by more
    /// than one source is a conflict reported in `SceneSet::warnings` and
    /// neither copy loads. Across levels the local level overrides the
    /// global one field-wise per scene name, and the built-in Default scene
    /// stays the fallback when a name resolves nowhere.
    pub fn load_scenes(
        config: &Config,
        cwd: &std::path::Path,
        grants: &crate::trusts::TrustGrants,
        explicit: Option<&std::path::Path>,
    ) -> Result<SceneSet> {
        let mut sources = config.scene_sources.clone();
        if sources.is_empty() {
            sources.push(SceneSource {
                label: "config.kdl".to_string(),
                scenes: config.scenes.clone(),
                local: false,
            });
        }
        let (local_layers, mut global_layers): (Vec<SceneSource>, Vec<SceneSource>) =
            sources.into_iter().partition(|source| source.local);
        if explicit.is_some() {
            global_layers.clear();
        }
        let global_layer = global_layers.pop();
        let granted = explicit.is_none() && grants.allows(crate::trusts::Category::Configs);
        let dropin_dirs: Vec<PathBuf> = if explicit.is_some() {
            vec![
                explicit
                    .map(|path| {
                        path.parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .join(SCENE_DIR_NAME)
                    })
                    .unwrap_or_default(),
            ]
        } else if granted {
            vec![Self::root_scene_dir(cwd), Self::workspace_scene_dir(cwd)]
        } else {
            Vec::new()
        };
        Ok(scene_set_from_levels(
            global_layer,
            local_layers,
            &Self::global_scene_dir()?,
            &dropin_dirs,
        ))
    }

    /// The runtime theme set for an app run, built from two levels: the same
    /// layout as [`Self::load_scenes`] with `themes.d` drop-in dirs — the
    /// global level is the chain's global config layer plus the global
    /// `themes.d`, the local level is the chain's local layers (or the
    /// explicit `--config` file as the single layer) plus the workspace
    /// `themes.d` dirs (the workspace-root `./themes.d` first, then the
    /// nested `<WORKSPACE_DIR_NAME>/themes.d`), which load only when the
    /// `configs` trust category is granted and no explicit config was named;
    /// with an explicit config the drop-in dir next to that file is used.
    /// Within a level a theme name must be unique: a name defined by more
    /// than one source is a conflict reported in `ThemeSet::warnings` and
    /// neither copy loads. Across levels the local level overrides the
    /// global one key-wise per theme name, and the built-in Faerun theme
    /// stays the fallback when `ui.theme` resolves nowhere.
    pub fn load_themes(
        config: &Config,
        cwd: &std::path::Path,
        grants: &crate::trusts::TrustGrants,
        explicit: Option<&std::path::Path>,
    ) -> Result<ThemeSet> {
        let mut sources = config.theme_sources.clone();
        if sources.is_empty() {
            sources.push(ThemeSource {
                label: "config.kdl".to_string(),
                themes: config.themes.clone(),
                local: false,
            });
        }
        let (local_layers, mut global_layers): (Vec<ThemeSource>, Vec<ThemeSource>) =
            sources.into_iter().partition(|source| source.local);
        if explicit.is_some() {
            global_layers.clear();
        }
        let global_layer = global_layers.pop();
        let granted = explicit.is_none() && grants.allows(crate::trusts::Category::Configs);
        let dropin_dirs: Vec<PathBuf> = if explicit.is_some() {
            vec![
                explicit
                    .map(|path| {
                        path.parent()
                            .unwrap_or_else(|| std::path::Path::new("."))
                            .join(THEMES_DIR_NAME)
                    })
                    .unwrap_or_default(),
            ]
        } else if granted {
            vec![Self::root_themes_dir(cwd), Self::workspace_themes_dir(cwd)]
        } else {
            Vec::new()
        };
        Ok(theme_set_from_levels(
            global_layer,
            local_layers,
            &Self::global_themes_dir()?,
            &dropin_dirs,
        ))
    }
    fn load_chain(paths: &[(PathBuf, bool)]) -> Result<Self> {
        let mut config = Self::default();
        let mut permissions = Vec::new();
        let mut scene_sources = Vec::new();
        let mut theme_sources = Vec::new();
        for (path, local) in paths {
            if let Some(layer) = read_layer(path)? {
                scene_sources.push(SceneSource {
                    label: source_label(path),
                    scenes: layer.config.scenes.clone(),
                    local: *local,
                });
                theme_sources.push(ThemeSource {
                    label: source_label(path),
                    themes: layer.config.themes.clone(),
                    local: *local,
                });
                if layer.sections.contains("permissions") {
                    permissions.push(layer.config.permissions.clone());
                }
                merge_layer(&mut config, &layer);
            }
        }
        config.permissions = stack_permissions(&permissions);
        let layers: Vec<ScenesConfig> = scene_sources.iter().map(|s| s.scenes.clone()).collect();
        config.scenes = stack_scenes(&layers);
        config.scene_sources = scene_sources;
        let layers: Vec<ThemesConfig> = theme_sources.iter().map(|s| s.themes.clone()).collect();
        config.themes = stack_themes(&layers);
        config.theme_sources = theme_sources;
        Ok(config)
    }

    /// Loads exactly the given file, ignoring the priority chain. Unlike the
    /// chain, a missing file is an error: the user named this file
    /// explicitly (`--config`).
    pub fn load_explicit(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let mut config = config_kdl::from_kdl(&contents)?;
                config.scene_sources = vec![SceneSource {
                    label: source_label(path),
                    scenes: config.scenes.clone(),
                    local: true,
                }];
                config.theme_sources = vec![ThemeSource {
                    label: source_label(path),
                    themes: config.themes.clone(),
                    local: true,
                }];
                Ok(config)
            }
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
    fn ui_title_defaults_to_first_user_prompt() {
        for text in [
            "",
            "ui { title }",
            "ui { title { first-user-prompt } }",
            "ui { title { first-user-prompt max-chars=30 } }",
        ] {
            let parsed = config_kdl::from_kdl(text).unwrap();
            assert_eq!(parsed.ui.title, TitleConfig::default(), "text: {text:?}");
        }
        assert_eq!(TitleConfig::default().prompt_max_chars(), 30);
    }

    #[test]
    fn ui_title_first_user_prompt_max_chars() {
        let parsed =
            config_kdl::from_kdl("ui { title { first-user-prompt max-chars=12 } }").unwrap();
        assert_eq!(parsed.ui.title.prompt_max_chars(), 12);
    }

    #[test]
    fn ui_title_disabled_round_trips() {
        let parsed = config_kdl::from_kdl("ui { title { disabled #true } }").unwrap();
        assert_eq!(parsed.ui.title, TitleConfig::Disabled);
        let text = config_kdl::to_kdl(&parsed).unwrap();
        assert!(text.contains("disabled #true"), "body: {text}");
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
        assert!(
            !config_kdl::to_kdl(&Config::default())
                .unwrap()
                .contains("title")
        );
    }

    #[test]
    fn ui_title_by_llm_round_trips() {
        let text = r#"
            ui {
                title {
                    by-llm {
                        provider "Openai"
                        model "gpt-5.6-luna"
                        system-prompt """
                        You name sessions.
                        """
                    }
                }
            }
        "#;
        let parsed = config_kdl::from_kdl(text).unwrap();
        let TitleConfig::ByLlm {
            provider,
            model,
            system_prompt,
        } = &parsed.ui.title
        else {
            panic!("expected the by-llm policy, got {:?}", parsed.ui.title);
        };
        assert_eq!(provider.as_deref(), Some("Openai"));
        assert_eq!(model.as_deref(), Some("gpt-5.6-luna"));
        assert!(
            system_prompt
                .as_deref()
                .unwrap()
                .contains("You name sessions.")
        );

        let text = config_kdl::to_kdl(&parsed).unwrap();
        assert!(text.contains("by-llm"), "body: {text}");
        assert!(text.contains("system-prompt"), "body: {text}");
        assert!(text.contains("provider"), "body: {text}");
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn ui_title_bare_by_llm_round_trips() {
        let parsed = config_kdl::from_kdl("ui { title { by-llm } }").unwrap();
        assert_eq!(
            parsed.ui.title,
            TitleConfig::ByLlm {
                provider: None,
                model: None,
                system_prompt: None,
            }
        );
        let text = config_kdl::to_kdl(&parsed).unwrap();
        assert!(text.contains("by-llm"), "body: {text}");
        let reparsed = config_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn ui_title_parse_errors() {
        let cases: &[(&str, &str)] = &[
            (
                "ui {\n    title {\n        first-user-prompt max-chars=30\n        by-llm\n    }\n}",
                "at most one",
            ),
            (
                "ui {\n    title {\n        disabled #true\n        first-user-prompt\n    }\n}",
                "at most one",
            ),
            ("ui { title { bogus } }", "unknown node `bogus` in `title`"),
            (
                "ui { title { by-llm { bogus \"x\" } } }",
                "unknown node `bogus` in `by-llm`",
            ),
            (
                "ui { title { first-user-prompt wrong=\"x\" } }",
                "unknown property `wrong`",
            ),
            (
                "ui { title { first-user-prompt 42 } }",
                "takes no positional arguments",
            ),
            (
                "ui { title { first-user-prompt max-chars=0 } }",
                "greater than zero",
            ),
            (
                "ui { title { first-user-prompt max-chars=\"30\" } }",
                "an integer",
            ),
            (
                "ui { title { first-user-prompt max-chars=-1 } }",
                "out of range",
            ),
            ("ui { title { disabled } }", "must be `#true`"),
            ("ui { title { disabled #false } }", "must be `#true`"),
            (
                "ui { title { by-llm { system-prompt \"\" } } }",
                "must not be empty",
            ),
            ("ui { title 42 }", "takes no positional arguments"),
        ];
        for (text, expected) in cases {
            let error = config_kdl::from_kdl(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}\n{error}");
        }
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

        let config = Config::load_chain(&[(global, false), (inner, true), (top, true)]).unwrap();
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

        let config = Config::load_chain(&[(global, false), (top, true)]).unwrap();
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

        let config =
            Config::load_chain(&[(dir.path().join("shuvarie.kdl"), true), (global, false)])
                .unwrap();
        assert_eq!(config.ui.frame_rate, 24);
    }

    #[test]
    fn chain_no_files_is_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let paths = vec![
            (dir.path().join("shuvarie.kdl"), true),
            (dir.path().join(".shuvarie/config.kdl"), true),
            (dir.path().join("global.kdl"), false),
        ];
        assert_eq!(Config::load_chain(&paths).unwrap(), Config::default());
    }

    #[test]
    fn chain_parse_error_propagates() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(&top, "ui {\n    frame-rate \"sixty\"\n}").unwrap();

        let err = Config::load_chain(&[(top, true)]).unwrap_err();
        let ConfigError::Parse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
    }

    #[test]
    fn trusted_chain_paths_skip_workspace_layers_when_configs_denied() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let global = cwd.join("global.kdl");

        let paths = Config::trusted_chain_paths(
            cwd,
            &TrustGrants::from_categories([Category::Skills]),
            &global,
        );
        assert_eq!(paths, vec![(global.clone(), false)]);

        let paths = Config::trusted_chain_paths(cwd, &TrustGrants::all(), &global);
        assert_eq!(
            paths,
            Config::local_config_candidates(cwd)
                .into_iter()
                .map(|path| (path, true))
                .chain([(global, false)])
                .collect::<Vec<_>>()
        );
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

        let config = Config::load_chain(&[(global, false), (top, true)]).unwrap();
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
        let config = Config::load_chain(&[(global, false), (top, true)]).unwrap();
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
        let config = Config::load_chain(&[(top, true), (global, false)]).unwrap();
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
        let config = Config::load_chain(&[(top, true), (global, false)]).unwrap();
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
        let config = Config::load_chain(&[(top, true)]).unwrap();
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
        let config = Config::load_chain(&[(top.clone(), true), (global.clone(), false)]).unwrap();
        assert_eq!(
            config.permissions.paths.default,
            Some(Verb::Allow),
            "the top file set no bare verb, the global one survives"
        );
        assert_eq!(config.permissions.paths.rules.len(), 2);

        std::fs::write(&top, "permissions { paths { ask-all } }").unwrap();
        let config = Config::load_chain(&[(top, true), (global, false)]).unwrap();
        assert_eq!(
            config.permissions.paths.default,
            Some(Verb::Ask),
            "the top file's bare verb wins"
        );
    }

    fn scenes(text: &str) -> ScenesConfig {
        config_kdl::from_kdl(text).unwrap().scenes
    }

    fn config_with_scenes(sc: ScenesConfig) -> Config {
        Config {
            scenes: sc,
            ..Config::default()
        }
    }

    fn scene_example() -> &'static str {
        r#"
            scenes {
                default "Plan"
                scene name="Plan" {
                    description "Plan before acting"
                    subagents {
                        disabled #true
                        editor {
                            system-prompts {
                                prelude """
You are a helpful editor, working on the manuscript.
You polish prose before publishing.
"""
                                interlude """
Now we're in Plan mode: plan first, no edits.
"""
                            }
                            thinking #false
                            tools {
                                enable-all
                                tool "edit_file" {
                                    disabled #true
                                }
                            }
                        }
                    }
                    system-prompts {
                        prelude """
You are a helpful tool, working in Plan mode.
You plan before acting.
"""
                        interlude """
Now we're in Plan mode: plan first, no edits.
"""
                        before-each {
                            user-prompt "before user"
                            assistant-prompt "before assistant"
                            turn "before turn"
                        }
                        after-each {
                            turn "after turn"
                        }
                    }
                    thinking #false
                    tools {
                        disable-all
                        tool "write_file" "edit_file" {
                            disabled #true
                            ask #false
                        }
                        tool "run_shell" {
                            ask #true
                        }
                    }
                }
                scene name="Build" {
                    tools {
                        tool "run_shell" {
                            disabled #true
                        }
                    }
                }
            }
        "#
    }

    #[test]
    fn scenes_absent_by_default() {
        let parsed = config_kdl::from_kdl("ui { frame-rate 30 }").unwrap();
        assert_eq!(parsed.scenes, ScenesConfig::default());
        assert_eq!(parsed.scenes.default, None);
        assert!(parsed.scenes.scenes.is_empty());
        assert!(
            !config_kdl::to_kdl(&Config::default())
                .unwrap()
                .contains("scenes")
        );
    }

    #[test]
    fn scenes_full_example_parses() {
        let parsed = scenes(scene_example());
        assert_eq!(parsed.default.as_deref(), Some("Plan"));
        assert_eq!(parsed.scenes.len(), 2);

        let plan = parsed.scene("Plan").unwrap();
        assert_eq!(plan.description.as_deref(), Some("Plan before acting"));
        assert!(plan.subagents.disabled);
        assert_eq!(plan.subagents.workers.len(), 1, "workers key by node name");
        let editor = plan.subagents.workers.get("editor").unwrap();
        assert_eq!(
            editor.system_prompts.prelude.as_deref(),
            Some(
                "You are a helpful editor, working on the manuscript.\nYou polish prose before publishing."
            )
        );
        assert_eq!(
            editor.system_prompts.interlude.as_deref(),
            Some("Now we're in Plan mode: plan first, no edits.")
        );
        assert_eq!(editor.thinking, Some(false));
        assert_eq!(editor.tools.verb, Some(SceneToolVerb::EnableAll));
        assert_eq!(
            editor.tools.tools.get("edit_file").map(|t| t.disabled),
            Some(Some(true))
        );
        assert_eq!(
            plan.system_prompts.prelude.as_deref(),
            Some("You are a helpful tool, working in Plan mode.\nYou plan before acting.")
        );
        assert_eq!(
            plan.system_prompts.interlude.as_deref(),
            Some("Now we're in Plan mode: plan first, no edits.")
        );
        let before = plan.system_prompts.before_each.as_ref().unwrap();
        assert_eq!(before.user_prompt.as_deref(), Some("before user"));
        assert_eq!(before.assistant_prompt.as_deref(), Some("before assistant"));
        assert_eq!(before.turn.as_deref(), Some("before turn"));
        let after = plan.system_prompts.after_each.as_ref().unwrap();
        assert_eq!(after.turn.as_deref(), Some("after turn"));
        assert_eq!(after.user_prompt, None);
        assert_eq!(plan.thinking, Some(false));
        assert_eq!(plan.tools.verb, Some(SceneToolVerb::DisableAll));
        let write = plan.tools.tools.get("write_file").unwrap();
        assert_eq!(write.disabled, Some(true));
        assert_eq!(write.ask, Some(false));
        assert_eq!(
            plan.tools.tools.get("edit_file").map(|t| t.ask),
            Some(Some(false))
        );
        assert_eq!(
            plan.tools.tools.get("run_shell").map(|t| t.ask),
            Some(Some(true))
        );

        let build = parsed.scene("Build").unwrap();
        assert_eq!(
            build.tools.tools.get("run_shell").map(|t| t.disabled),
            Some(Some(true))
        );
    }

    #[test]
    fn scenes_round_trip_with_multiline_prompts() {
        let parsed = scenes(scene_example());
        let out = config_kdl::to_kdl(&config_with_scenes(parsed.clone())).unwrap();
        assert!(out.contains("\"\"\""), "multiline prompts stay raw: {out}");
        let reparsed = config_kdl::from_kdl(&out).unwrap();
        assert_eq!(parsed, reparsed.scenes);
    }

    #[test]
    fn scenes_bare_scene_stays_declared() {
        let parsed = scenes(r#"scenes { scene name="Only" }"#);
        assert!(parsed.scene("Only").unwrap().is_default());
        let out = config_kdl::to_kdl(&config_with_scenes(parsed.clone())).unwrap();
        assert!(out.contains("scene name=Only"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().scenes, parsed);
    }

    #[test]
    fn scenes_default_round_trips() {
        let parsed = scenes(r#"scenes { default "Build" }"#);
        assert_eq!(parsed.default.as_deref(), Some("Build"));
        let out = config_kdl::to_kdl(&config_with_scenes(parsed)).unwrap();
        assert!(out.contains("default Build"), "body: {out}");
    }

    #[test]
    fn scenes_parse_errors() {
        let cases: &[(&str, &str)] = &[
            ("scenes {\n    scene { }\n}", "requires a `name`"),
            ("scenes { scene name=\"\" }", "must name a scene"),
            ("scenes { default \"\" }", "must name a scene"),
            (
                "scenes {\n    scene name=\"A\" { description \"x\" }\n    scene name=\"A\"\n}",
                "duplicate",
            ),
            ("scenes { bogus }", "unknown node"),
            ("scenes { scene name=\"A\" { bogus } }", "unknown node"),
            (
                "scenes {\n    scene name=\"A\" {\n        tools {\n            enable-all\n            ask-all\n        }\n    }\n}",
                "duplicate",
            ),
            (
                "scenes { scene name=\"A\" { tools { tool \"x\" { } } } }",
                "requires `disabled` or `ask`",
            ),
            (
                "scenes {\n    scene name=\"A\" {\n        tools {\n            tool \"x\" { disabled #true }\n            tool \"x\" { ask #true }\n        }\n    }\n}",
                "duplicate",
            ),
            (
                "scenes { scene name=\"A\" { tools { tool 42 { disabled #true } } } }",
                "tool name",
            ),
            (
                "scenes { scene name=\"A\" { tools { tool { disabled #true } } } }",
                "at least one tool name",
            ),
            (
                "scenes {\n    scene name=\"A\" {\n        tools {\n            enable-all { disabled #true }\n        }\n    }\n}",
                "takes no children",
            ),
            (
                "scenes { scene name=\"A\" { system-prompts { prelude \"  \" } } }",
                "must not be empty",
            ),
            (
                "scenes {\n    scene name=\"A\" {\n        subagents {\n            editor\n            editor\n        }\n    }\n}",
                "duplicate",
            ),
            (
                "scenes { scene name=\"A\" { subagents { editor { bogus } } } }",
                "unknown node",
            ),
            (
                "scenes { scene name=\"A\" { subagents { editor name=\"x\" } } }",
                "takes no arguments",
            ),
            (
                "scenes { scene name=\"A\" { system-prompts { bogus \"x\" } } }",
                "unknown node",
            ),
            (
                "scenes { scene name=\"A\" { before-each { turn \"x\" } } }",
                "unknown node",
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
    fn scenes_chain_merges_fieldwise() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(
            &global,
            r#"
            scenes {
                scene name="Plan" {
                    description "global plan"
                    system-prompts { prelude "global prelude" }
                }
                scene name="Only-Global"
            }
        "#,
        )
        .unwrap();
        std::fs::write(
            &top,
            r#"
            scenes {
                default "Plan"
                scene name="Plan" {
                    tools { disable-all }
                }
            }
        "#,
        )
        .unwrap();

        let config = Config::load_chain(&[(top, true), (global, false)]).unwrap();
        assert_eq!(config.scenes.default.as_deref(), Some("Plan"));
        assert!(config.scenes.scenes.contains_key("Only-Global"));
        let plan = config.scenes.scene("Plan").unwrap();
        assert_eq!(plan.description.as_deref(), Some("global plan"));
        assert_eq!(
            plan.system_prompts.prelude.as_deref(),
            Some("global prelude")
        );
        assert_eq!(plan.tools.verb, Some(SceneToolVerb::DisableAll));
    }

    #[test]
    fn scene_dir_loads_sorted_kdl_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let scene_dir = dir.path().join(SCENE_DIR_NAME);
        std::fs::create_dir_all(&scene_dir).unwrap();
        std::fs::write(
            scene_dir.join("10-build.kdl"),
            r#"
            scenes {
                scene name="Build" { description "from drop-in" }
            }
            ignored { whatever #true }
        "#,
        )
        .unwrap();
        std::fs::write(
            scene_dir.join("20-plan.kdl"),
            r#"
            scenes {
                scene name="Plan" { description "from drop-in" }
            }
        "#,
        )
        .unwrap();
        std::fs::write(scene_dir.join("30-notes.txt"), "not kdl").unwrap();
        std::fs::create_dir_all(scene_dir.join("40-subdir.kdl")).unwrap();

        let scenes = Config::load_scene_dir(&scene_dir).scenes;
        assert_eq!(scenes.scenes.len(), 2);
        assert_eq!(
            scenes.scene("Plan").unwrap().description.as_deref(),
            Some("from drop-in")
        );
        assert_eq!(
            scenes.scene("Build").unwrap().description.as_deref(),
            Some("from drop-in")
        );

        assert_eq!(
            Config::load_scene_dir(&dir.path().join("missing")),
            SceneSet::default()
        );
    }

    #[test]
    fn scene_dir_parse_error_warns_and_skips() {
        let dir = tempfile::tempdir().unwrap();
        let scene_dir = dir.path().join(SCENE_DIR_NAME);
        std::fs::create_dir_all(&scene_dir).unwrap();
        std::fs::write(
            scene_dir.join("10-plan.kdl"),
            r#"scenes { scene name="Plan" { tools { disable-all } } }"#,
        )
        .unwrap();
        std::fs::write(
            scene_dir.join("20-typo.kdl"),
            r#"scenes { scene name="Typo" { tools { disabled-all } } }"#,
        )
        .unwrap();

        let set = Config::load_scene_dir(&scene_dir);
        assert!(set.scenes.scene("Plan").is_some(), "valid drop-ins load");
        assert!(set.scenes.scene("Typo").is_none(), "broken file skipped");
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("scene.d/20-typo.kdl"));
        assert!(set.warnings[0].contains("(file skipped)"));
    }

    #[test]
    fn dropin_warnings_survive_the_level_merge() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let dropins = dir.path().join("dropins");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&dropins).unwrap();
        std::fs::write(
            dropins.join("10-bad.kdl"),
            r#"scenes { scene name="Bad" { tools { disabled-all } } }"#,
        )
        .unwrap();

        let set = scene_set_from_levels(None, Vec::new(), &global, &[dropins]);
        assert!(
            set.warnings.iter().any(|warning| {
                warning.contains("scene.d/10-bad.kdl") && warning.contains("(file skipped)")
            }),
            "warnings: {:?}",
            set.warnings
        );
    }

    #[test]
    fn scene_dir_duplicates_conflict_and_load_neither() {
        let dir = tempfile::tempdir().unwrap();
        let scene_dir = dir.path().join(SCENE_DIR_NAME);
        std::fs::create_dir_all(&scene_dir).unwrap();
        std::fs::write(
            scene_dir.join("10-plan.kdl"),
            r#"scenes { scene name="Plan" { description "one" } }"#,
        )
        .unwrap();
        std::fs::write(
            scene_dir.join("20-plan.kdl"),
            r#"scenes { scene name="Plan" { tools { ask-all } }
 scene name="Keep" }"#,
        )
        .unwrap();
        std::fs::write(
            scene_dir.join("30-blocks.kdl"),
            r#"scenes { scene name="Plan" { description "block" } }"#,
        )
        .unwrap();

        let set = Config::load_scene_dir(&scene_dir);
        assert!(set.scenes.scene("Plan").is_none(), "neither copy loads");
        assert!(set.scenes.scene("Keep").is_some(), "unaffected scenes load");
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("`Plan`"));
        assert!(set.warnings[0].contains("scene.d/10-plan.kdl"));
        assert!(set.warnings[0].contains("scene.d/20-plan.kdl"));
        assert!(set.warnings[0].contains("scene.d/30-blocks.kdl"));
    }

    #[test]
    fn scene_levels_local_overrides_global_fieldwise() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(global.join("a.kdl"), "scenes {\n    scene name=\"B\"\n}").unwrap();
        std::fs::write(
            workspace.join("z.kdl"),
            r#"scenes { scene name="A" { system-prompts { prelude "local" } } }"#,
        )
        .unwrap();
        let global_layer = SceneSource {
            label: "config.kdl".to_string(),
            scenes: scenes(
                "scenes {\n    scene name=\"A\" {\n        description \"global\"\n        tools { ask-all }\n    }\n}",
            ),
            local: false,
        };

        let set = scene_set_from_levels(Some(global_layer), Vec::new(), &global, &[workspace]);
        let a = set.scenes.scene("A").unwrap();
        assert_eq!(a.description.as_deref(), Some("global"), "global field");
        assert_eq!(
            a.system_prompts.prelude.as_deref(),
            Some("local"),
            "the local field wins"
        );
        assert_eq!(
            a.tools.verb,
            Some(SceneToolVerb::AskAll),
            "a global-only field falls through"
        );
        assert!(set.scenes.scene("B").is_some(), "global drop-in scene");
        assert!(set.warnings.is_empty(), "cross-level names do not conflict");
    }

    #[test]
    fn same_level_duplicates_conflict_and_load_neither() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("plan.kdl"),
            "scenes {\n    scene name=\"Plan\" { system-prompts { prelude \"g\" } }\n    scene name=\"Only-Global\"\n}",
        )
        .unwrap();
        let global_layer = SceneSource {
            label: "config.kdl".to_string(),
            scenes: scenes(r#"scenes { scene name="Plan" { tools { ask-all } } }"#),
            local: false,
        };

        let set = scene_set_from_levels(Some(global_layer), Vec::new(), &global, &[]);
        assert!(
            set.scenes.scene("Plan").is_none(),
            "neither global copy loads"
        );
        assert!(set.scenes.scene("Only-Global").is_some());
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("global config"));
        assert!(set.warnings[0].contains("config.kdl"));
        assert!(set.warnings[0].contains("scene.d/plan.kdl"));
    }

    #[test]
    fn a_local_scene_survives_a_global_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::write(
            global.join("plan.kdl"),
            r#"scenes { scene name="Plan" { description "g1" } }"#,
        )
        .unwrap();
        let global_layer = SceneSource {
            label: "config.kdl".to_string(),
            scenes: scenes(r#"scenes { scene name="Plan" { description "g2" } }"#),
            local: false,
        };
        let local = SceneSource {
            label: "shuvarie.kdl".to_string(),
            scenes: scenes(
                r#"scenes { scene name="Plan" { system-prompts { prelude "local" } } }"#,
            ),
            local: true,
        };

        let set = scene_set_from_levels(Some(global_layer), vec![local], &global, &[]);
        let plan = set.scenes.scene("Plan").unwrap();
        assert_eq!(
            plan.description.as_deref(),
            None,
            "the conflicted global copies contribute nothing"
        );
        assert_eq!(plan.system_prompts.prelude.as_deref(), Some("local"));
        assert_eq!(set.warnings.len(), 1);
    }

    #[test]
    fn local_config_layers_conflict_within_the_local_level() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        std::fs::create_dir_all(&global).unwrap();
        let top = SceneSource {
            label: "shuvarie.kdl".to_string(),
            scenes: scenes(r#"scenes { scene name="Plan" { description "a" } }"#),
            local: true,
        };
        let nested = SceneSource {
            label: ".shuvarie/config.kdl".to_string(),
            scenes: scenes(
                "scenes {\n    scene name=\"Plan\" { description \"b\" }\n    scene name=\"Keep\"\n}",
            ),
            local: true,
        };
        let global_layer = SceneSource {
            label: "config.kdl".to_string(),
            scenes: ScenesConfig::default(),
            local: false,
        };

        let set = scene_set_from_levels(Some(global_layer), vec![top, nested], &global, &[]);
        assert!(set.scenes.scene("Plan").is_none());
        assert!(set.scenes.scene("Keep").is_some());
        assert!(
            set.warnings[0].contains("local config"),
            "{:#?}",
            set.warnings
        );
        assert!(set.warnings[0].contains("shuvarie.kdl"));
        assert!(set.warnings[0].contains(".shuvarie/config.kdl"));
    }

    #[test]
    fn level_default_takes_the_highest_priority_source() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let workspace = dir.path().join("ws");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("a.kdl"), r#"scenes { default "Dropin" }"#).unwrap();
        let global_layer = SceneSource {
            label: "config.kdl".to_string(),
            scenes: scenes(r#"scenes { default "Global" }"#),
            local: false,
        };
        let local_layer = SceneSource {
            label: "shuvarie.kdl".to_string(),
            scenes: scenes(r#"scenes { default "Local" }"#),
            local: true,
        };

        let set =
            scene_set_from_levels(Some(global_layer), vec![local_layer], &global, &[workspace]);
        assert_eq!(
            set.scenes.default.as_deref(),
            Some("Local"),
            "config layers over drop-ins within the level, local over global"
        );
    }

    #[test]
    fn explicit_config_records_a_single_scene_source() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("my.kdl");
        std::fs::write(&path, r#"scenes { scene name="A" { description "x" } }"#).unwrap();

        let config = Config::load_explicit(&path).unwrap();
        assert_eq!(config.scene_sources.len(), 1);
        assert_eq!(config.scene_sources[0].label, "my.kdl");
        assert!(config.scene_sources[0].scenes.scene("A").is_some());
    }

    /// Serializes tests that redirect the global config dir via
    /// `XDG_CONFIG_HOME` (process-global env, read by `config_dir()`).
    fn global_dir_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// An end-to-end startup against an isolated global config dir: a
    /// workspace config layer plus a workspace scene.d drop-in must land in
    /// the local level when `configs` is granted, and the global config's
    /// scene must not shadow the local one.
    #[test]
    fn local_scenes_load_end_to_end_when_configs_are_granted() {
        let _lock = global_dir_lock();
        let global_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // SAFETY: serialized behind `global_dir_lock`.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", global_home.path()) };
        let cfg_dir = global_home.path().join(CONFIG_DIR_NAME);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.kdl"),
            "scenes { scene name=\"Plan\" { description \"global\" } }",
        )
        .unwrap();
        let ws_dir = workspace.path().join(WORKSPACE_DIR_NAME);
        std::fs::create_dir_all(ws_dir.join("scene.d")).unwrap();
        std::fs::write(
            ws_dir.join("config.kdl"),
            "scenes { scene name=\"Plan\" { description \"local\" } }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.join("scene.d").join("draft.kdl"),
            "scenes { scene name=\"Draft\" { description \"drop-in\" } }",
        )
        .unwrap();

        let grants = crate::trusts::TrustGrants::from_categories([
            crate::trusts::Category::Configs,
            crate::trusts::Category::Contexts,
        ]);
        let config = Config::load_trusted(workspace.path(), &grants).unwrap();
        let set = Config::load_scenes(&config, workspace.path(), &grants, None).unwrap();
        // SAFETY: restoring the test process env.
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };

        let plan = set.scenes.scene("Plan").unwrap();
        assert_eq!(plan.description.as_deref(), Some("local"), "local wins");
        assert_eq!(
            set.scenes.scene("Draft").unwrap().description.as_deref(),
            Some("drop-in")
        );
        assert!(set.warnings.is_empty());
    }

    /// Without the global config file the local layers must still form the
    /// local level: two workspace files defining the same scene name are a
    /// same-level conflict, not a cross-level override.
    /// A workspace-root `./scene.d` loads as a local source too — the same
    /// level as the nested `<WORKSPACE_DIR_NAME>/scene.d`, so a scene name
    /// defined in both is a same-level conflict and neither copy loads.
    #[test]
    fn root_scene_dir_loads_as_a_local_source() {
        let _lock = global_dir_lock();
        let global_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let ws_dir = workspace.path().join(WORKSPACE_DIR_NAME);
        std::fs::create_dir_all(ws_dir.join("scene.d")).unwrap();
        std::fs::create_dir_all(workspace.path().join("scene.d")).unwrap();
        std::fs::write(
            workspace.path().join("scene.d").join("root.kdl"),
            "scenes {\n    scene name=\"Plan\" { description \"root\" }\n    scene name=\"Only-Root\"\n}",
        )
        .unwrap();
        std::fs::write(
            ws_dir.join("scene.d").join("nested.kdl"),
            "scenes {\n    scene name=\"Plan\" { description \"nested\" }\n    scene name=\"Only-Nested\"\n}",
        )
        .unwrap();

        let grants =
            crate::trusts::TrustGrants::from_categories([crate::trusts::Category::Configs]);
        // SAFETY: isolated global dir for this test.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", global_home.path()) };
        let config = Config::load_trusted(workspace.path(), &grants).unwrap();
        let set = Config::load_scenes(&config, workspace.path(), &grants, None).unwrap();
        // SAFETY: restoring the test process env.
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };

        assert!(
            set.scenes.scene("Plan").is_none(),
            "same name across local scene.d dirs conflicts"
        );
        assert!(set.scenes.scene("Only-Root").is_some());
        assert!(set.scenes.scene("Only-Nested").is_some());
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("scene.d/root.kdl"));
        assert!(set.warnings[0].contains("scene.d/nested.kdl"));
    }

    #[test]
    fn local_layers_conflict_even_without_a_global_config() {
        let _lock = global_dir_lock();
        let global_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // SAFETY: serialized behind `global_dir_lock`.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", global_home.path()) };
        let ws_dir = workspace.path().join(WORKSPACE_DIR_NAME);
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            workspace.path().join(LOCAL_CONFIG_FILE_NAME),
            "scenes { scene name=\"Plan\" { description \"top\" } }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.join("config.kdl"),
            "scenes { scene name=\"Plan\" { description \"nested\" } }",
        )
        .unwrap();

        let grants =
            crate::trusts::TrustGrants::from_categories([crate::trusts::Category::Configs]);
        let config = Config::load_trusted(workspace.path(), &grants).unwrap();
        let set = Config::load_scenes(&config, workspace.path(), &grants, None).unwrap();
        // SAFETY: restoring the test process env.
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };

        assert!(
            set.scenes.scene("Plan").is_none(),
            "same-name local layers conflict"
        );
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("local config"));
        assert!(set.warnings[0].contains(LOCAL_CONFIG_FILE_NAME));
        assert!(set.warnings[0].contains(".shuvarie-dev/config.kdl"));
    }

    fn themes(text: &str) -> ThemesConfig {
        config_kdl::from_kdl(text).unwrap().themes
    }

    fn config_with_themes(th: ThemesConfig) -> Config {
        Config {
            themes: th,
            ..Config::default()
        }
    }

    fn theme_example() -> &'static str {
        r##"
            themes {
                theme name="Ayu" {
                    bg "#0b0e14"
                    surface "#151a23"
                    surface-focused "#1c2230"
                    overlay "#1f2430"
                    accent "#ffb454"
                    accent-bg "#2d3644"
                    selection "#2d4a63"
                    text "#e6e1cf"
                    text-dim "#8a919e"
                    text-muted "#5c6773"
                    prompt-bg "#1c2130"
                    running-bg "#20283a"
                    success-bg "#1c2b23"
                    warning-bg "#322b1a"
                    error-bg "#331d19"
                    diff-add-bg "#203528"
                    diff-add-emph-bg "#2c4f3a"
                    diff-del-bg "#3a2224"
                    diff-del-emph-bg "#543032"
                    success "#a6cc70"
                    warning "#e6b674"
                    error "#f07178"
                }
                theme name="One Dark" {
                    bg "#282c34"
                    accent "#98c379"
                }
            }
        "##
    }

    #[test]
    fn themes_absent_by_default() {
        let parsed = config_kdl::from_kdl("ui { frame-rate 30 }").unwrap();
        assert_eq!(parsed.themes, ThemesConfig::default());
        assert!(parsed.themes.themes.is_empty());
        assert!(
            !config_kdl::to_kdl(&Config::default())
                .unwrap()
                .contains("themes")
        );
    }

    #[test]
    fn themes_full_example_parses() {
        let parsed = themes(theme_example());
        assert_eq!(parsed.themes.len(), 2);

        let ayu = parsed.theme("Ayu").unwrap();
        assert_eq!(ayu.colors.len(), 22, "every palette role is set");
        assert_eq!(ayu.colors.get("bg"), Some(&(11, 14, 20)));
        assert_eq!(ayu.colors.get("accent"), Some(&(255, 180, 84)));
        assert_eq!(ayu.colors.get("success"), Some(&(166, 204, 112)));

        let one_dark = parsed.theme("One Dark").unwrap();
        assert_eq!(one_dark.colors.get("bg"), Some(&(40, 44, 52)));
        assert_eq!(one_dark.colors.get("accent"), Some(&(152, 195, 121)));
        assert_eq!(
            one_dark.colors.len(),
            2,
            "unset roles keep the Faerun values"
        );
    }

    #[test]
    fn themes_round_trip() {
        let parsed = themes(theme_example());
        let out = config_kdl::to_kdl(&config_with_themes(parsed.clone())).unwrap();
        assert!(out.contains("theme name=Ayu"), "body: {out}");
        assert!(out.contains("0b0e14"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().themes, parsed);
    }

    #[test]
    fn themes_bare_theme_stays_declared() {
        let parsed = themes(r##"themes { theme name="Only" }"##);
        assert!(parsed.theme("Only").unwrap().colors.is_empty());
        let out = config_kdl::to_kdl(&config_with_themes(parsed.clone())).unwrap();
        assert!(out.contains("theme name=Only"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().themes, parsed);
    }

    #[test]
    fn ui_theme_round_trips() {
        let parsed = config_kdl::from_kdl("ui { theme \"Ayu\" }").unwrap();
        assert_eq!(parsed.ui.theme.as_deref(), Some("Ayu"));
        let out = config_kdl::to_kdl(&parsed).unwrap();
        assert!(out.contains("theme Ayu"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().ui, parsed.ui);
        assert!(
            !config_kdl::to_kdl(&Config::default())
                .unwrap()
                .contains("theme")
        );
    }

    #[test]
    fn themes_parse_errors() {
        let cases: &[(&str, &str)] = &[
            ("themes { theme { } }", "requires a `name`"),
            ("themes { theme name=\"\" }", "must name a theme"),
            (
                "themes {\n    theme name=\"A\" { bg \"#000000\" }\n    theme name=\"A\"\n}",
                "duplicate",
            ),
            ("themes { bogus }", "unknown node"),
            (
                "themes { theme name=\"A\" { bogus \"#000000\" } }",
                "unknown color",
            ),
            (
                "themes { theme name=\"A\" { accent } }",
                "requires a color value",
            ),
            (
                "themes { theme name=\"A\" { bg \"#00\" } }",
                "must be a hex color",
            ),
            (
                "themes { theme name=\"A\" { bg \"#gggggg\" } }",
                "must be a hex color",
            ),
            (
                "themes { theme name=\"A\" { bg \"#12345\" } }",
                "must be a hex color",
            ),
            (
                "themes { theme name=\"A\" { bg \"#00000000\" } }",
                "must be a hex color",
            ),
            (
                "themes { theme name=\"A\" { bg \"#00000g\" } }",
                "must be a hex color",
            ),
            (
                "themes {\n    theme name=\"A\" {\n        bg \"#000000\"\n        bg \"#111111\"\n    }\n}",
                "duplicate",
            ),
            (
                "themes { theme name=\"A\" { bg \"#000000\" { nested } } }",
                "takes no children",
            ),
            (
                "themes { theme name=\"A\" 42 }",
                "takes no positional arguments",
            ),
            (
                "themes { theme name=\"A\" wrong=\"x\" }",
                "unknown property",
            ),
            ("ui { theme \"\" }", "must name a theme"),
            ("ui { theme \"  \" }", "must name a theme"),
            ("ui { theme \"a\" \"b\" }", "takes a single argument"),
        ];
        for (text, expected) in cases {
            let error = config_kdl::from_kdl(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}\n{error}");
        }
    }

    #[test]
    fn themes_chain_merges_fieldwise() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.kdl");
        let top = dir.path().join("shuvarie.kdl");
        std::fs::write(
            &global,
            r##"
            themes {
                theme name="Ayu" {
                    bg "#0b0e14"
                    accent "#ffb454"
                }
                theme name="Only-Global" {
                    accent "#ff0000"
                }
            }
        "##,
        )
        .unwrap();
        std::fs::write(
            &top,
            r##"
            themes {
                theme name="Ayu" {
                    accent "#00ff00"
                }
            }
        "##,
        )
        .unwrap();

        let config = Config::load_chain(&[(top, true), (global, false)]).unwrap();
        assert!(
            config
                .themes
                .themes
                .contains_key(&("Only-Global".to_string(), None))
        );
        let ayu = config.themes.theme("Ayu").unwrap();
        assert_eq!(
            ayu.colors.get("bg"),
            Some(&(11, 14, 20)),
            "the top file only overrides its own roles"
        );
        assert_eq!(ayu.colors.get("accent"), Some(&(0, 255, 0)));
    }

    #[test]
    fn theme_dir_loads_sorted_kdl_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let themes_dir = dir.path().join(THEMES_DIR_NAME);
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("10-ayu.kdl"),
            r##"
            themes {
                theme name="Ayu" { accent "#ffb454" }
            }
            ignored { whatever #true }
        "##,
        )
        .unwrap();
        std::fs::write(
            themes_dir.join("20-dark.kdl"),
            r##"
            themes {
                theme name="One Dark" { accent "#98c379" }
            }
        "##,
        )
        .unwrap();
        std::fs::write(themes_dir.join("30-notes.txt"), "not kdl").unwrap();
        std::fs::create_dir_all(themes_dir.join("40-subdir.kdl")).unwrap();

        let set = Config::load_theme_dir(&themes_dir);
        assert_eq!(set.themes.themes.len(), 2);
        assert!(set.themes.theme("Ayu").is_some());
        assert!(set.themes.theme("One Dark").is_some());
        assert!(set.warnings.is_empty());

        assert_eq!(
            Config::load_theme_dir(&dir.path().join("missing")),
            ThemeSet::default()
        );
    }

    #[test]
    fn theme_dir_parse_error_warns_and_skips() {
        let dir = tempfile::tempdir().unwrap();
        let themes_dir = dir.path().join(THEMES_DIR_NAME);
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("10-ayu.kdl"),
            r##"themes { theme name="Ayu" { accent "#ffb454" } }"##,
        )
        .unwrap();
        std::fs::write(
            themes_dir.join("20-bad.kdl"),
            r##"themes { theme name="Bad" { primary-color "#ff0000" } }"##,
        )
        .unwrap();

        let set = Config::load_theme_dir(&themes_dir);
        assert!(
            set.themes.theme("Ayu").is_some(),
            "the parseable file still loads"
        );
        assert!(set.themes.theme("Bad").is_none());
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("themes.d/20-bad.kdl"));
        assert!(set.warnings[0].contains("(file skipped)"));
    }

    #[test]
    fn theme_dropin_warnings_survive_the_level_merge() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global");
        let dropins = dir.path().join("dropins");
        std::fs::create_dir_all(&global).unwrap();
        std::fs::create_dir_all(&dropins).unwrap();
        std::fs::write(
            dropins.join("10-bad.kdl"),
            r##"themes { theme name="Bad" { accent "not-a-color" } }"##,
        )
        .unwrap();

        let set = theme_set_from_levels(None, Vec::new(), &global, &[dropins]);
        assert!(
            set.warnings.iter().any(|warning| {
                warning.contains("themes.d/10-bad.kdl") && warning.contains("(file skipped)")
            }),
            "warnings: {:?}",
            set.warnings
        );
    }

    #[test]
    fn theme_dir_duplicates_conflict_and_load_neither() {
        let dir = tempfile::tempdir().unwrap();
        let themes_dir = dir.path().join(THEMES_DIR_NAME);
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("10-ayu.kdl"),
            r##"themes { theme name="Ayu" { accent "#ffb454" } }"##,
        )
        .unwrap();
        std::fs::write(
            themes_dir.join("20-ayu.kdl"),
            r##"themes { theme name="Ayu" { accent "#00ff00" } }"##,
        )
        .unwrap();

        let set = Config::load_theme_dir(&themes_dir);
        assert!(set.themes.theme("Ayu").is_none(), "neither copy loads");
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("theme dir"));
        assert!(set.warnings[0].contains("10-ayu.kdl"));
        assert!(set.warnings[0].contains("20-ayu.kdl"));
    }

    #[test]
    fn theme_levels_local_overrides_global_fieldwise() {
        let global = ThemeSource {
            label: "config.kdl".to_string(),
            themes: themes(
                r##"
                themes {
                    theme name="Ayu" {
                        bg "#0b0e14"
                        accent "#ffb454"
                    }
                    theme name="Only-Global"
                }
            "##,
            ),
            local: false,
        };
        let local = ThemeSource {
            label: "shuvarie.kdl".to_string(),
            themes: themes(
                r##"
                themes {
                    theme name="Ayu" {
                        accent "#00ff00"
                    }
                }
            "##,
            ),
            local: true,
        };

        let set = theme_set_from_levels(
            Some(global),
            vec![local],
            &std::path::Path::new("missing"),
            &[],
        );
        let ayu = set.themes.theme("Ayu").unwrap();
        assert_eq!(
            ayu.colors.get("bg"),
            Some(&(11, 14, 20)),
            "the local level only overrides its own roles"
        );
        assert_eq!(ayu.colors.get("accent"), Some(&(0, 255, 0)));
        assert!(set.themes.theme("Only-Global").is_some());
        assert!(set.warnings.is_empty());
    }

    #[test]
    fn theme_dir_duplicates_conflict_across_sources_of_one_level() {
        let global = ThemeSource {
            label: "config.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" { accent "#ffb454" } }"##),
            local: false,
        };
        let first = ThemeSource {
            label: "themes.d/10-ayu.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" { accent "#00ff00" } }"##),
            local: true,
        };
        let second = ThemeSource {
            label: "themes.d/20-ayu.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" { accent "#0000ff" } }"##),
            local: true,
        };

        let set = theme_set_from_levels(
            Some(global),
            vec![first, second],
            &std::path::Path::new("missing"),
            &[],
        );
        assert_eq!(
            set.themes.theme("Ayu").unwrap().colors.get("accent"),
            Some(&(255, 180, 84)),
            "the local copies conflict, the global one survives"
        );
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("local config"));
        assert!(set.warnings[0].contains("themes.d/10-ayu.kdl"));
        assert!(set.warnings[0].contains("themes.d/20-ayu.kdl"));
    }

    #[test]
    fn explicit_config_records_a_single_theme_source() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("custom.kdl");
        std::fs::write(
            &file,
            "themes { theme name=\"Ayu\" { accent \"#ffb454\" } }",
        )
        .unwrap();

        let config = Config::load_explicit(&file).unwrap();
        assert_eq!(config.theme_sources.len(), 1);
        assert!(config.theme_sources[0].local);
        assert_eq!(config.theme_sources[0].label, "custom.kdl");
    }

    /// An end-to-end startup against an isolated global config dir: a
    /// workspace theme layer plus a workspace themes.d drop-in must land in
    /// the local level when `configs` is granted, and the global config's
    /// theme must not shadow the local one.
    #[test]
    fn local_themes_load_end_to_end_when_configs_are_granted() {
        let _lock = global_dir_lock();
        let global_home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        // SAFETY: serialized behind `global_dir_lock`.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", global_home.path()) };
        let cfg_dir = global_home.path().join(CONFIG_DIR_NAME);
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.kdl"),
            "themes { theme name=\"Ayu\" { accent \"#111111\" } }",
        )
        .unwrap();
        let ws_dir = workspace.path().join(WORKSPACE_DIR_NAME);
        std::fs::create_dir_all(ws_dir.join("themes.d")).unwrap();
        std::fs::write(
            ws_dir.join("config.kdl"),
            "themes { theme name=\"Ayu\" { accent \"#222222\" } }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.join("themes.d").join("draft.kdl"),
            "themes { theme name=\"Draft\" { accent \"#333333\" } }",
        )
        .unwrap();

        let grants = crate::trusts::TrustGrants::from_categories([
            crate::trusts::Category::Configs,
            crate::trusts::Category::Contexts,
        ]);
        let config = Config::load_trusted(workspace.path(), &grants).unwrap();
        let set = Config::load_themes(&config, workspace.path(), &grants, None).unwrap();
        // SAFETY: restoring the test process env.
        unsafe { std::env::remove_var("XDG_CONFIG_HOME") };

        let resolved = set.resolve(Some("Ayu"), ThemeVariant::Dark);
        assert_eq!(
            resolved.colors.accent,
            (34, 34, 34),
            "the local layer's accent wins"
        );
        assert_eq!(
            resolved.colors.bg,
            ThemeColors::faerun().bg,
            "unset roles keep the built-in values"
        );
        assert!(set.themes.theme("Draft").is_some(), "drop-ins load too");
        assert!(set.warnings.is_empty());
    }

    #[test]
    fn resolve_falls_back_to_faerun_and_warns_on_unknown_names() {
        let set = ThemeSet::default();
        assert_eq!(
            set.resolve(None, ThemeVariant::Dark).colors,
            ThemeColors::faerun()
        );
        assert_eq!(
            set.resolve(None, ThemeVariant::Light).colors,
            ThemeColors::faerun_light()
        );
        assert_eq!(
            set.resolve(Some("Faerun"), ThemeVariant::Light).colors,
            ThemeColors::faerun_light()
        );
        assert!(
            set.resolve(Some("Faerun"), ThemeVariant::Dark)
                .warnings
                .is_empty()
        );
        assert_eq!(
            set.resolve(Some("Ayu"), ThemeVariant::Dark).colors,
            ThemeColors::faerun()
        );
        let warnings = set.resolve(Some("Ayu"), ThemeVariant::Light).warnings;
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("Ayu"));
    }

    #[test]
    fn resolve_applies_overrides_fieldwise_over_faerun() {
        let set = ThemeSet {
            themes: themes(
                r##"
                themes {
                    theme name="Ayu" {
                        accent "#ffb454"
                        text-dim "#8a919e"
                    }
                }
            "##,
            ),
            warnings: Vec::new(),
        };
        let resolved = set.resolve(Some("Ayu"), ThemeVariant::Dark);
        assert_eq!(resolved.colors.accent, (255, 180, 84));
        assert_eq!(resolved.colors.text_dim, (138, 145, 158));
        assert_eq!(
            resolved.colors.text,
            ThemeColors::faerun().text,
            "unset roles keep the built-in values"
        );
        assert!(resolved.warnings.is_empty());
    }

    #[test]
    fn a_user_defined_faerun_shadows_the_builtin() {
        let set = ThemeSet {
            themes: themes(
                r##"
                themes {
                    theme name="Faerun" {
                        accent "#ff0000"
                    }
                }
            "##,
            ),
            warnings: Vec::new(),
        };
        let resolved = set.resolve(Some("Faerun"), ThemeVariant::Dark);
        assert_eq!(resolved.colors.accent, (255, 0, 0));
        assert_eq!(
            resolved.colors.bg,
            ThemeColors::faerun().bg,
            "the built-in palette still fills the unset roles"
        );
        assert!(resolved.warnings.is_empty());
    }

    fn theme_set(text: &str) -> ThemeSet {
        ThemeSet {
            themes: themes(text),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn theme_variant_and_mode_round_trip() {
        let parsed = themes(
            r##"
            themes {
                theme name="Ayu" variant="light" mode="light" {
                    accent "#ffb454"
                }
                theme name="Ayu" {
                    accent "#ff8800"
                }
            }
        "##,
        );
        assert_eq!(parsed.themes.len(), 2, "base and variant coexist");
        assert!(
            parsed.theme("Ayu").is_some(),
            "the base definition is keyed"
        );
        assert!(parsed.theme("Ayu").unwrap().mode == ThemeVariant::Dark);

        let defs = parsed.defs("Ayu");
        assert_eq!(defs.len(), 2);
        assert!(defs[0].variant.is_none(), "base first");
        assert_eq!(defs[1].variant.as_deref(), Some("light"));
        assert_eq!(defs[1].mode, ThemeVariant::Light);

        let out = config_kdl::to_kdl(&config_with_themes(parsed.clone())).unwrap();
        assert!(out.contains("variant=light"), "body: {out}");
        assert!(out.contains("mode=light"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().themes, parsed);
    }

    #[test]
    fn theme_mode_defaults_to_dark_and_is_omitted_from_saves() {
        let parsed = themes(r##"themes { theme name="A" { accent "#000000" } }"##);
        assert_eq!(parsed.defs("A")[0].mode, ThemeVariant::Dark);
        let out = config_kdl::to_kdl(&config_with_themes(parsed.clone())).unwrap();
        assert!(!out.contains("mode"), "body: {out}");
        assert_eq!(config_kdl::from_kdl(&out).unwrap().themes, parsed);
    }

    #[test]
    fn theme_variant_and_mode_parse_errors() {
        let cases: &[(&str, &str)] = &[
            (
                "themes { theme name=\"A\" variant=\"\" { accent \"#000000\" } }",
                "must name a variant",
            ),
            (
                "themes { theme name=\"A\" mode=\"blue\" { accent \"#000000\" } }",
                "must be `dark` or `light`",
            ),
            (
                "themes {\n    theme name=\"A\" variant=\"light\" { accent \"#000000\" }\n    theme name=\"A\" variant=\"light\"\n}",
                "duplicate",
            ),
            (
                "themes { theme name=\"A\" mode=\"dark\" mode=\"light\" { accent \"#000000\" } }",
                "duplicate",
            ),
            (
                "themes { theme name=\"A\" wrong=\"x\" { accent \"#000000\" } }",
                "unknown property",
            ),
        ];
        for (text, expected) in cases {
            let error = config_kdl::from_kdl(text).unwrap_err();
            assert!(error.to_string().contains(expected), "{text}\n{error}");
        }
    }

    #[test]
    fn theme_variant_conflicts_within_a_level_drop_the_variant_only() {
        let global = ThemeSource {
            label: "config.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" { accent "#ffb454" } }"##),
            local: false,
        };
        let first = ThemeSource {
            label: "themes.d/10-ayu.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" variant="light" { accent "#00ff00" } }"##),
            local: true,
        };
        let second = ThemeSource {
            label: "themes.d/20-ayu.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" variant="light" { accent "#0000ff" } }"##),
            local: true,
        };

        let set = theme_set_from_levels(
            Some(global),
            vec![first, second],
            &std::path::Path::new("missing"),
            &[],
        );
        assert_eq!(set.themes.defs("Ayu").len(), 1, "the base def survives");
        assert!(set.themes.theme("Ayu").is_some());
        assert_eq!(set.warnings.len(), 1, "warnings: {:?}", set.warnings);
        assert!(set.warnings[0].contains("theme `Ayu:light`"));
        assert!(set.warnings[0].contains("themes.d/10-ayu.kdl"));
        assert!(set.warnings[0].contains("themes.d/20-ayu.kdl"));
    }

    #[test]
    fn theme_variants_merge_across_levels() {
        let global = ThemeSource {
            label: "config.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" { accent "#ffb454" } }"##),
            local: false,
        };
        let local = ThemeSource {
            label: "shuvarie.kdl".to_string(),
            themes: themes(r##"themes { theme name="Ayu" variant="light" { accent "#00ff00" } }"##),
            local: true,
        };

        let set = theme_set_from_levels(
            Some(global),
            vec![local],
            &std::path::Path::new("missing"),
            &[],
        );
        assert_eq!(set.themes.defs("Ayu").len(), 2);
        assert!(set.warnings.is_empty());
    }

    #[test]
    fn resolve_picks_by_mode_with_a_base_definition() {
        let set = theme_set(
            r##"
            themes {
                theme name="Ayu" { accent "#111111" }
                theme name="Ayu" variant="light" mode="light" { accent "#222222" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("Ayu"), ThemeVariant::Dark).colors.accent,
            (17, 17, 17)
        );
        assert_eq!(
            set.resolve(Some("Ayu"), ThemeVariant::Light).colors.accent,
            (34, 34, 34)
        );
        assert_eq!(
            set.resolve(Some("Ayu:light"), ThemeVariant::Dark)
                .colors
                .accent,
            (34, 34, 34),
            "an explicit variant ignores the detected mode"
        );
    }

    #[test]
    fn resolve_no_base_theme_picks_by_mode() {
        let set = theme_set(
            r##"
            themes {
                theme name="Solarized" variant="solarized-light" mode="light" { accent "#111111" }
                theme name="Solarized" variant="solarized-dark" mode="dark" { accent "#222222" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("Solarized"), ThemeVariant::Dark)
                .colors
                .accent,
            (34, 34, 34)
        );
        assert_eq!(
            set.resolve(Some("Solarized"), ThemeVariant::Light)
                .colors
                .accent,
            (17, 17, 17)
        );
    }

    #[test]
    fn resolve_light_base_wins_on_light_terminals() {
        let set = theme_set(
            r##"
            themes {
                theme name="Paper" mode="light" { accent "#111111" }
                theme name="Paper" variant="midnight" mode="dark" { accent "#222222" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("Paper"), ThemeVariant::Dark).colors.accent,
            (34, 34, 34)
        );
        assert_eq!(
            set.resolve(Some("Paper"), ThemeVariant::Light)
                .colors
                .accent,
            (17, 17, 17),
            "the base definition matches the mode"
        );
    }

    #[test]
    fn resolve_falls_back_to_base_then_first_remaining() {
        let set = theme_set(
            r##"
            themes {
                theme name="A" { accent "#111111" }
                theme name="A" variant="bright" mode="light" { accent "#222222" }
                theme name="B" variant="solar" mode="light" { accent "#333333" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("A"), ThemeVariant::Dark).colors.accent,
            (17, 17, 17),
            "no mode matches: the base definition keeps the theme alive"
        );
        assert_eq!(
            set.resolve(Some("B"), ThemeVariant::Dark).colors.accent,
            (51, 51, 51),
            "no base: the first remaining definition wins over Faerun"
        );
    }

    #[test]
    fn resolve_unknown_explicit_variant_warns_and_falls_back() {
        let set = theme_set(r##"themes { theme name="Ayu" { accent "#111111" } }"##);
        let resolved = set.resolve(Some("Ayu:midnight"), ThemeVariant::Dark);
        assert_eq!(resolved.colors, ThemeColors::faerun());
        assert_eq!(resolved.warnings.len(), 1);
        assert!(resolved.warnings[0].contains("Ayu:midnight"));
        assert!(resolved.warnings[0].contains("midnight"));
    }

    #[test]
    fn resolve_exact_name_wins_over_variant_split() {
        let set = theme_set(
            r##"
            themes {
                theme name="One:Dark" { accent "#111111" }
                theme name="One" { accent "#222222" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("One:Dark"), ThemeVariant::Dark)
                .colors
                .accent,
            (17, 17, 17),
            "the whole string names a theme"
        );
    }

    #[test]
    fn resolve_user_defs_shadow_builtins_per_variant() {
        let set = theme_set(
            r##"
            themes {
                theme name="Faerun" variant="light" mode="light" { accent "#111111" }
            }
        "##,
        );
        assert_eq!(
            set.resolve(Some("Faerun"), ThemeVariant::Light)
                .colors
                .accent,
            (17, 17, 17),
            "the user light def shadows the built-in light palette"
        );
        assert_eq!(
            set.resolve(Some("Faerun"), ThemeVariant::Dark).colors,
            ThemeColors::faerun(),
            "the built-in dark base stays"
        );

        let set = theme_set(r##"themes { theme name="Faerun" { accent "#111111" } }"##);
        assert_eq!(
            set.resolve(Some("Faerun"), ThemeVariant::Dark)
                .colors
                .accent,
            (17, 17, 17)
        );
        assert_eq!(
            set.resolve(Some("Faerun"), ThemeVariant::Light).colors.bg,
            ThemeColors::faerun_light().bg,
            "the unshadowed built-in light palette still serves light terminals"
        );
    }

    #[test]
    fn builtin_defs_cover_every_role_for_both_modes() {
        let builtins = ThemeDef::builtins();
        assert_eq!(builtins.len(), 2);
        for def in &builtins {
            assert_eq!(def.colors.len(), THEME_ROLES.len());
        }
        assert_eq!(builtins[0].variant, None);
        assert_eq!(builtins[0].mode, ThemeVariant::Dark);
        assert_eq!(builtins[1].variant.as_deref(), Some("light"));
        assert_eq!(builtins[1].mode, ThemeVariant::Light);
    }
}
