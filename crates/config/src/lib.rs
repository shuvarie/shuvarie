use std::collections::HashSet;
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
/// section to defaults. `lsp.servers` is the exception: it merges key-by-key
/// so a file adding one server doesn't shadow the rest.
fn merge_layer(config: &mut Config, layer: &ConfigLayer) {
    for section in &layer.sections {
        match section.as_str() {
            "ui" => config.ui = layer.config.ui.clone(),
            "embedding" => config.embedding = layer.config.embedding.clone(),
            "agent" => config.agent = layer.config.agent.clone(),
            "skills" => config.skills = layer.config.skills.clone(),
            "context" => config.context = layer.config.context.clone(),
            "shell" => config.shell = layer.config.shell.clone(),
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
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            frame_rate: 60,
            sidebar: SidebarPref::Auto,
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
    /// `lsp.servers`, which merges key-by-key). With no file present this
    /// returns `Default`. Debug builds read the `-dev` suffixed names
    /// (`shuvarie-dev.kdl`, `.shuvarie-dev`, `~/.config/shuvarie-dev`).
    pub fn load() -> Result<Self> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut paths = Self::local_config_candidates(&cwd).to_vec();
        paths.push(Self::config_path()?);
        Self::load_chain(&paths)
    }

    fn load_chain(paths: &[PathBuf]) -> Result<Self> {
        let mut config = Self::default();
        for path in paths {
            if let Some(layer) = read_layer(path)? {
                merge_layer(&mut config, &layer);
            }
        }
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
}
