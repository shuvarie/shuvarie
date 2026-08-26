use std::collections::HashMap;

use shuvarie_catalog::Provider;
use shuvarie_core::{Config, Connections, ProviderConfig};

fn sample_connections() -> Connections {
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
    Connections {
        providers,
        active_provider: Some("my-openai".to_string()),
        active_model: Some("gpt-5.5".to_string()),
    }
}

#[test]
fn round_trip_serialization() {
    let connections = sample_connections();
    let toml_str = toml::to_string_pretty(&connections).expect("serialize");
    let parsed: Connections = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(connections, parsed);
}

#[test]
fn default_connections_round_trip() {
    let connections = Connections::default();
    let toml_str = toml::to_string_pretty(&connections).expect("serialize");
    let parsed: Connections = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(connections, parsed);
}

#[test]
fn default_config_round_trip() {
    let config = Config::default();
    let toml_str = toml::to_string_pretty(&config).expect("serialize");
    let parsed: Config = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(config, parsed);
}

#[test]
fn empty_connections_has_no_connected_providers() {
    let connections = Connections::default();
    assert!(!connections.has_connected_providers());
}

#[test]
fn missing_active_provider_has_no_connected_providers() {
    let connections = Connections {
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
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn active_provider_missing_from_map_has_no_connected_providers() {
    let connections = Connections {
        providers: HashMap::new(),
        active_provider: Some("nonexistent".to_string()),
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn active_provider_without_key_has_no_connected_providers() {
    let connections = Connections {
        providers: HashMap::from([(
            "my-openai".to_string(),
            ProviderConfig::new(Provider::OpenAiCompatible, None, None),
        )]),
        active_provider: Some("my-openai".to_string()),
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn active_provider_with_key_is_connected() {
    let connections = sample_connections();
    assert!(connections.has_connected_providers());
}

#[test]
fn ollama_without_key_is_connected() {
    let connections = Connections {
        providers: HashMap::from([(
            "local".to_string(),
            ProviderConfig::new(Provider::Ollama, None, None),
        )]),
        active_provider: Some("local".to_string()),
        active_model: None,
    };
    assert!(connections.has_connected_providers());
}

#[test]
fn ollama_cloud_without_key_is_not_connected() {
    let connections = Connections {
        providers: HashMap::from([(
            "cloud".to_string(),
            ProviderConfig::new(Provider::OllamaCloud, None, None),
        )]),
        active_provider: Some("cloud".to_string()),
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn ollama_cloud_with_key_is_connected() {
    let connections = Connections {
        providers: HashMap::from([(
            "cloud".to_string(),
            ProviderConfig::new(Provider::OllamaCloud, Some("ollama-key".to_string()), None),
        )]),
        active_provider: Some("cloud".to_string()),
        active_model: None,
    };
    assert!(connections.has_connected_providers());
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
    let path = dir.join("shuvarie").join("connections.toml");

    let connections = sample_connections();
    connections.save_to(&path).expect("save");

    let loaded = Connections::load_from(&path).expect("load");
    assert_eq!(connections, loaded);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn load_from_missing_file_returns_default() {
    let path = std::env::temp_dir().join("shuvarie-nonexistent-connections.toml");
    let connections = Connections::load_from(&path).expect("load");
    assert_eq!(connections, Connections::default());
}

#[test]
fn default_agent_config_is_bounded() {
    let config = Config::default();
    assert_eq!(config.agent.max_turns, 30);
    assert_eq!(config.agent.worker_max_turns, 0);
    assert_eq!(config.agent.effective_max_turns(), 30);
    assert_eq!(config.agent.effective_worker_max_turns(), usize::MAX);
}

#[test]
fn agent_config_round_trip_with_limits() {
    let mut config = Config::default();
    config.agent.max_turns = 20;
    config.agent.worker_max_turns = 10;
    let toml_str = toml::to_string_pretty(&config).expect("serialize");
    let parsed: Config = toml::from_str(&toml_str).expect("deserialize");
    assert_eq!(config, parsed);
    assert_eq!(parsed.agent.effective_max_turns(), 20);
    assert_eq!(parsed.agent.effective_worker_max_turns(), 10);
}

#[test]
fn agent_config_defaults_when_section_absent() {
    let toml_str = "[ui]\n[embedding]\nenabled = false\n";
    let parsed: Config = toml::from_str(toml_str).expect("deserialize");
    assert_eq!(parsed.agent.max_turns, 30);
    assert_eq!(parsed.agent.worker_max_turns, 0);
    assert_eq!(parsed.agent.effective_max_turns(), 30);
}
