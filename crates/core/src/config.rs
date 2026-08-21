use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use shuvarie_catalog::Provider;

use crate::{CoreError, Result};

const CONFIG_DIR_NAME: &str = "shuvarie";
const CONFIG_FILE_NAME: &str = "config.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Config {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    #[serde(default)]
    pub active_provider: Option<String>,

    #[serde(default)]
    pub active_model: Option<String>,

    #[serde(default)]
    pub ui: UiPrefs,

    #[serde(default)]
    pub embedding: EmbeddingConfig,
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
pub struct ProviderConfig {
    pub kind: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UiPrefs {}

impl Config {
    pub fn config_path() -> Result<PathBuf> {
        let dir = dirs::config_dir().ok_or_else(|| {
            CoreError::ConfigIo(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no config directory for this platform",
            ))
        })?;
        Ok(dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME))
    }

    pub fn load() -> Result<Self> {
        let path = Self::config_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
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

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self)?;
        std::fs::write(path, contents)?;
        Ok(())
    }

    pub fn has_connected_providers(&self) -> bool {
        if self.providers.is_empty() {
            return false;
        }
        match &self.active_provider {
            None => false,
            Some(name) => match self.providers.get(name) {
                None => false,
                Some(p) => p.is_connectable(),
            },
        }
    }
}

impl ProviderConfig {
    pub fn new(kind: Provider, api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            kind,
            api_key,
            base_url,
        }
    }

    pub fn is_connectable(&self) -> bool {
        if self.kind.requires_api_key() {
            self.api_key
                .as_ref()
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false)
        } else {
            true
        }
    }
}
