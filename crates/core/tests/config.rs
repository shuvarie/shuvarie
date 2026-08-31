use std::collections::BTreeMap;

use shuvarie_core::{
    Config, Connections, CoreError, LspConfigRepr, LspServerSpecRepr, ProviderConfig,
};

fn sample_connections() -> Connections {
    let mut providers = BTreeMap::new();
    providers.insert(
        "my-openai".to_string(),
        ProviderConfig::new("openai", Some("sk-test".to_string()), None),
    );
    providers.insert(
        "local-ollama".to_string(),
        ProviderConfig::new("ollama", None, Some("http://localhost:11434".into())),
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
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");

    connections.save_to(&path).expect("save");
    let saved = std::fs::read_to_string(&path).expect("read saved file");
    assert!(saved.contains("active my-openai") || saved.contains("active \"my-openai\""));
    assert!(saved.contains("gpt-5.5"));
    assert!(saved.contains("api-key"));
    assert!(saved.contains("sk-test"));

    let parsed = Connections::load_from(&path).expect("load");
    assert_eq!(connections, parsed);
}

#[test]
fn default_connections_round_trip() {
    let connections = Connections::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");

    connections.save_to(&path).expect("save");
    let parsed = Connections::load_from(&path).expect("load");
    assert_eq!(connections, parsed);
}

#[test]
fn default_config_round_trip() {
    let config = Config::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");

    config.save_to(&path).expect("save");
    let parsed = Config::load_from(&path).expect("load");
    assert_eq!(config, parsed);
}

#[test]
fn kdl_document_shape() {
    let connections = sample_connections();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");
    connections.save_to(&path).expect("save");

    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(
        saved.contains("providers {"),
        "expected providers section:\n{saved}"
    );
    assert!(
        saved.contains("provider my-openai") || saved.contains("provider \"my-openai\""),
        "provider keyed by first argument:\n{saved}"
    );
    assert!(saved.contains("kind=openai") || saved.contains("kind=\"openai\""));
    assert!(
        saved.contains("base-url \"http://localhost:11434\"")
            || saved.contains("base-url http://localhost:11434")
    );
    assert!(
        saved.contains("active my-openai") && saved.contains("gpt-5.5")
            || saved.contains("active \"my-openai\" \"gpt-5.5\"")
    );
}

#[test]
fn parse_hand_written_kdl() {
    let kdl = r#"
active "my-openai" "gpt-5.5"
providers {
    provider "my-openai" kind="openai" {
        api-key "sk-test"
    }
    provider "local-ollama" kind="ollama" {
        base-url "http://localhost:11434"
    }
}
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");
    std::fs::write(&path, kdl).expect("write");
    let parsed = Connections::load_from(&path).expect("parse");
    assert_eq!(parsed, sample_connections());
}

#[test]
fn parse_ignores_unknown_nodes() {
    let kdl = r#"
providers {
    provider "my-openai" {
        api-key "sk-test"
        future-field "x"
    }
}
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");
    std::fs::write(&path, kdl).expect("write");
    let parsed = Connections::load_from(&path).expect("parse");
    assert_eq!(parsed.providers.len(), 1);
}

#[test]
fn parse_options_absent() {
    let kdl = "providers {\n    provider local kind=\"ollama\"\n}\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");
    std::fs::write(&path, kdl).expect("write");
    let parsed = Connections::load_from(&path).expect("parse");
    let provider = parsed.providers.get("local").expect("provider");
    assert_eq!(provider.kind, "ollama");
    assert_eq!(provider.api_key, None);
    assert_eq!(provider.base_url, None);
}

#[test]
fn empty_connections_has_no_connected_providers() {
    let connections = Connections::default();
    assert!(!connections.has_connected_providers());
}

#[test]
fn missing_active_provider_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "my-openai".to_string(),
            ProviderConfig::new("openai", Some("sk-test".to_string()), None),
        )]),
        active_provider: None,
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn active_provider_missing_from_map_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::new(),
        active_provider: Some("nonexistent".to_string()),
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn active_provider_without_key_has_no_connected_providers() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "my-openai".to_string(),
            ProviderConfig::new("openai", None, None),
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
        providers: BTreeMap::from([(
            "local".to_string(),
            ProviderConfig::new("ollama", None, None),
        )]),
        active_provider: Some("local".to_string()),
        active_model: None,
    };
    assert!(connections.has_connected_providers());
}

