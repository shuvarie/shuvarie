use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::registry;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,

    #[serde(default)]
    pub servers: BTreeMap<String, LspServerSpec>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LspServerSpec {
    #[serde(default)]
    pub command: Vec<String>,

    #[serde(default)]
    pub extensions: Vec<String>,

    #[serde(default = "default_true")]
    pub auto_start: bool,

    #[serde(default)]
    pub root_markers: Vec<String>,
}

impl Default for LspServerSpec {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            extensions: Vec::new(),
            auto_start: true,
            root_markers: Vec::new(),
        }
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            servers: BTreeMap::new(),
        }
    }
}

impl LspConfig {
    pub fn resolve(&self) -> BTreeMap<String, LspServerSpec> {
        registry::merge_overrides(&self.servers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_enabled_with_no_overrides() {
        let cfg = LspConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.servers.is_empty());
    }

    #[test]
    fn resolve_merges_overrides_onto_builtins() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "rust".to_string(),
            LspServerSpec {
                command: vec!["my-ra".into()],
                extensions: Vec::new(),
                auto_start: true,
                root_markers: Vec::new(),
            },
        );
        let cfg = LspConfig {
            enabled: true,
            servers,
        };
        let resolved = cfg.resolve();
        let rust = resolved.get("rust").expect("rust present");
        assert_eq!(rust.command, vec!["my-ra".to_string()]);
        let go = resolved.get("go").expect("builtin go still present");
        assert_eq!(go.command[0], "gopls");
    }

    #[test]
    fn override_can_add_a_new_language() {
        let mut servers = BTreeMap::new();
        servers.insert(
            "ocaml".to_string(),
            LspServerSpec {
                command: vec!["ocaml-lsp-server".into()],
                extensions: vec![".ml".into(), ".mli".into()],
                auto_start: true,
                root_markers: vec!["dune-project".into()],
            },
        );
        let cfg = LspConfig {
            enabled: true,
            servers,
        };
        let resolved = cfg.resolve();
        assert!(resolved.contains_key("ocaml"));
        assert!(resolved.contains_key("rust"));
    }
}
