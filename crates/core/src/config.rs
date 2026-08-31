use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

mod connections;
mod connections_kdl;
mod kdlserde;

pub use self::connections::{Active, Connections, ProviderConfig};
pub(crate) use self::kdlserde::span_to_line_column;

const CONFIG_DIR_NAME: &str = "shuvarie";
const CONFIG_FILE_NAME: &str = "config.kdl";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    #[serde(default)]
    pub ui: UiPrefs,

    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub lsp: LspConfigRepr,

    #[serde(default)]
    pub skills: SkillsConfig,

    #[serde(default)]
    pub context: ContextConfig,
}

fn default_max_turns() -> usize {
    0
}

/// Context-window management: bounds the input tokens sent to the LLM. One
/// forecast (anchored on the last call's real request size) drives three
/// layers — the per-call mechanical trim, the stop-before-call overflow
/// guard, and the pre-send LLM compaction — all sharing this budget. See
/// `docs/design/context-compaction.md` for the pipeline design.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ContextConfig {
    /// Stored inverted in the file as `disabled`; defaults to enabled.
    #[serde(deserialize_with = "kdlserde::de_default")]
    pub disabled: bool,

    /// Tokens reserved for the model's reply and a safety buffer. The input
    /// budget is `context_length - reserved`.
    #[serde(deserialize_with = "kdlserde::de_reserved")]
    pub reserved: u64,

    /// Tokens kept verbatim as the "tail" when trimming older messages (both
    /// the in-run hook trim and compaction's cut point use this budget).
    #[serde(deserialize_with = "kdlserde::de_keep_recent_tokens")]
    pub keep_recent_tokens: u64,

    /// Maximum chars of a tool result's text sent to the model. Larger outputs
    /// are truncated with a marker hinting the model to read ranges. `0`
    /// disables the cap.
    #[serde(deserialize_with = "kdlserde::de_tool_output_max_chars")]
    pub tool_output_max_chars: usize,

    /// Default context length used when the catalog has no entry for the
    /// active model.
    #[serde(deserialize_with = "kdlserde::de_fallback_context")]
    pub fallback_context_length: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            disabled: false,
            reserved: 20_000,
            keep_recent_tokens: 20_000,
            tool_output_max_chars: 16_000,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct SkillsConfig {
    #[serde(deserialize_with = "kdlserde::de_default")]
    pub disabled: bool,
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct EmbeddingConfig {
    #[serde(deserialize_with = "kdlserde::de_default")]
    pub disabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct UiPrefs {
    /// Target frames per second for the TUI render loop. `0` disables the cap
    /// (one draw per event, the original behavior). Defaults to 60.
    #[serde(deserialize_with = "kdlserde::de_frame_rate")]
    pub frame_rate: u32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self { frame_rate: 60 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct AgentConfig {
    /// `0` = unlimited.
    #[serde(
        rename = "max-turns",
        default,
        deserialize_with = "kdlserde::de_default",
        skip_serializing_if = "is_zero_usize"
    )]
    pub max_turns: usize,

    /// `0` = unlimited.
    #[serde(
        rename = "worker-max-turns",
        default,
        deserialize_with = "kdlserde::de_default",
        skip_serializing_if = "is_zero_usize"
    )]
    pub worker_max_turns: usize,
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
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
    if value == 0 {
        usize::MAX
    } else {
        value
    }
}

/// Serde mirror of [`shuvarie_lsp::LspConfig`] for the KDL file layout; the
/// `shuvarie-lsp` crate itself stays config-format-free.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct LspConfigRepr {
    #[serde(deserialize_with = "kdlserde::de_default")]
    pub disabled: bool,
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub servers: BTreeRepr,
}

type BTreeRepr = std::collections::BTreeMap<String, LspServerSpecRepr>;

impl From<&shuvarie_lsp::LspConfig> for LspConfigRepr {
    fn from(cfg: &shuvarie_lsp::LspConfig) -> Self {
        Self {
            disabled: !cfg.enabled,
            servers: cfg
                .servers
                .iter()
                .map(|(name, spec)| (name.clone(), LspServerSpecRepr::from(spec)))
                .collect(),
        }
    }
}

