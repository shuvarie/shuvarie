use std::collections::BTreeMap;

use shuvarie_config::{Active, Connections, LspServerSpecRepr, ProviderConfig};
use shuvarie_core::catalog::has_connected_providers;
use shuvarie_core::lsp_manager::lsp_config_from_repr;

fn sample_connections() -> Connections {
    let mut providers = BTreeMap::new();
    providers.insert(
        "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
        ProviderConfig::new("my-openai", "openai", Some("sk-test".to_string()), None),
    );
    providers.insert(
        "70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1".to_string(),
        ProviderConfig::new(
            "local-ollama",
            "ollama",
            None,
            Some("http://localhost:11434".into()),
        ),
    );
    Connections {
        providers,
        active: Some(Active {
            provider: "67e55044-10b1-426f-9247-bb680e5fe0c8".to_string(),
            model: Some("gpt-5.5".to_string()),
            variant: None,
        }),
    }
}

#[test]
fn empty_connections_has_no_connected_providers() {
    let connections = Connections::default();
    assert!(!has_connected_providers(&connections));
}

#[test]
fn missing_active_provider_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "my-openai".to_string(),
            ProviderConfig::new("my-openai", "openai", Some("sk-test".to_string()), None),
        )]),
        active: None,
    };
    assert!(!has_connected_providers(&connections));
}

#[test]
fn active_provider_missing_from_map_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::new(),
        active: Some(Active {
            provider: "nonexistent".to_string(),
            model: None,
            variant: None,
        }),
    };
    assert!(!has_connected_providers(&connections));
}

#[test]
fn active_provider_without_key_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "my-openai".to_string(),
            ProviderConfig::new("my-openai", "openai", None, None),
        )]),
        active: Some(Active {
            provider: "my-openai".to_string(),
            model: None,
            variant: None,
        }),
    };
    assert!(!has_connected_providers(&connections));
}

#[test]
fn active_provider_with_key_is_connected() {
    let connections = sample_connections();
    assert!(has_connected_providers(&connections));
}

#[test]
fn ollama_without_key_is_connected() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "local".to_string(),
            ProviderConfig::new("local", "ollama", None, None),
        )]),
        active: Some(Active {
            provider: "local".to_string(),
            model: None,
            variant: None,
        }),
    };
    assert!(has_connected_providers(&connections));
}

#[test]
fn ollama_cloud_without_key_is_not_connected() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "cloud".to_string(),
            ProviderConfig::new("cloud", "ollama-cloud", None, None),
        )]),
        active: Some(Active {
            provider: "cloud".to_string(),
            model: None,
            variant: None,
        }),
    };
    assert!(!has_connected_providers(&connections));
}

#[test]
fn ollama_cloud_with_key_is_connected() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "cloud".to_string(),
            ProviderConfig::new("cloud", "ollama", Some("ollama-key".to_string()), None),
        )]),
        active: Some(Active {
            provider: "cloud".to_string(),
            model: None,
            variant: None,
        }),
    };
    assert!(has_connected_providers(&connections));
}

#[test]
fn lsp_repr_converts_to_lsp_config() {
    let mut config = shuvarie_config::Config::default();
    config.lsp.servers.insert(
        "go".to_string(),
        LspServerSpecRepr {
            command: vec!["gopls".to_string()],
            extensions: vec!["go".to_string()],
            no_auto_start: true,
            root_markers: vec!["go.mod".to_string()],
        },
    );
    let lsp_config = lsp_config_from_repr(&config.lsp);
    assert!(lsp_config.enabled);
    let resolved = lsp_config.resolve();
    let go = resolved.get("go").expect("go resolved");
    assert_eq!(go.command, vec!["gopls".to_string()]);
    assert!(!go.auto_start);
    // builtin rust override still merges
    let rust = resolved.get("rust").expect("rust builtin");
    assert_eq!(rust.command, vec!["rust-analyzer".to_string()]);
}
