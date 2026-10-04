use std::collections::BTreeMap;

use shuvarie_config::{
    Active, Config, ConfigError, Connections, LspConfigRepr, LspServerSpecRepr, ProviderConfig,
    RankingConfig,
};

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
        decision_providers: BTreeMap::new(),
        decision: None,
    }
}

#[test]
fn round_trip_serialization() {
    let connections = sample_connections();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");

    connections.save_to(&path).expect("save");
    let saved = std::fs::read_to_string(&path).expect("read saved file");
    assert!(
        saved.contains("provider id=67e55044") || saved.contains("provider id=\"67e55044"),
        "provider keyed by id property:\n{saved}"
    );
    assert!(saved.contains("gpt-5.5"));
    assert!(saved.contains("api-key"));
    assert!(saved.contains("sk-test"));

    let parsed = Connections::load_from(&path).expect("load");
    assert_eq!(connections, parsed);
}

#[test]
fn saved_connections_start_with_secret_warning() {
    let connections = sample_connections();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("connections.kdl");

    connections.save_to(&path).expect("save");
    let saved = std::fs::read_to_string(&path).expect("read saved file");
    assert!(
        saved.starts_with(
            "// ¡¡¡ THIS FILE CONTAINS YOUR API KEYS !!!\n\
             // ¡¡¡ DO NOT SHARE IT IN PUBLIC !!!\n\n"
        ),
        "expected warning header followed by a blank line at top:\n{saved}"
    );

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
        saved.contains("id=67e55044") || saved.contains("id=\"67e55044"),
        "provider keyed by id property:\n{saved}"
    );
    assert!(saved.contains("name=\"my-openai\"") || saved.contains("name=my-openai"));
    assert!(saved.contains("kind \"openai\"") || saved.contains("kind openai"));
    assert!(
        saved.contains("base-url \"http://localhost:11434\"")
            || saved.contains("base-url http://localhost:11434")
    );
    assert!(
        saved.contains("active {") && saved.contains("gpt-5.5"),
        "expected active block with model:\n{saved}",
    );
}

