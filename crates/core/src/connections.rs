use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::kdlserde;
use crate::{CoreError, Result as CoreResult};

const CONNECTIONS_FILE_NAME: &str = "connections.kdl";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Connections {
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct ProviderConfig {
    /// The provider id as referenced in the Selune catalog (e.g. `anthropic`,
    /// `openai`, `togetherai`). This is the canonical identity; behavior such
    /// as whether an API key is required comes from the matching
    /// [`selune::Provider`].
    #[serde(default, deserialize_with = "kdlserde::de_default")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Connections {
    pub fn connections_path() -> CoreResult<PathBuf> {
        Ok(config_dir()?.join(CONNECTIONS_FILE_NAME))
    }

    pub fn load() -> CoreResult<Self> {
        let path = Self::connections_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> CoreResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(kdlserde::from_str(&contents)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(CoreError::ConfigIo(e)),
        }
    }

    pub fn save(&self) -> CoreResult<()> {
        let path = Self::connections_path()?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, kdlserde::to_string(self)?)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_defaults() {
        let parsed: Connections = kdlserde::from_str("").unwrap();
        assert!(parsed.providers.is_empty());
        assert!(parsed.active_provider.is_none());
    }

    #[test]
    fn providers_layout_round_trip() {
        let text = r#"
            providers {
                acme {
                    kind "openai"
                    api-key "sk-secret"
                    base-url "https://example.com/v1"
                }
                ollama-local {
                    kind "ollama"
                }
            }
            active-provider "acme"
            active-model "gpt-4o"
        "#;
        let parsed: Connections = kdlserde::from_str(text).unwrap();
        assert_eq!(parsed.providers.len(), 2);
        let acme = parsed.providers.get("acme").expect("acme");
        assert_eq!(acme.kind, "openai");
        assert_eq!(acme.api_key.as_deref(), Some("sk-secret"));
        assert_eq!(acme.base_url.as_deref(), Some("https://example.com/v1"));
        let ollama = parsed.providers.get("ollama-local").expect("ollama");
        assert_eq!(ollama.kind, "ollama");
        assert!(ollama.api_key.is_none());
        assert_eq!(parsed.active_provider.as_deref(), Some("acme"));
        assert_eq!(parsed.active_model.as_deref(), Some("gpt-4o"));

        let text = kdlserde::to_string(&parsed).unwrap();
        let reparsed: Connections = kdlserde::from_str(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn save_and_reload_via_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.kdl");
        let mut connections = Connections::default();
        connections.providers.insert(
            "ollama".to_string(),
            ProviderConfig::new("ollama", None, None),
        );
        connections.active_provider = Some("ollama".to_string());
        connections.active_model = Some("llama3".to_string());
        connections.save_to(&path).unwrap();
        let loaded = Connections::load_from(&path).unwrap();
        assert_eq!(connections, loaded);
    }
}
