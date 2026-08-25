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

    #[serde(default)]
    pub agent: AgentConfig,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UiPrefs {}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentConfig {
    #[serde(default)]
    pub max_turns: usize,

    #[serde(default)]
    pub worker_max_turns: usize,
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
