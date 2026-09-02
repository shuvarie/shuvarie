use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{config_dir, connections_kdl};
use crate::{CoreError, Result as CoreResult};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Active {
    pub provider: String,
    pub model: Option<String>,
    pub variant: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Connections {
    pub providers: BTreeMap<String, ProviderConfig>,
    pub active: Option<Active>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderConfig {
    /// The display name of the provider (e.g. `Ollama Cloud`).
    pub name: String,
    /// The provider id as referenced in the Selune catalog (e.g. `anthropic`,
    /// `openai`, `togetherai`). This is the canonical identity; behavior such
    /// as whether an API key is required comes from the matching
    /// [`selune::Provider`].
    pub kind: String,
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

    /// Whether the active provider (if any) is configured and connectable.
    pub fn has_connected_providers(&self) -> bool {
        if self.providers.is_empty() {
            return false;
        }
        match &self.active {
            None => false,
            Some(active) => match self.providers.get(&active.provider) {
                None => false,
                Some(p) => p.is_connectable(),
            },
        }
    }
}

impl ProviderConfig {
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        api_key: Option<String>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
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
        let parsed: Connections = connections_kdl::from_kdl("").unwrap();
        assert!(parsed.providers.is_empty());
        assert!(parsed.active.is_none());
    }

    #[test]
    fn parses_planned_format() {
        let text = r#"
            active {
                provider "67e55044-10b1-426f-9247-bb680e5fe0c8"
                model "glm-5.3-flash"
                variant "high"
            }

            providers {
                provider id="67e55044-10b1-426f-9247-bb680e5fe0c8" name="Ollama Cloud" {
                    kind "ollama"
                    api-key "<API KEY>"
                    base-url "https://ollama.com"
                }
                provider id="5c1fd0f6-2f0e-4a3a-9b1d-4f2d2f2f2f2f" name="Ollama Cloud compat" {
                    kind "openai"
                    api-key "<API KEY>"
                    base-url "https://ollama.com/v1"
                }
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        let active = parsed.active.as_ref().expect("active");
        assert_eq!(active.provider, "67e55044-10b1-426f-9247-bb680e5fe0c8");
        assert_eq!(active.model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(active.variant.as_deref(), Some("high"));
        assert_eq!(parsed.providers.len(), 2);
        let cloud = parsed
            .providers
            .get("67e55044-10b1-426f-9247-bb680e5fe0c8")
            .expect("cloud");
        assert_eq!(cloud.name, "Ollama Cloud");
        assert_eq!(cloud.kind, "ollama");
        assert_eq!(cloud.api_key.as_deref(), Some("<API KEY>"));
        assert_eq!(cloud.base_url.as_deref(), Some("https://ollama.com"));
        let compat = parsed
            .providers
            .get("5c1fd0f6-2f0e-4a3a-9b1d-4f2d2f2f2f2f")
            .expect("compat");
        assert_eq!(compat.name, "Ollama Cloud compat");
        assert_eq!(compat.kind, "openai");
        assert_eq!(compat.base_url.as_deref(), Some("https://ollama.com/v1"));
    }

    #[test]
    fn providers_layout_round_trip() {
        let text = r#"
            active {
                provider "0d3f7f2a-6c9b-4a5b-8e0a-1b2c3d4e5f60"
                model "gpt-4o"
            }
            providers {
                provider id="0d3f7f2a-6c9b-4a5b-8e0a-1b2c3d4e5f60" name="acme" {
                    kind "openai"
                    api-key "sk-secret"
                    base-url "https://example.com/v1"
                }
                provider id="70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1" name="ollama-local" {
                    kind "ollama"
                }
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        assert_eq!(parsed.providers.len(), 2);
        let acme = parsed
            .providers
            .get("0d3f7f2a-6c9b-4a5b-8e0a-1b2c3d4e5f60")
            .expect("acme");
        assert_eq!(acme.name, "acme");
        assert_eq!(acme.kind, "openai");
        assert_eq!(acme.api_key.as_deref(), Some("sk-secret"));
        assert_eq!(acme.base_url.as_deref(), Some("https://example.com/v1"));
        let ollama = parsed
            .providers
            .get("70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1")
            .expect("ollama");
        assert_eq!(ollama.kind, "ollama");
        assert!(ollama.api_key.is_none());
        let active = parsed.active.as_ref().expect("active");
        assert_eq!(active.provider, "0d3f7f2a-6c9b-4a5b-8e0a-1b2c3d4e5f60");
        assert_eq!(active.model.as_deref(), Some("gpt-4o"));
        assert!(active.variant.is_none());

        let text = connections_kdl::to_kdl(&parsed).unwrap();
        let reparsed: Connections = connections_kdl::from_kdl(&text).unwrap();
        assert_eq!(parsed, reparsed);
    }

    #[test]
    fn full_active_round_trip() {
        let mut connections = Connections::default();
        connections.active = Some(Active {
            provider: "6f9619ff-8b86-d011-b42d-00cf4fc964ff".into(),
            model: Some("glm-5.3-flash".into()),
            variant: Some("high".into()),
        });
        let saved = connections_kdl::to_kdl(&connections).unwrap();
        assert!(
            saved.contains("variant high") || saved.contains("variant \"high\""),
            "expected variant in:\n{saved}"
        );
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(connections, reparsed);
    }

    #[test]
    fn parses_user_format() {
        let text = r#"
            active {
                provider "67e55044-10b1-426f-9247-bb680e5fe0c8"
                model "glm-5.3-flash"
                variant "high"
            }

            providers {
                provider id="67e55044-10b1-426f-9247-bb680e5fe0c8" name="Ollama Cloud" {
                    kind "ollama"
                    api-key "<API KEY>"
                    base-url "https://ollama.com"
                }
            }
        "#;
        let parsed: Connections = connections_kdl::from_kdl(text).unwrap();
        let active = parsed.active.as_ref().expect("active");
        assert_eq!(active.provider, "67e55044-10b1-426f-9247-bb680e5fe0c8");
        assert_eq!(active.model.as_deref(), Some("glm-5.3-flash"));
        assert_eq!(active.variant.as_deref(), Some("high"));
        let cloud = parsed
            .providers
            .get("67e55044-10b1-426f-9247-bb680e5fe0c8")
            .expect("cloud");
        assert_eq!(cloud.name, "Ollama Cloud");
        assert_eq!(cloud.kind, "ollama");
        assert_eq!(cloud.api_key.as_deref(), Some("<API KEY>"));
        assert_eq!(cloud.base_url.as_deref(), Some("https://ollama.com"));
    }

    #[test]
    fn requires_kind_on_save() {
        let mut connections = Connections::default();
        connections.providers.insert(
            "local".to_string(),
            ProviderConfig::new("local", "ollama", None, None),
        );
        let saved = connections_kdl::to_kdl(&connections).unwrap();
        assert!(saved.contains("kind"), "expected kind child in:\n{saved}");
        let missing_kind = "providers {\n    provider id=\"x\" name=\"x\" { }\n}";
        let err = connections_kdl::from_kdl(missing_kind).unwrap_err();
        assert!(err.to_string().contains("`kind`"));
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(connections, reparsed);
    }

    #[test]
    fn active_without_children_is_error() {
        let err = connections_kdl::from_kdl("active").unwrap_err();
        assert!(err.to_string().contains("block"));
    }

    #[test]
    fn active_without_provider_child_is_error() {
        let text = "active {\n    model \"m\"\n}";
        let err = connections_kdl::from_kdl(text).unwrap_err();
        assert!(err.to_string().contains("requires a `provider` child"));
    }

    #[test]
    fn rejects_positional_active() {
        let err = connections_kdl::from_kdl("active \"ollama\" \"llama3\"").unwrap_err();
        assert!(err.to_string().contains("requires a block"));
    }

    #[test]
    fn rejects_duplicate_children_in_active() {
        for text in [
            "active {\n    provider \"a\"\n    provider \"b\"\n}",
            "active {\n    provider \"a\"\n    model \"m\"\n    model \"m2\"\n}",
            "active {\n    provider \"a\"\n    variant \"v\"\n    variant \"high\"\n}",
        ] {
            let err = connections_kdl::from_kdl(text).unwrap_err();
            assert!(err.to_string().contains("duplicate"), "{text}");
        }
    }

    #[test]
    fn empty_connections_save_has_empty_providers_block() {
        let saved = connections_kdl::to_kdl(&Connections::default()).unwrap();
        let reparsed: Connections = connections_kdl::from_kdl(&saved).unwrap();
        assert_eq!(Connections::default(), reparsed);
    }

    #[test]
    fn rejects_non_string_active_children() {
        for text in [
            "active {\n    provider 1\n}",
            "active {\n    provider \"a\"\n    model #true\n}",
        ] {
            let err = connections_kdl::from_kdl(text).unwrap_err();
            assert!(err.to_string().contains("must be a string"), "{text}");
        }
    }

    #[test]
    fn rejects_old_format() {
        let err = connections_kdl::from_kdl("active-provider \"acme\"").unwrap_err();
        assert!(err.to_string().contains("legacy"));
    }

    #[test]
    fn rejects_duplicate_active() {
        let text = "active {\n    provider \"a\"\n}\nactive {\n    provider \"b\"\n}";
        let err = connections_kdl::from_kdl(text).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn rejects_duplicate_provider() {
        let text = "providers {\n    provider id=\"a\" name=\"a\" { kind \"openai\" }\n    provider id=\"a\" name=\"b\" { kind \"ollama\" }\n}";
        let err = connections_kdl::from_kdl(text).unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn malformed_kdl_has_location() {
        let err = connections_kdl::from_kdl("active {").unwrap_err();
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
            "70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1".to_string(),
            ProviderConfig::new("ollama-local", "ollama", None, None),
        );
        connections.active = Some(Active {
            provider: "70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1".to_string(),
            model: Some("llama3".to_string()),
            variant: None,
        });
        connections.save_to(&path).unwrap();
        let loaded = Connections::load_from(&path).unwrap();
        assert_eq!(connections, loaded);
    }
}
