use std::collections::HashMap;
use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use serde::{Deserialize, Serialize};

use crate::config::config_dir;
use crate::kdlfmt::{self, NodeNamed, finish, push_child, scalar};
use crate::{CoreError, Result as CoreResult};

const CONNECTIONS_FILE_NAME: &str = "connections.kdl";

fn first_string(node: &kdl::KdlNode) -> Option<String> {
    let entry = node.entries().first()?;
    entry.value().as_string().map(str::to_string)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct Connections {
    pub providers: HashMap<String, ProviderConfig>,

    pub active_provider: Option<String>,

    pub active_model: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", default)]
pub struct ProviderConfig {
    /// The provider id as referenced in the Selune catalog (e.g. `anthropic`,
    /// `openai`, `togetherai`). This is the canonical identity; behavior such
    /// as whether an API key is required comes from the matching
    /// [`selune::Provider`].
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
            Ok(contents) => Ok(Self::from_kdl(&contents)?),
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
        std::fs::write(path, self.to_kdl())?;
        Ok(())
    }

    fn from_kdl(contents: &str) -> CoreResult<Self> {
        let doc = kdlfmt::parse(contents)?;
        let mut connections = Self::default();

        for node in doc.nodes() {
            match node.name().value() {
                "providers" => {
                    for provider in node.children().map(|c| c.nodes()).unwrap_or_default() {
                        if provider.name().value() != "provider" {
                            continue;
                        }
                        let Some(name) = provider.entries().first().and_then(|e| match e.value() {
                            KdlValue::String(s) => Some(s.to_string()),
                            KdlValue::Integer(i) => Some(i.to_string()),
                            _ => None,
                        }) else {
                            continue;
                        };
                        let provider_view = provider.view();
                        let config = ProviderConfig {
                            kind: provider_view.string("kind").unwrap_or_default(),
                            api_key: provider_view.string("api-key"),
                            base_url: provider_view.string("base-url"),
                        };
                        connections.providers.insert(name, config);
                    }
                }
                "active-provider" => {
                    connections.active_provider = first_string(node);
                }
                "active-model" => {
                    connections.active_model = first_string(node);
                }
                _ => {}
            }
        }

        Ok(connections)
    }

    fn to_kdl(&self) -> String {
        let mut doc = KdlDocument::new();

        let mut providers = KdlNode::new("providers");
        let mut names: Vec<&String> = self.providers.keys().collect();
        names.sort();
        for name in names {
            let provider = &self.providers[name];
            let mut node = KdlNode::new("provider");
            node.entries_mut().push(KdlEntry::new(name.as_str()));
            push_child(&mut node, scalar("kind", provider.kind.as_str()));
            if let Some(api_key) = &provider.api_key {
                push_child(&mut node, scalar("api-key", api_key.as_str()));
            }
            if let Some(base_url) = &provider.base_url {
                push_child(&mut node, scalar("base-url", base_url.as_str()));
            }
            push_child(&mut providers, node);
        }
        doc.nodes_mut().push(providers);

        if let Some(active_provider) = &self.active_provider {
            doc.nodes_mut()
                .push(scalar("active-provider", active_provider.as_str()));
        }
        if let Some(active_model) = &self.active_model {
            doc.nodes_mut()
                .push(scalar("active-model", active_model.as_str()));
        }

        finish(doc)
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
