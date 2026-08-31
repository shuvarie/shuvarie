use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{config_dir, connections_kdl};
use crate::{CoreError, Result as CoreResult};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connections {
    pub providers: BTreeMap<String, ProviderConfig>,
    pub active_provider: Option<String>,
    pub active_model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderConfig {
    /// The provider id as referenced in the Selune catalog (e.g. `anthropic`,
    /// `openai`, `togetherai`). This is the canonical identity; behavior such
    /// as whether an API key is required comes from the matching
    /// [`selune::Provider`].
    pub kind: String,
    pub(crate) kind_omitted: bool,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Connections {
    pub fn connections_path() -> CoreResult<PathBuf> {
        Ok(config_dir()?.join("connections.kdl"))
    }

    pub fn load() -> CoreResult<Self> {
        let path = Self::connections_path()?;
        Self::load_from(&path)
    }

    pub fn load_from(path: &Path) -> CoreResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(contents) => Ok(connections_kdl::from_kdl(&contents)?),
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
        std::fs::write(path, connections_kdl::to_kdl(self)?)?;
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
            kind_omitted: false,
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
        let parsed: Connections = connections_kdl::from_kdl("").unwrap();
        assert!(parsed.providers.is_empty());
        assert!(parsed.active_provider.is_none());
        assert!(parsed.active_model.is_none());
    }

    #[test]
    fn parses_planned_format() {
        let text = r#"
            active "Ollama Cloud" "glm-5.3-flash"

            providers {
                provider "Ollama Cloud" kind="ollama-cloud" {
                    api-key "<API KEY>"
                    base-url "https://ollama.com"
                }
                provider "Ollama Cloud compat" {
                    api-key "<API KEY>"
                    base-url "https://ollama.com/v1"
                }
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.active_provider.as_deref(), Some("Ollama Cloud"));
        assert_eq!(parsed.active_model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(parsed.providers.len(), 2);
        let cloud = parsed.providers.get("Ollama Cloud").expect("cloud");
        assert_eq!(cloud.kind, "ollama-cloud");
        assert!(!cloud.kind_omitted);
        assert_eq!(cloud.api_key.as_deref(), Some("<API KEY>"));
        assert_eq!(cloud.base_url.as_deref(), Some("https://ollama.com"));
        let compat = parsed.providers.get("Ollama Cloud compat").expect("compat");
        assert_eq!(compat.kind, "openai");
        assert!(compat.kind_omitted);
        assert_eq!(compat.base_url.as_deref(), Some("https://ollama.com/v1"));
    }

    #[test]
    fn providers_layout_round_trip() {
        let text = r#"
            active "acme" "gpt-4o"
            providers {
                provider "acme" kind="openai" {
                    api-key "sk-secret"
                    base-url "https://example.com/v1"
                }
                provider "ollama-local" kind="ollama"
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.providers.len(), 2);
        let acme = parsed.providers.get("acme").expect("acme");
        assert_eq!(acme.kind, "openai");
        assert!(!acme.kind_omitted);
        assert_eq!(acme.api_key.as_deref(), Some("sk-secret"));
        assert_eq!(acme.base_url.as_deref(), Some("https://example.com/v1"));
        let ollama = parsed.providers.get("ollama-local").expect("ollama");
        assert_eq!(ollama.kind, "ollama");
        assert!(ollama.api_key.is_none());
        assert_eq!(parsed.active_provider.as_deref(), Some("acme"));
        assert_eq!(parsed.active_model.as_deref(), Some("gpt-4o"));

        let text = connections_kdl::to_kdl(&parsed).unwrap();
        let reparsed: Connections = connections_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn omitted_kind_stays_omitted_on_save() {
        let text = r#"
            providers {
                provider "compat" {
                    base-url "https://example.com/v1"
                }
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        let compat = parsed.providers.get("compat").expect("compat");
        assert_eq!(compat.kind, "openai");
        assert!(compat.kind_omitted);

        let saved = connections_kdl::to_kdl(&parsed).unwrap();
        assert!(!saved.contains("kind="), "unexpected kind in:\n{saved}");
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn explicit_kind_is_emitted_on_save() {
        let mut connections = Connections::default();
        connections.providers.insert(
            "local".to_string(),
            ProviderConfig::new("ollama", None, None),
        );
        let saved = connections_kdl::to_kdl(&connections).unwrap();
        assert!(
            saved.contains("provider local kind=ollama")
                || saved.contains("provider \"local\" kind=\"ollama\""),
            "expected explicit kind in:\n{saved}"
        );
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(connections, reparsed);
    }

    #[test]
    fn active_without_model_omits_model_slot() {
        let mut connections = Connections::default();
        connections.active_provider = Some("ollama".to_string());
        let text = connections_kdl::to_kdl(&connections).unwrap();
        assert!(
            text.contains("active ollama") || text.contains("active \"ollama\"\n"),
            "expected single-arg active:\n{text}"
        );
        let reparsed: Connections = connections_kdl::from_kdl(&text).unwrap();
        assert_eq!(reparsed.active_provider.as_deref(), Some("ollama"));
        assert!(reparsed.active_model.is_none());
    }

    #[test]
    fn empty_connections_save_has_empty_providers_block() {
        let saved = connections_kdl::to_kdl(&Connections::default()).unwrap();
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(Connections::default(), reparsed);
    }

    #[test]
    fn rejects_wrong_active_arg_count() {
        let err = connections_kdl::from_kdl("active \"a\" \"b\" \"c\"").unwrap_err();
        assert!(err.to_string().contains("at most two"));
    }

    #[test]
    fn rejects_old_format() {
        let err = connections_kdl::from_kdl("active-provider \"acme\"").unwrap_err();
        assert!(err.to_string().contains("legacy"));
    }

    #[test]
    fn rejects_duplicate_active() {
        let text = "active \"a\"\nactive \"b\"";
        let err = connections_kdl::from_kdl(text).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_duplicate_provider() {
        let text = "providers {\n    provider \"a\" kind=\"openai\"\n    provider \"a\" kind=\"ollama\"\n}";
        let err = connections_kdl::from_kdl(text).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn malformed_kdl_has_location() {
        let err = connections_kdl::from_kdl("active ").unwrap_err();
        let CoreError::ConfigParse(e) = err else {
            panic!("expected parse error");
        };
        assert!(e.line >= 1);
        assert!(e.column >= 1);
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
