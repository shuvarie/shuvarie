use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use shuvarie_llm::Provider;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_config() -> Config {
        let mut providers = HashMap::new();
        providers.insert(
            "my-openai".to_string(),
            ProviderConfig::new(
                Provider::OpenAiCompatible,
                Some("sk-test".to_string()),
                None,
            ),
        );
        providers.insert(
            "local-ollama".to_string(),
            ProviderConfig::new(
                Provider::Ollama,
                None,
                Some("http://localhost:11434".into()),
            ),
        );
        Config {
            providers,
            active_provider: Some("my-openai".to_string()),
            active_model: Some("gpt-5.5".to_string()),
            ui: UiPrefs::default(),
        }
    }

    #[test]
    fn round_trip_serialization() {
        let config = sample_config();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        let parsed: Config = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(config, parsed);
    }

    #[test]
    fn default_config_round_trip() {
        let config = Config::default();
        let toml_str = toml::to_string_pretty(&config).expect("serialize");
        let parsed: Config = toml::from_str(&toml_str).expect("deserialize");
        assert_eq!(config, parsed);
    }

    #[test]
    fn empty_config_has_no_connected_providers() {
        let config = Config::default();
        assert!(!config.has_connected_providers());
    }

    #[test]
    fn missing_active_provider_has_no_connected_providers() {
        let config = Config {
            providers: HashMap::from([(
                "my-openai".to_string(),
                ProviderConfig::new(
                    Provider::OpenAiCompatible,
                    Some("sk-test".to_string()),
                    None,
                ),
            )]),
            active_provider: None,
            active_model: None,
            ui: UiPrefs::default(),
        };
        assert!(!config.has_connected_providers());
    }

    #[test]
    fn active_provider_missing_from_map_has_no_connected_providers() {
        let config = Config {
            providers: HashMap::new(),
            active_provider: Some("nonexistent".to_string()),
            active_model: None,
            ui: UiPrefs::default(),
        };
        assert!(!config.has_connected_providers());
    }

    #[test]
    fn active_provider_without_key_has_no_connected_providers() {
        let config = Config {
            providers: HashMap::from([(
                "my-openai".to_string(),
                ProviderConfig::new(Provider::OpenAiCompatible, None, None),
            )]),
            active_provider: Some("my-openai".to_string()),
            active_model: None,
            ui: UiPrefs::default(),
        };
        assert!(!config.has_connected_providers());
    }

    #[test]
    fn active_provider_with_key_is_connected() {
        let config = sample_config();
        assert!(config.has_connected_providers());
    }

    #[test]
    fn ollama_without_key_is_connected() {
        let config = Config {
            providers: HashMap::from([(
                "local".to_string(),
                ProviderConfig::new(Provider::Ollama, None, None),
            )]),
            active_provider: Some("local".to_string()),
            active_model: None,
            ui: UiPrefs::default(),
        };
        assert!(config.has_connected_providers());
    }

    #[test]
    fn save_and_load_from_temp_dir() {
        let dir = std::env::temp_dir().join(format!(
            "shuvarie-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join(CONFIG_DIR_NAME).join(CONFIG_FILE_NAME);

        let config = sample_config();
        config.save_to(&path).expect("save");

        let loaded = Config::load_from(&path).expect("load");
        assert_eq!(config, loaded);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_from_missing_file_returns_default() {
        let path = std::env::temp_dir().join("shuvarie-nonexistent-config.toml");
        let config = Config::load_from(&path).expect("load");
        assert_eq!(config, Config::default());
    }
}