#[test]
fn parse_hand_written_kdl() {
    let kdl = r#"
active {
    provider "67e55044-10b1-426f-9247-bb680e5fe0c8"
    model "gpt-5.5"
}
providers {
    provider id="67e55044-10b1-426f-9247-bb680e5fe0c8" name="my-openai" {
        kind "openai"
        api-key "sk-test"
    }
    provider id="70b3d8e2-58e1-48a2-9d21-4ef0be7f95c1" name="local-ollama" {
        kind "ollama"
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
    provider id="12f6bb9a-4db2-4d8f-bd3a-4d1d5b8f0b9b" name="my-openai" {
        kind "openai"
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
    let kdl = "providers {\n    provider id=\"local\" name=\"local\" { kind \"ollama\" }\n}\n";
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
    assert_eq!(parsed.context.keep_recent_tokens, 20_000);
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
fn config_context_fields_kebab_round_trip() {
    let mut config = Config::default();
    config.context.disabled = true;
    config.context.reserved = 5_000;
    config.context.keep_recent_tokens = 8_000;
    config.context.tool_output_max_chars = 1_000;
    config.context.fallback_context_length = 32_000;
    config.skills.dirs = vec!["/tmp/skills".to_string(), "/opt/skills".to_string()];
    config.ui.frame_rate = 0;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    config.save_to(&path).expect("save");

    let saved = std::fs::read_to_string(&path).expect("read");
    assert!(saved.contains("tool-output-max-chars 1000"));
    assert!(saved.contains("keep-recent-tokens 8000"));
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
    let ConfigError::Parse(parse_err) = err else {
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
    let ConfigError::Parse(parse_err) = err else {
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

/// Parses one config file through the public surface.
fn parse_config(kdl: &str) -> shuvarie_config::Result<Config> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    std::fs::write(&path, kdl).expect("write");
    Config::load_from(&path)
}

/// Writes a config and reads it back, the way the app's save path does.
fn round_trip(config: &Config) -> Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("config.kdl");
    config.save_to(&path).expect("save");
    Config::load_from(&path).expect("load")
}

#[test]
fn decisions_cover_all_three_answer_shapes() {
    let config = parse_config(
        r#"
        decisions {
            decision "shell-safety" {
                type "noul"
                instructions "Does this command carry a destructive intent?"
                yes "harmful"
                no "benign"
            }
            decision "tool-risk" {
                type "choice"
                instructions "Which risk class does this tool call fall into?"
                option "safe"
                option "review" { description "Runs, but should be confirmed" }
            }
            decision "option-fit" {
                type "score"
                instructions "How well does this option satisfy the intent?"
                level "poor" { description "Does not address the intent" }
                level "best"
            }
        }
        "#,
    )
    .expect("parses");

    assert_eq!(config.decisions.decisions.len(), 3);
    let safety = config.decisions.get("shell-safety").expect("shell-safety");
    assert_eq!(safety.kind, shuvarie_config::DecisionType::Noul);
    assert_eq!(safety.yes.as_deref(), Some("harmful"));
    assert_eq!(safety.no.as_deref(), Some("benign"));

    let risk = config.decisions.get("tool-risk").expect("tool-risk");
    assert_eq!(risk.options.len(), 2);
    assert_eq!(risk.options[0].label, "safe");
    // A missing description reads as the label itself.
    assert!(risk.options[0].description.is_none());
    assert_eq!(
        risk.options[1].description.as_deref(),
        Some("Runs, but should be confirmed")
    );

    let fit = config.decisions.get("option-fit").expect("option-fit");
    assert_eq!(fit.levels.len(), 2);
    // A labelled level keeps its label; the wire text is the description.
    assert_eq!(fit.levels[0].name.as_deref(), Some("poor"));
    assert_eq!(fit.levels[0].description, "Does not address the intent");
    // A bare level is its own description.
    assert_eq!(fit.levels[1].name, None);
    assert_eq!(fit.levels[1].description, "best");

    assert_eq!(config, round_trip(&config));
}

#[test]
fn decision_children_must_match_the_declared_type() {
    for (kdl, expected) in [
        (
            "decisions {\n    decision \"a\" {\n        type \"noul\"\n        \
                    instructions \"x\"\n        option \"safe\"\n    }\n}",
            "a `noul` decision takes",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"choice\"\n        \
                    instructions \"x\"\n    }\n}",
            "at least one `option`",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"score\"\n        \
                    instructions \"x\"\n    }\n}",
            "at least one `level`",
        ),
        (
            "decisions {\n    decision \"a\" {\n        instructions \"x\"\n    }\n}",
            "requires a `type` child",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"noul\"\n    }\n}",
            "requires an `instructions` child",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"ranking\"\n        \
                    instructions \"x\"\n    }\n}",
            "unknown decision type `ranking`",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"noul\"\n        \
                    instructions \"x\"\n        yes \"y\"\n    }\n}",
            "needs both `yes` and `no`",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"choice\"\n        \
                    instructions \"x\"\n        option \"safe\"\n        option \"safe\"\n    \
                    }\n}",
            "duplicate `option` label",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"noul\"\n        \
                    instructions \"\"\n    }\n}",
            "`instructions` must not be empty",
        ),
        (
            "decisions {\n    decision \"a\" {\n        type \"noul\"\n        \
                    instructions \"x\"\n    }\n    decision \"a\" {\n        \
                    type \"noul\"\n        instructions \"y\"\n    }\n}",
            "duplicate decision `a`",
        ),
    ] {
        let err = parse_config(kdl).expect_err("should fail");
        assert!(err.to_string().contains(expected), "{kdl}: {err}");
    }
}

/// Decision models reach a network endpoint, so a `decisions` section is off
/// unless it says otherwise — and the switch round-trips in both spellings.
#[test]
fn decisions_are_disabled_unless_the_section_enables_them() {
    const DECISION: &str =
        "    decision \"a\" {\n        type \"noul\"\n        instructions \"x\"\n    \n}";

    // No switch at all: the definitions parse, but the feature stays off.
    let silent = parse_config(&format!("decisions {{\n{DECISION}}}\n")).expect("parses");
    assert!(!silent.decisions.is_enabled());
    assert_eq!(silent.decisions.disabled, None);
    assert_eq!(silent, round_trip(&silent));

    let on =
        parse_config(&format!("decisions {{\n    enabled #true\n{DECISION}}}\n")).expect("parses");
    assert!(on.decisions.is_enabled());
    assert_eq!(on.decisions.disabled, Some(false));
    assert_eq!(on, round_trip(&on));

    // `disabled #true` is the default state, but writing it is meaningful: it
    // pins the feature off against a lower-priority layer that would turn it on.
    let off =
        parse_config(&format!("decisions {{\n    disabled #true\n{DECISION}}}\n")).expect("parses");
    assert!(!off.decisions.is_enabled());
    assert_eq!(off.decisions.disabled, Some(true));
    assert_eq!(off, round_trip(&off));
}

/// A `decisions` section carrying only the switch is still worth writing back,
/// and a bad switch is a loud error rather than a silent drop.
#[test]
fn the_decisions_switch_parses_strictly() {
    let only_switch = parse_config("decisions {\n    enabled #true\n}\n").expect("parses");
    assert!(only_switch.decisions.is_enabled());
    assert_eq!(only_switch, round_trip(&only_switch));

    for (kdl, expected) in [
        (
            "decisions {\n    enabled #true\n    enabled #true\n}",
            "duplicate `enabled`",
        ),
        ("decisions {\n    enabled \"yes\"\n}", "a boolean"),
        (
            "decisions {\n    ranking #true\n}",
            "expected `decision`, `enabled`, or `disabled`",
        ),
    ] {
        let err = parse_config(kdl).expect_err("should fail");
        assert!(err.to_string().contains(expected), "{kdl}: {err}");
    }
}

/// `ranking` names the `decisions` entry that reorders a question's options —
/// globally and per scene — and round-trips in both spellings of the switch.
#[test]
fn ranking_round_trips_at_top_level_and_per_scene() {
    let config = parse_config(
        r#"
        decisions {
            decision "option-fit" {
                type "score"
                instructions "How well does this option fit the intent?"
                level "poor" { description "Does not address the intent" }
                level "best"
            }
        }
        ranking {
            decision "option-fit"
        }
        scenes {
            scene name="Plan" {
                ranking {
                    decision "option-fit"
                }
            }
            scene name="Review" {
                ranking {
                    disabled #true
                }
            }
        }
        "#,
    )
    .expect("parses");

    assert_eq!(config.ranking.decision.as_deref(), Some("option-fit"));
    assert!(!config.ranking.disabled);

    let plan = config.scenes.scene("Plan").expect("Plan");
    assert_eq!(plan.ranking.decision.as_deref(), Some("option-fit"));
    assert!(!plan.ranking.disabled);

    let review = config.scenes.scene("Review").expect("Review");
    assert_eq!(review.ranking.decision, None);
    assert!(review.ranking.disabled);

    // The scene layers resolve over the global block: a scene that names one
    // wins, and a scene that is `disabled` turns ranking off.
    assert_eq!(
        RankingConfig::resolve(&config.ranking, Some(&review.ranking)),
        None
    );
    assert_eq!(
        RankingConfig::resolve(&config.ranking, Some(&plan.ranking)),
        Some("option-fit")
    );

    assert_eq!(config, round_trip(&config));

    // The switch round-trips at the top level too, in both spellings: written
    // explicitly so it survives against a lower-priority layer.
    let off = parse_config("ranking {\n    disabled #true\n    decision \"option-fit\"\n}\n")
        .expect("parses");
    assert!(off.ranking.disabled);
    assert_eq!(off.ranking.decision.as_deref(), Some("option-fit"));
    assert_eq!(off, round_trip(&off));

    let on = parse_config("ranking {\n    enabled #true\n    decision \"option-fit\"\n}\n")
        .expect("parses");
    assert!(!on.ranking.disabled);
    assert_eq!(on, round_trip(&on));

    // An empty block is nothing to write back.
    let empty = parse_config("ranking {\n    enabled #true\n}\n").expect("parses");
    assert!(empty.ranking.is_empty());
    assert_eq!(empty, round_trip(&empty));
}

/// A `ranking` block takes one name and the switch, and nothing else.
#[test]
fn ranking_errors_are_reported() {
    for (kdl, expected) in [
        (
            "ranking {\n    option-fit \"fit\"\n}",
            "unknown node `option-fit` in `ranking`",
        ),
        ("ranking \"option-fit\"", "takes no arguments"),
        (
            "ranking {\n    decision \"option-fit\"\n    decision \"other\"\n}",
            "duplicate `decision`",
        ),
        ("ranking {\n    decision \"\"\n}", "must not be empty"),
        ("ranking {\n    decision\n}", "requires a name"),
        // A property on the name is not silently dropped, and neither is one on
        // the block itself — the block takes no properties at all.
        (
            "ranking {\n    decision \"option-fit\" fit=#true\n}",
            "not properties",
        ),
        (
            "ranking fit=#true {\n    decision \"option-fit\"\n}",
            "`ranking` takes no properties",
        ),
        (
            "scenes { scene name=\"Plan\" { ranking { fit } } }",
            "unknown node `fit` in `ranking`",
        ),
    ] {
        let err = match parse_config(kdl) {
            Ok(_) => panic!("expected an error for: {kdl}"),
            Err(err) => err,
        };
        assert!(err.to_string().contains(expected), "{kdl}: {err}");
    }
}
#[test]
fn shell_checks_parse_with_defaults_and_round_trip() {
    let config = parse_config(
        r#"
        permissions {
            shell-patterns {
                allow-all
                check-all { decision "shell-safety" }
                check "rm" "sudo" pattern="regex" on-error="deny" threshold=0.9 {
                    decision "shell-safety"
                }
            }
        }
        "#,
    )
    .expect("parses");

    let checks = &config.permissions.checks;
    assert_eq!(checks.len(), 2);
    assert_eq!(
        checks[0].source,
        shuvarie_config::ShellCheckSource::Decision("shell-safety".into())
    );
    assert!(
        checks[0].patterns.is_empty(),
        "`check-all` covers everything"
    );
    // A check that cannot run must not silently widen access.
    assert_eq!(checks[0].on_error, shuvarie_config::Verb::Ask);
    assert_eq!(
        checks[0].threshold,
        shuvarie_config::DEFAULT_CHECK_THRESHOLD
    );

    assert_eq!(
        checks[1].patterns,
        vec!["rm".to_string(), "sudo".to_string()]
    );
    assert_eq!(checks[1].kind, shuvarie_config::ShellPatternKind::Regex);
    assert_eq!(checks[1].on_error, shuvarie_config::Verb::Deny);
    assert_eq!(checks[1].threshold, 0.9);

    assert_eq!(config, round_trip(&config));
}

/// A check may name a `subagents` worker instead of a `decisions` entry. The two
/// are different machinery, so the config says which and the round trip keeps
/// them apart.
#[test]
fn worker_checks_parse_and_round_trip() {
    let config = parse_config(
        r#"
        permissions {
            shell-patterns {
                check-all { worker "command-checker" }
                check "rm" on-error="deny" { worker "command-checker" }
            }
        }
        "#,
    )
    .expect("parses");

    let checks = &config.permissions.checks;
    assert_eq!(checks.len(), 2);
    assert_eq!(
        checks[0].source,
        shuvarie_config::ShellCheckSource::Worker("command-checker".into())
    );
    assert!(!checks[0].source.is_decision());
    assert_eq!(checks[0].source.name(), "command-checker");
    assert!(
        checks[0].patterns.is_empty(),
        "`check-all` covers everything"
    );

    assert_eq!(checks[1].on_error, shuvarie_config::Verb::Deny);
    assert_eq!(checks[1].patterns, vec!["rm".to_string()]);
    assert_eq!(config, round_trip(&config));
}

/// A worker answers in prose, so there is no probability for a threshold to sit
/// on: writing one would silently do nothing.
#[test]
fn a_worker_check_rejects_a_threshold() {
    let err = parse_config(
        "permissions {\n    shell-patterns {\n        check-all threshold=0.9 {\n            \
         worker \"command-checker\"\n        }\n    }\n}",
    )
    .expect_err("a worker check takes no threshold");
    assert!(
        err.to_string()
            .contains("`threshold` applies to a `decision` check"),
        "{err}"
    );
}

/// Naming both would leave it undefined which answer the verdict came from.
#[test]
fn a_check_asking_both_a_decision_and_a_worker_is_rejected() {
    let err = parse_config(
        "permissions {\n    shell-patterns {\n        check-all {\n            decision \
         \"shell-safety\"\n            worker \"command-checker\"\n        }\n    }\n}",
    )
    .expect_err("a check asks one or the other");
    assert!(err.to_string().contains("not both"), "{err}");
}

/// A worker name is as opaque as a decision name: nothing validates it against
/// a registry, because there is none.
#[test]
fn a_worker_check_rejects_an_empty_name() {
    let err = parse_config(
        r#"
        permissions {
            shell-patterns {
                check-all {
                    worker ""
                }
            }
        }
        "#,
    )
    .expect_err("an empty worker name names nothing");
    assert!(
        err.to_string().contains("`worker` name must not be empty"),
        "{err}"
    );
}

#[test]
fn a_config_without_checks_has_none() {
    let config = parse_config("permissions {\n    shell-patterns {\n        allow-all\n    }\n}")
        .expect("parses");
    assert!(config.permissions.checks.is_empty());
    assert_eq!(
        config.permissions.shell.default,
        Some(shuvarie_config::Verb::Allow)
    );
    // The section's rules survive untouched and re-serialize unchanged.
    assert_eq!(config, round_trip(&config));
}

/// `permissions { tool-check { … } }` maps a `choice` decision's option labels
/// to verbs, with the unmappable answer falling back to `on-error`.
#[test]
fn tool_checks_parse_with_defaults_and_round_trip() {
    let config = parse_config(
        r#"
        permissions {
            tool-check {
                decision "tool-risk"
                allow "safe"
                ask "review"
                deny "block" "dangerous"
            }
        }
        "#,
    )
    .expect("parses");

    let check = config.permissions.tool_check.as_ref().expect("tool check");
    assert_eq!(check.decision, "tool-risk");
    // A check that cannot run must not silently widen access.
    assert_eq!(check.on_error, shuvarie_config::Verb::Ask);
    assert_eq!(
        check
            .rules
            .iter()
            .map(|rule| (rule.verb, rule.label.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (shuvarie_config::Verb::Allow, "safe"),
            (shuvarie_config::Verb::Ask, "review"),
            // One node carries several labels, one rule per label.
            (shuvarie_config::Verb::Deny, "block"),
            (shuvarie_config::Verb::Deny, "dangerous"),
        ]
    );

    assert_eq!(config, round_trip(&config));

    // `on-error` is written only when it differs from the default, and the
    // block round-trips either way.
    let explicit = parse_config(
        "permissions {\n    tool-check on-error=\"deny\" {\n        decision \"tool-risk\"\n        \
         allow \"safe\"\n    }\n}\n",
    )
    .expect("parses");
    let check = explicit
        .permissions
        .tool_check
        .as_ref()
        .expect("tool check");
    assert_eq!(check.on_error, shuvarie_config::Verb::Deny);
    assert_eq!(check.rules.len(), 1);
    assert_eq!(explicit, round_trip(&explicit));

    // Nothing configured: no tool check at all.
    let plain = parse_config("permissions {\n    ask-all\n}\n").expect("parses");
    assert!(plain.permissions.tool_check.is_none());
    assert_eq!(plain, round_trip(&plain));
}

#[test]
fn tool_check_errors_are_reported() {
    for (kdl, expected) in [
        // The node itself takes no arguments and one property.
        (
            "permissions {\n    tool-check \"tool-risk\"\n}",
            "takes no arguments",
        ),
        (
            "permissions {\n    tool-check decision=\"tool-risk\"\n}",
            "unknown property `decision`",
        ),
        (
            "permissions {\n    tool-check on-error=\"maybe\" {\n        decision \"a\"\n    \n}\n}",
            "`on-error` must be `allow`, `ask`, or `deny`",
        ),
        (
            "permissions {\n    tool-check {\n        allow \"safe\"\n    \n}\n}",
            "requires a `decision` child",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"a\"\n        decision \"b\"\n    \n}\n}",
            "duplicate `decision`",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"\"\n    \n}\n}",
            "must not be empty",
        ),
        (
            "permissions {\n    tool-check {\n        decision\n    \n}\n}",
            "requires a `decision` child",
        ),
        // A positional name carrying properties is rejected, not silently read
        // as the name.
        (
            "permissions {\n    tool-check {\n        decision \"a\" some-prop=#true\n    \n}\n}",
            "not properties",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"a\" \"b\"\n    \n}\n}",
            "takes a single argument",
        ),
        // Verb nodes take labels, never properties, and at least one label.
        (
            "permissions {\n    tool-check {\n        decision \"a\"\n        allow \"safe\" review=#true\n    \n}\n}",
            "not properties",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"a\"\n        allow\n    \n}\n}",
            "requires at least one option label",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"a\"\n        allow \"safe\"\n        \
             deny \"safe\"\n    \n}\n}",
            "duplicate option label",
        ),
        (
            "permissions {\n    tool-check {\n        decision \"a\"\n        escalate \"high\"\n    \n}\n}",
            "unknown node `escalate` in `tool-check`",
        ),
        // The block is a sibling of `shell-patterns`, not a check inside it.
        (
            "permissions {\n    shell-patterns {\n        tool-check {\n            decision \"a\"\n        \
             \n    }\n}\n}",
            "unknown node `tool-check` in `shell-patterns`",
        ),
        // And the section names it among its children.
        (
            "permissions {\n    tool-checks {\n        decision \"a\"\n    \n}\n}",
            "expected `allow-all`",
        ),
    ] {
        let err = parse_config(kdl).expect_err("should fail");
        assert!(err.to_string().contains(expected), "{kdl}: {err}");
    }
}

#[test]
fn shell_check_errors_are_reported() {
    for (kdl, expected) in [
        (
            "permissions {\n    shell-patterns {\n        check-all {\n            decision \
                    \"a\"\n            \"x\"\n        }\n    }\n}",
            "unknown node",
        ),
        (
            "permissions {\n    shell-patterns {\n        check-all {\n        }\n    }\n}",
            "requires a `decision` or `worker` child",
        ),
        (
            "permissions {\n    shell-patterns {\n        check \"rm\" {\n        }\n    }\n}",
            "requires a `decision` or `worker` child",
        ),
        (
            "permissions {\n    shell-patterns {\n        check-all pattern=\"regex\" {\n            \
                    decision \"a\"\n        }\n    }\n}",
            "takes no `pattern` property",
        ),
        (
            "permissions {\n    shell-patterns {\n        check-all on-error=\"maybe\" {\n            \
                    decision \"a\"\n        }\n    }\n}",
            "`on-error` must be `allow`, `ask`, or `deny`",
        ),
        (
            "permissions {\n    shell-patterns {\n        check-all threshold=1.5 {\n            \
                    decision \"a\"\n        }\n    }\n}",
            "must be a probability between 0 and 1",
        ),
        (
            "permissions {\n    shell-patterns {\n        check-all {\n            decision \
                    \"a\"\n            \"x\"\n        }\n    }\n}",
            "unknown node",
        ),
        (
            "permissions {\n    paths {\n        check-all {\n            decision \"a\"\n        \
                    }\n    }\n}",
            "only `shell-patterns` takes decision checks",
        ),
    ] {
        let err = parse_config(kdl).expect_err("should fail");
        assert!(err.to_string().contains(expected), "{kdl}: {err}");
    }
}