#[test]
fn ollama_cloud_without_key_is_not_connected() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "cloud".to_string(),
            ProviderConfig::new("ollama-cloud", None, None),
        )]),
        active_provider: Some("cloud".to_string()),
        active_model: None,
    };
    assert!(!connections.has_connected_providers());
}

#[test]
fn ollama_cloud_with_key_is_connected() {
    let connections = Connections {
        providers: BTreeMap::from([(
            "cloud".to_string(),
            ProviderConfig::new("ollama-cloud", Some("ollama-key".to_string()), None),
        )]),
        active_provider: Some("cloud".to_string()),
        active_model: None,
    };
    assert!(connections.has_connected_providers());
}

#[test]
fn load_from_missing_file_returns_default() {
    let path = std::env::temp_dir().join("shuvarie-nonexistent-connections.kdl");
    let connections = Connections::load_from(&path).expect("load");
    assert_eq!(connections, Connections::default());
}

#[test]
fn default_agent_config_is_unlimited() {
    let config = Config::default();
    assert_eq!(config.agent.max_turns, 0);
    assert_eq!(config.agent.worker_max_turns, 0);
    assert_eq!(config.agent.effective_max_turns(), usize::MAX);
    assert_eq!(config.agent.effective_worker_max_turns(), usize::MAX);
}

#[test]
fn agent_config_round_trip_with_limits() {
    let mut config = Config::default();
    config.agent.max_turns = 20;
    config.agent.worker_max_turns = 10;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");

    config.save_to(&path).expect("save");
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("max-turns 20"));
    assert!(saved.contains("worker-max-turns 10"));
    // canonical output is stable across a save/load/save cycle
    let path2 = dir.path().join("config-again.kdl");
    let reparsed = Config::load_from(&path).expect("load");
    reparsed.save_to(&path2).expect("resave");
    let resaved = std::fs::read_to_string(&path2).expect("read resaved");
    assert_eq!(saved, resaved, "canonical KDL form should be stable");

    let parsed = Config::load_from(&path).expect("load");
    assert_eq!(config, parsed);
    assert_eq!(parsed.agent.effective_max_turns(), 20);
    assert_eq!(parsed.agent.effective_worker_max_turns(), 10);
}

#[test]
fn agent_config_defaults_when_section_absent() {
    let kdl = "embedding {\n    disabled #true\n}\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, kdl).expect("write");
    let parsed = Config::load_from(&path).expect("deserialize");
    assert_eq!(parsed.agent.max_turns, 0);
    assert_eq!(parsed.agent.worker_max_turns, 0);
    assert_eq!(parsed.agent.effective_max_turns(), usize::MAX);
    assert!(parsed.embedding.disabled);
    assert!(!parsed.context.disabled);
    assert_eq!(parsed.context.reserved, 20_000);
    assert_eq!(parsed.context.tool_output_max_chars, 16_000);
    assert_eq!(parsed.context.fallback_context_length, 128_000);
    assert!(!parsed.lsp.disabled);
    assert!(!parsed.skills.disabled);
    assert_eq!(parsed.ui.frame_rate, 60);
}

#[test]
fn config_with_lsp_servers_round_trip() {
    let mut config = Config::default();
    config.lsp.disabled = true;
    config.lsp.servers.insert(
        "rust".to_string(),
        LspServerSpecRepr {
            command: vec!["rust-analyzer".to_string()],
            extensions: vec!["rs".to_string()],
            no_auto_start: true,
            root_markers: vec!["Cargo.toml".to_string()],
        },
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");

    config.save_to(&path).expect("save");
    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("servers {"), "new layout:\n{saved}");
    assert!(saved.contains("rust {") || saved.contains("\"rust\" {"));
    assert!(saved.contains("no-auto-start #true"));
    assert!(saved.contains("disabled #true"));

    // hand-written form parses too
    let hand = "lsp {\n    disabled #true\n    servers {\n        rust {\n            command \"rust-analyzer\"\n            no-auto-start #true\n        }\n    }\n}\n";
    let dir2 = tempfile::tempdir().expect("tempdir");
    let path2 = dir2.path().join("config.kdl");
    std::fs::write(&path2, hand).expect("write");
    let hand_parsed = Config::load_from(&path2).expect("load");
    assert!(hand_parsed.lsp.disabled);
    let spec = hand_parsed.lsp.servers.get("rust").expect("server");
    assert_eq!(spec.command, vec!["rust-analyzer".to_string()]);
    assert!(spec.no_auto_start);

    let parsed = Config::load_from(&path).expect("load");
    assert_eq!(config, parsed);
}

