use std::collections::HashMap;

use shuvarie_core::{Config, ProviderConfig};
use shuvarie_llm::Provider;

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
        ui: Default::default(),
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
        ui: Default::default(),
    };
    assert!(!config.has_connected_providers());
}

#[test]
fn active_provider_missing_from_map_has_no_connected_providers() {
    let config = Config {
        providers: HashMap::new(),
        active_provider: Some("nonexistent".to_string()),
        active_model: None,
        ui: Default::default(),
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
        ui: Default::default(),
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
        ui: Default::default(),
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
    let path = dir.join("shuvarie").join("config.toml");

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
