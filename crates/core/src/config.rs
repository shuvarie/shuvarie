use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

const CONFIG_DIR_NAME: &str = "shuvarie";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub ui: UiPrefs,

    #[serde(default)]
    pub embedding: EmbeddingConfig,

    #[serde(default = "default_agent_config")]
    pub agent: AgentConfig,

    #[serde(default)]
    pub lsp: shuvarie_lsp::LspConfig,

    #[serde(default)]
    pub skills: SkillsConfig,

    #[serde(default)]
    pub context: ContextConfig,
}

fn default_agent_config() -> AgentConfig {
    AgentConfig {
        max_turns: default_max_turns(),
        worker_max_turns: 0,
    }
}

fn default_max_turns() -> usize {
    30
}

/// Context-window management: bounds the input tokens sent to the LLM per model
/// call. Modeled on OpenCode's compaction/preserve approach but without an LLM
/// summarizer — older tool results are dropped (replaced with a short marker)
/// once the estimated request size exceeds the budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextConfig {
    /// Enable the history-budget hook that trims old tool results per model
    /// call when the estimated request size exceeds the budget.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Tokens reserved for the model's reply and a safety buffer. The input
    /// budget is `context_length - reserved`.
    #[serde(default = "default_reserved")]
    pub reserved: u64,

    /// Maximum chars of a tool result's text sent to the model. Larger outputs
    /// are truncated with a marker hinting the model to read ranges. `0`
    /// disables the cap.
    #[serde(default = "default_tool_output_max_chars")]
    pub tool_output_max_chars: usize,

    /// Default context length used when the catalog has no entry for the
    /// active model.
    #[serde(default = "default_fallback_context")]
    pub fallback_context_length: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            reserved: default_reserved(),
            tool_output_max_chars: default_tool_output_max_chars(),
            fallback_context_length: default_fallback_context(),
        }
    }
}

fn default_reserved() -> u64 {
    20_000
}

fn default_tool_output_max_chars() -> usize {
    16_000
}

fn default_fallback_context() -> u64 {
    128_000
}

impl ContextConfig {
    /// Tokens kept verbatim as the "tail" during per-call history trimming.
    /// 25% of the usable budget, clamped to [2_000, 15_000] tokens (estimated
    /// at ~4 chars/token).
    pub fn preserve_recent_tokens(&self, context_length: u64) -> u64 {
        let usable = context_length.saturating_sub(self.reserved);
        let pct = usable / 4;
        pct.clamp(2_000, 15_000)
    }

    /// Usable input-token budget for the given model context length.
    pub fn usable(&self, context_length: u64) -> u64 {
        context_length.saturating_sub(self.reserved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkillsConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub dirs: Vec<String>,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<u32>,
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: None,
            model: None,
            dimensions: None,
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UiPrefs {
    /// Target frames per second for the TUI render loop. `0` disables the cap
    /// (one draw per event, the original behavior). Defaults to 60.
    #[serde(default = "default_frame_rate")]
    pub frame_rate: u32,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            frame_rate: default_frame_rate(),
        }
    }
}

fn default_frame_rate() -> u32 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,

    #[serde(default)]
    pub worker_max_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        default_agent_config()
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
            Ok(contents) => {
                let config: Config = toml::from_str(&contents)?;
                Ok(config)
            }
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
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }
}
