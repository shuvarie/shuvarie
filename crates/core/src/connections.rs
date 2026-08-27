use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::{CoreError, Result};

const CONNECTIONS_FILE_NAME: &str = "connections.toml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Connections {
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,

    #[serde(default)]
    pub active_provider: Option<String>,

    #[serde(default)]
    pub active_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    /// The provider id as referenced in the Selune catalog (e.g. `anthropic`,
    /// `openai`, `togetherai`). This is the canonical identity; behavior such
    /// as whether an API key is required comes from the matching
    /// [`selune::Provider`].
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Connections {
    pub fn connections_path() -> Result<PathBuf> {
        Ok(config_dir()?.join(CONNECTIONS_FILE_NAME))
    }

    pub fn load() -> Result<Self> {
        let path = Self::connections_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => {
                let connections: Connections = toml::from_str(&contents)?;
                Ok(connections)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::ConfigIo(e)),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::connections_path()?;
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

    /// Whether the active provider is configured and connectable.
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
    pub fn new(kind: impl Into<String>, api_key: Option<String>, base_url: Option<String>) -> Self {
        Self {
            kind: kind.into(),
            api_key,
            base_url,
        }
    }

    /// A provider is connectable when it declares no API key requirement, or
    /// when a non-empty key is present. Provider-level key requirements come
    /// from the Selune catalog; here we treat the absence of a catalog entry as
    /// requiring a key only when the provider id looks remote.
    pub fn is_connectable(&self) -> bool {
        let catalog = crate::catalog::providers();
        match catalog.iter().find(|p| p.id.0 == self.kind) {
            Some(p) => match &p.api_key {
                Some(_) => self
                    .api_key
                    .as_ref()
                    .map(|k| !k.trim().is_empty())
                    .unwrap_or(false),
                None => true,
            },
            // Unknown provider id: fall back to requiring a key unless it's an
            // Ollama-style local provider.
            None => {
                if self.kind == "ollama" {
                    true
                } else {
                    self.api_key
                        .as_ref()
                        .map(|k| !k.trim().is_empty())
                        .unwrap_or(false)
                }
            }
        }
    }
}