impl From<&LspConfigRepr> for shuvarie_lsp::LspConfig {
    fn from(repr: &LspConfigRepr) -> Self {
        Self {
            enabled: !repr.disabled,
            servers: repr
                .servers
                .iter()
                .map(|(name, spec)| (name.clone(), shuvarie_lsp::LspServerSpec::from(spec)))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct LspServerSpecRepr {
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub command: Vec<String>,
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub extensions: Vec<String>,
    #[serde(deserialize_with = "kdlserde::de_default")]
    pub no_auto_start: bool,
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub root_markers: Vec<String>,
}

impl From<&shuvarie_lsp::LspServerSpec> for LspServerSpecRepr {
    fn from(spec: &shuvarie_lsp::LspServerSpec) -> Self {
        Self {
            command: spec.command.clone(),
            extensions: spec.extensions.clone(),
            no_auto_start: !spec.auto_start,
            root_markers: spec.root_markers.clone(),
        }
    }
}

impl From<&LspServerSpecRepr> for shuvarie_lsp::LspServerSpec {
    fn from(repr: &LspServerSpecRepr) -> Self {
        Self {
            command: repr.command.clone(),
            extensions: repr.extensions.clone(),
            auto_start: !repr.no_auto_start,
            root_markers: repr.root_markers.clone(),
        }
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir().ok_or_else(|| {
        CoreError::ConfigIo(std::io::Error::new(
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

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(kdlserde::from_str(&contents)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::ConfigIo(e)),
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
        std::fs::write(path, kdlserde::to_string(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let config = Config::default();
        let text = kdlserde::to_string(&config).unwrap();
        let parsed: Config = kdlserde::from_str(&text).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn empty_file_is_all_defaults() {
        let parsed: Config = kdlserde::from_str("").unwrap();
        assert_eq!(parsed, Config::default());
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
        let parsed: Config = kdlserde::from_str(text).unwrap();
        assert_eq!(parsed.ui.frame_rate, 30);
        assert!(parsed.skills.disabled);
        assert_eq!(parsed.skills.dirs, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parsed.embedding, EmbeddingConfig::default());
        assert_eq!(parsed.context, ContextConfig::default());
        assert_eq!(parsed.lsp, LspConfigRepr::default());
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
        let parsed: Config = kdlserde::from_str(text).unwrap();
        assert!(parsed.embedding.disabled);
        assert_eq!(parsed.embedding.provider.as_deref(), Some("openai"));
        assert_eq!(parsed.embedding.dimensions, Some(1536));
        assert!(parsed.context.disabled);
        assert_eq!(parsed.context.reserved, 5000);
        assert_eq!(parsed.context.keep_recent_tokens, 10_000);
        assert_eq!(parsed.context.tool_output_max_chars, 1000);
        assert_eq!(parsed.context.fallback_context_length, 64_000);

        let text = kdlserde::to_string(&parsed).unwrap();
        let reparsed: Config = kdlserde::from_str(&text).unwrap();
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
        let parsed: Config = kdlserde::from_str(text).unwrap();
        assert!(parsed.lsp.disabled);
        let rust = parsed.lsp.servers.get("rust").expect("rust server");
        assert_eq!(rust.command, vec!["rust-analyzer".to_string()]);
        assert_eq!(rust.extensions, vec![".rs".to_string()]);
        assert!(rust.no_auto_start);
        assert_eq!(rust.root_markers, vec!["Cargo.toml".to_string()]);
        let zig = parsed.lsp.servers.get("zig").expect("zig server");
        assert_eq!(zig.command, vec!["zls".to_string()]);
        assert!(!zig.no_auto_start);

        let text = kdlserde::to_string(&parsed).unwrap();
        let reparsed: Config = kdlserde::from_str(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn lsp_mirror_converts() {
        let repr = LspConfigRepr {
            disabled: true,
            servers: [(
                "go".to_string(),
                LspServerSpecRepr {
                    command: vec!["gopls".to_string()],
                    extensions: vec![".go".to_string()],
                    no_auto_start: true,
                    root_markers: vec!["go.mod".to_string()],
                },
            )]
            .into_iter()
            .collect(),
        };
        let lsp: shuvarie_lsp::LspConfig = (&repr).into();
        assert!(!lsp.enabled);
        assert_eq!(lsp.resolve().get("go").expect("go").command[0], "gopls");
        let back = LspConfigRepr::from(&lsp);
        assert_eq!(repr, back);
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
        let parsed: Config = kdlserde::from_str(text).unwrap();
        assert_eq!(parsed.ui.frame_rate, 30);
        assert_eq!(parsed.agent, AgentConfig::default());
    }

    #[test]
    fn type_error_surfaced_with_location() {
        let text = "ui {\n    frame-rate \"sixty\"\n}";
        let err = kdlserde::from_str::<Config>(text).unwrap_err();
        let CoreError::ConfigParse(parse_err) = err else {
            panic!("expected config parse error");
        };
        assert_eq!(parse_err.line, 2);
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
}