#[test]
fn lsp_repr_converts_to_shuvarie_lsp_config() {
    let mut config = Config::default();
    config.lsp.servers.insert(
        "go".to_string(),
        LspServerSpecRepr {
            command: vec!["gopls".to_string()],
            extensions: vec!["go".to_string()],
            no_auto_start: true,
            root_markers: vec!["go.mod".to_string()],
        },
    );
    let lsp_config = shuvarie_lsp::LspConfig::from(&config.lsp);
    assert!(lsp_config.enabled);
    let resolved = lsp_config.resolve();
    let go = resolved.get("go").expect("go resolved");
    assert_eq!(go.command, vec!["gopls".to_string()]);
    assert!(!go.auto_start);
    // builtin rust override still merges
    let rust = resolved.get("rust").expect("rust builtin");
    assert_eq!(rust.command, vec!["rust-analyzer".to_string()]);
}

#[test]
fn config_context_fields_kebab_round_trip() {
    let mut config = Config::default();
    config.context.disabled = true;
    config.context.reserved = 5_000;
    config.context.tool_output_max_chars = 1_000;
    config.context.fallback_context_length = 32_000;
    config.skills.dirs = vec!["/tmp/skills".to_string(), "/opt/skills".to_string()];
    config.ui.frame_rate = 0;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    config.save_to(&path).expect("save");

    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("tool-output-max-chars 1000"));
    assert!(saved.contains("fallback-context-length 32000"));
    assert!(saved.contains("frame-rate 0"));
    assert!(saved.contains("dirs") && saved.contains("/tmp/skills"));

    let parsed = Config::load_from(&path).expect("load");
    assert_eq!(config, parsed);
}

#[test]
fn config_embedding_options_round_trip() {
    let mut config = Config::default();
    config.embedding.disabled = true;
    config.embedding.provider = Some("ollama".to_string());
    config.embedding.model = Some("nomic-embed-text".to_string());
    config.embedding.dimensions = Some(768);

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    config.save_to(&path).expect("save");

    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("ollama"));
    assert!(saved.contains("dimensions 768"));

    let parsed = Config::load_from(&path).expect("load");
    assert_eq!(config, parsed);
}

#[test]
fn malformed_kdl_reports_location() {
    let kdl = "ui {\n    frame-rate \"sixty\"\n}\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, kdl).expect("write");

    let err = Config::load_from(&path).expect_err("should fail to parse");
    let CoreError::ConfigParse(parse_err) = err else {
        panic!("expected ConfigParse error, got {err:?}");
    };
    assert_eq!(parse_err.line, 2, "error on second line, got {parse_err:?}");
    assert!(parse_err.location().starts_with("2:"));

    let snippet = parse_err.snippet(kdl).expect("snippet");
    assert!(
        snippet.contains("frame-rate"),
        "snippet shows source line:\n{snippet}"
    );
    assert!(snippet.contains('^'), "snippet has caret:\n{snippet}");
}

#[test]
fn malformed_kdl_detailed_display() {
    let kdl = "foo 1.\n";
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, kdl).expect("write");

    let err = Config::load_from(&path).expect_err("should fail to parse");
    let CoreError::ConfigParse(parse_err) = err else {
        panic!("expected ConfigParse error, got {err:?}");
    };
    let display = parse_err.to_string();
    assert!(display.contains("config parse error") || display.contains("1:"));
}

#[test]
fn lsp_config_repr_default_matches() {
    let repr = LspConfigRepr::default();
    assert!(!repr.disabled);
    assert!(repr.servers.is_empty());
}
