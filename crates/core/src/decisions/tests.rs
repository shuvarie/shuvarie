use super::*;
use crate::question::{QuestionOption, QuestionPrompt};
use httpmock::prelude::*;
use shuvarie_config::{
    DEFAULT_CHECK_THRESHOLD, DecisionLevel, DecisionOption, DecisionsConfig, RankingConfig,
    SceneConfig, ScenesConfig, ShellCheck as ShellCheckConfig, ShellCheckSource, ShellPatternKind,
    SubagentConfig, SubagentToolset, SubagentsConfig, SystemPromptsConfig, ToolCheck,
    ToolChoiceRule,
};

/// The `noul` decision the fixtures name.
fn safety_decision() -> DecisionConfig {
    DecisionConfig {
        kind: DecisionType::Noul,
        instructions: "Does this command carry a destructive intent?".to_string(),
        yes: None,
        no: None,
        options: Vec::new(),
        levels: Vec::new(),
    }
}

fn check(decision: &str, patterns: &[&str]) -> ShellCheckConfig {
    ShellCheckConfig {
        source: ShellCheckSource::Decision(decision.to_string()),
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        kind: ShellPatternKind::Raw,
        on_error: Verb::Ask,
        threshold: 0.5,
    }
}

/// A `worker "…"` check: the prose-answer half of the same syntax.
fn worker_check(worker: &str, patterns: &[&str]) -> ShellCheckConfig {
    ShellCheckConfig {
        source: ShellCheckSource::Worker(worker.to_string()),
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        kind: ShellPatternKind::Raw,
        on_error: Verb::Ask,
        threshold: DEFAULT_CHECK_THRESHOLD,
    }
}

fn config_with(decision: DecisionConfig, check: ShellCheckConfig) -> Config {
    let mut decisions = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    decisions.decisions.insert("safety".to_string(), decision);
    let mut config = Config {
        decisions,
        ..Default::default()
    };
    config.permissions.checks = vec![check];
    config
}

fn connections_for(endpoint: Option<String>) -> Connections {
    let mut connections = Connections::default();
    if let Some(endpoint) = endpoint {
        connections.decision_providers.insert(
            "local-clef".to_string(),
            shuvarie_config::DecisionProviderConfig {
                kind: "systemone".to_string(),
                api_key: None,
                base_url: Some(endpoint),
            },
        );
        connections.decision = Some(shuvarie_config::DecisionActive {
            provider: "local-clef".to_string(),
            model: "clef".to_string(),
        });
    }
    connections
}

/// A compiled check with `on_error` and threshold set, for the pure policy
/// tests.
fn compiled(on_error: Verb) -> CompiledCheck {
    CompiledCheck {
        source: CheckSource::Decision("safety".to_string()),
        scope: CheckScope::All,
        on_error,
        threshold: 0.5,
        problem: None,
    }
}

/// The same, for a `worker` check.
fn compiled_worker(on_error: Verb) -> CompiledCheck {
    CompiledCheck {
        source: CheckSource::Worker("command-checker".to_string()),
        scope: CheckScope::All,
        on_error,
        threshold: DEFAULT_CHECK_THRESHOLD,
        problem: None,
    }
}

// --- the policy (pure) ---------------------------------------------------

#[test]
fn a_suspicious_answer_tightens_an_allow_into_an_ask() {
    let verdict = apply(
        Verdict::Allow,
        &compiled(Verb::Ask),
        Ok(noul_answer(0.97, 0.5)),
    );
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("decision `safety`"), "{reason}");
    assert!(reason.contains("0.97"), "{reason}");
}

#[test]
fn a_benign_answer_leaves_the_verdict_alone() {
    assert_eq!(
        apply(
            Verdict::Allow,
            &compiled(Verb::Ask),
            Ok(noul_answer(0.02, 0.5))
        ),
        Verdict::Allow
    );
}

#[test]
fn the_threshold_is_inclusive() {
    let check = compiled(Verb::Ask);
    assert!(matches!(
        apply(
            Verdict::Allow,
            &check,
            Ok(noul_answer(check.threshold, check.threshold))
        ),
        Verdict::Ask { .. }
    ));
    assert_eq!(
        apply(
            Verdict::Allow,
            &check,
            Ok(noul_answer(check.threshold - 0.01, check.threshold))
        ),
        Verdict::Allow
    );
}

#[test]
fn an_existing_ask_keeps_its_own_reason() {
    let original = Verdict::Ask {
        reason: "shell-patterns: ask \"git\"".to_string(),
    };
    // A decision can tighten but never rewrite the rule that already asked.
    assert_eq!(
        apply(
            original.clone(),
            &compiled(Verb::Ask),
            Ok(noul_answer(0.99, 0.5))
        ),
        original
    );
}

#[test]
fn a_decision_never_loosens_a_deny() {
    let denied = Verdict::Deny {
        reason: "shell-patterns: deny \"rm\"".to_string(),
    };
    // Not a benign answer, not an `on-error allow`, not an `on-error deny`:
    // an existing deny keeps the reason naming the rule that decided it.
    assert_eq!(
        apply(
            denied.clone(),
            &compiled(Verb::Ask),
            Ok(noul_answer(0.0, 0.5))
        ),
        denied
    );
    assert_eq!(
        apply(denied.clone(), &compiled(Verb::Allow), Err("boom".into())),
        denied
    );
    assert_eq!(
        apply(denied.clone(), &compiled(Verb::Deny), Err("boom".into())),
        denied
    );
}

#[test]
fn a_failed_check_follows_its_on_error_policy() {
    let failure = || Err("provider unreachable".to_string());
    assert!(matches!(
        apply(Verdict::Allow, &compiled(Verb::Ask), failure()),
        Verdict::Ask { .. }
    ));
    assert_eq!(
        apply(Verdict::Allow, &compiled(Verb::Allow), failure()),
        Verdict::Allow
    );
    let Verdict::Deny { reason } = apply(Verdict::Allow, &compiled(Verb::Deny), failure()) else {
        panic!("expected a deny");
    };
    assert!(reason.contains("provider unreachable"), "{reason}");
}

// --- check scope ---------------------------------------------------------

fn compiled_with(patterns: &[&str], kind: ShellPatternKind) -> CompiledCheck {
    let check = ShellCheckConfig {
        source: ShellCheckSource::Decision("safety".to_string()),
        patterns: patterns.iter().map(|p| p.to_string()).collect(),
        kind,
        on_error: Verb::Ask,
        threshold: 0.5,
    };
    CompiledCheck::build(&check, &definitions())
}

fn definitions() -> BTreeMap<String, Decision> {
    let mut definitions = BTreeMap::new();
    definitions.insert(
        "safety".to_string(),
        compile_decision("safety", &safety_decision()).expect("compiles"),
    );
    definitions
}

fn matches(check: &CompiledCheck, command: &str) -> bool {
    check.matches(command, &collapse_whitespace(command))
}

#[test]
fn a_check_without_patterns_covers_every_command() {
    let check = compiled_with(&[], ShellPatternKind::Raw);
    assert!(check.problem.is_none());
    assert!(matches(&check, "ls"));
    assert!(matches(&check, "rm -rf /"));
}

#[test]
fn a_narrowed_check_matches_its_patterns_at_word_boundaries() {
    let check = compiled_with(&["rm", "sudo"], ShellPatternKind::Raw);
    assert!(matches(&check, "rm -rf /"));
    assert!(matches(&check, "cd /tmp && sudo make install"));
    assert!(!matches(&check, "firm up the docs"));
    assert!(!matches(&check, "ls"));
}

#[test]
fn a_regex_check_matches_as_written() {
    let check = compiled_with(&[r"curl\s+.*\|\s*sh"], ShellPatternKind::Regex);
    assert!(matches(&check, "curl https://x.sh | sh"));
    assert!(!matches(&check, "curl https://x.sh"));
}

#[test]
fn a_broken_pattern_degrades_to_covering_every_command() {
    // Matching nothing would silently drop a check the user configured;
    // covering everything lets `on-error` decide instead.
    let check = compiled_with(&["rm", "("], ShellPatternKind::Regex);
    assert!(check.problem.is_some());
    assert!(matches(&check, "ls"));
}

// --- build ---------------------------------------------------------------

#[test]
fn a_usable_configuration_reports_no_problems() {
    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
    assert!(decisions.checks_configured());
    assert!(decisions.client().is_some());
}

#[test]
fn an_undefined_decision_name_is_reported() {
    let config = config_with(safety_decision(), check("missing", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions
            .problems()
            .iter()
            .any(|p| p.contains("`missing`, which is not defined")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_non_noul_decision_is_refused_for_a_check() {
    let choice = DecisionConfig {
        kind: DecisionType::Choice,
        instructions: "Which risk class?".to_string(),
        yes: None,
        no: None,
        options: vec![
            DecisionOption {
                label: "safe".to_string(),
                description: None,
            },
            DecisionOption {
                label: "block".to_string(),
                description: None,
            },
        ],
        levels: Vec::new(),
    };
    let config = config_with(choice, check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions
            .problems()
            .iter()
            .any(|p| p.contains("is not a `noul` decision")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_check_without_a_decision_connection_is_reported() {
    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(
        decisions
            .problems()
            .iter()
            .any(|p| p.contains("no decision provider is selected")),
        "{:?}",
        decisions.problems()
    );
    assert!(decisions.client().is_none());
}

#[test]
fn an_invalid_definition_is_reported_rather_than_compiled() {
    let mut decision = safety_decision();
    decision.instructions = "  ".to_string();
    let config = config_with(decision, check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    // The definition is dropped, so the check that names it fails too.
    assert!(decisions.definitions().is_empty());
    assert!(
        decisions
            .problems()
            .iter()
            .any(|p| p.contains("instructions must not be empty")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_score_rubric_is_carried_through_compilation() {
    let score = DecisionConfig {
        kind: DecisionType::Score,
        instructions: "How well does this fit?".to_string(),
        yes: None,
        no: None,
        options: Vec::new(),
        levels: vec![
            DecisionLevel {
                name: Some("poor".to_string()),
                description: "Does not address it".to_string(),
            },
            DecisionLevel {
                name: None,
                description: "Fully addresses it".to_string(),
            },
        ],
    };
    let mut decisions = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    decisions.decisions.insert("fit".to_string(), score);
    let config = Config {
        decisions,
        ..Default::default()
    };
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
    let compiled = decisions.definitions().get("fit").expect("compiled");
    let DecisionKind::Score { levels } = &compiled.kind else {
        panic!("expected a score decision");
    };
    assert_eq!(levels.len(), 2);
    assert_eq!(levels[0].name.as_deref(), Some("poor"));
    assert_eq!(levels[1].description, "Fully addresses it");
}

// --- end to end ----------------------------------------------------------

#[tokio::test]
async fn a_suspicious_command_becomes_an_ask_through_the_provider() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "safety": { "type": "noul", "noul": 0.97 } },
            }));
    });
    let config = config_with(safety_decision(), check("safety", &[]));
    let connections = connections_for(Some(server.url("/v1/systemone")));
    let decisions = Decisions::build(&config, &connections);

    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    mock.assert();
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("0.97"), "{reason}");
}

#[tokio::test]
async fn a_benign_command_stays_allowed_through_the_provider() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "safety": { "type": "noul", "noul": 0.02 } },
            }));
    });
    let config = config_with(safety_decision(), check("safety", &[]));
    let connections = connections_for(Some(server.url("/v1/systemone")));
    let decisions = Decisions::build(&config, &connections);

    assert_eq!(
        decisions.check_shell("ls", Verdict::Allow).await,
        Verdict::Allow
    );
}

#[tokio::test]
async fn a_narrowed_check_leaves_unmatched_commands_alone() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "safety": { "type": "noul", "noul": 0.99 } },
            }));
    });
    let config = config_with(safety_decision(), check("safety", &["rm"]));
    let connections = connections_for(Some(server.url("/v1/systemone")));
    let decisions = Decisions::build(&config, &connections);

    // `ls` is out of scope: no call is made at all.
    assert_eq!(
        decisions.check_shell("ls -la", Verdict::Allow).await,
        Verdict::Allow
    );
    assert_eq!(mock.calls(), 0);
}

#[tokio::test]
async fn an_already_denied_command_never_reaches_the_provider() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({ "model": "clef", "answers": {} }));
    });
    let config = config_with(safety_decision(), check("safety", &[]));
    let connections = connections_for(Some(server.url("/v1/systemone")));
    let decisions = Decisions::build(&config, &connections);

    let denied = Verdict::Deny {
        reason: "shell-patterns: deny \"rm\"".to_string(),
    };
    assert_eq!(
        decisions.check_shell("rm -rf /", denied.clone()).await,
        denied
    );
    // Nothing to learn about a command that will not run.
    assert_eq!(mock.calls(), 0);
}

#[tokio::test]
async fn an_unreachable_provider_follows_on_error() {
    // A port nothing listens on: the check cannot run.
    let config = config_with(safety_decision(), check("safety", &[]));
    let connections = connections_for(Some("http://127.0.0.1:1/v1/systemone".into()));
    let decisions = Decisions::build(&config, &connections);

    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("could not run"), "{reason}");
}

// --- mid-session connection ----------------------------------------------

/// The connections for one decision provider, with the model chosen and the
/// endpoint pointing at `endpoint`.
fn connections_with(endpoint: &str, model: &str) -> Connections {
    let mut connections = connections_for(None);
    connections.decision_providers.insert(
        "local-clef".to_string(),
        shuvarie_config::DecisionProviderConfig {
            kind: "systemone".to_string(),
            api_key: None,
            base_url: Some(endpoint.to_string()),
        },
    );
    connections.decision = Some(shuvarie_config::DecisionActive {
        provider: "local-clef".to_string(),
        model: model.to_string(),
    });
    connections
}

/// A mock answering every `/v1/systemone` request with one `noul` probability.
fn noul_mock(server: &MockServer, probability: f64) -> httpmock::Mock<'_> {
    server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "safety": { "type": "noul", "noul": probability } },
            }));
    })
}

/// The dialog registers decision connections while the program runs, so a
/// service compiled before one existed has to start asking without a restart.
#[tokio::test]
async fn a_check_starts_working_when_a_connection_is_set_mid_session() {
    let server = MockServer::start();
    let mock = noul_mock(&server, 0.97);

    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(decisions.client().is_none());

    decisions
        .set_connection(&connections_with(&server.url("/v1/systemone"), "clef"))
        .expect("the connection builds");
    assert!(decisions.client().is_some());

    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    mock.assert();
    assert!(matches!(verdict, Verdict::Ask { .. }), "{verdict:?}");
}

/// Re-running the dialog over an existing name edits it in place, so the
/// service has to follow the new endpoint and model rather than keep the ones
/// it started with.
#[tokio::test]
async fn setting_a_connection_replaces_the_one_in_use() {
    let first = MockServer::start();
    noul_mock(&first, 0.02);
    let second = MockServer::start();
    let switched = noul_mock(&second, 0.97);

    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_with(&first.url("/v1/systemone"), "clef"),
    );
    // The startup connection reads the command as benign.
    assert_eq!(
        decisions.check_shell("rm -rf /", Verdict::Allow).await,
        Verdict::Allow
    );

    decisions
        .set_connection(&connections_with(
            &second.url("/v1/systemone"),
            "jev-latest",
        ))
        .expect("the connection builds");

    assert_eq!(
        decisions.client().expect("a client").model(),
        "jev-latest",
        "the selected model is replaced, not kept"
    );
    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    switched.assert();
    assert!(matches!(verdict, Verdict::Ask { .. }), "{verdict:?}");
}

/// Selecting no connection has to clear the client, so a check falls back to
/// its `on-error` policy instead of asking a connection that is gone.
#[tokio::test]
async fn setting_no_connection_returns_the_check_to_its_error_policy() {
    let server = MockServer::start();
    let mock = noul_mock(&server, 0.97);

    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );
    assert!(decisions.client().is_some());

    // Selecting nothing is reported too — there is nothing left to ask — and
    // the half that matters here is that the old client is cleared.
    let error = decisions
        .set_connection(&Connections::default())
        .expect_err("nothing is selected");
    assert!(
        error.contains("no decision provider is selected"),
        "{error}"
    );
    assert!(decisions.client().is_none());

    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(
        reason.contains("no decision provider is selected"),
        "{reason}"
    );
    // Nothing was asked of the provider that is no longer selected.
    assert_eq!(mock.calls(), 0);
}

/// A selection that names a provider which is not defined is unusable. The
/// service must not keep answering on the connection it was already holding —
/// that would run checks against an endpoint the user replaced — and the reason
/// is returned rather than swallowed.
#[tokio::test]
async fn an_unusable_selection_clears_the_client_and_reports_why() {
    let server = MockServer::start();
    let mock = noul_mock(&server, 0.97);

    let config = config_with(safety_decision(), check("safety", &[]));
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );
    assert!(decisions.client().is_some());

    // The `decision` selection names a provider that `decision-providers` does
    // not define.
    let broken = Connections {
        decision: Some(shuvarie_config::DecisionActive {
            provider: "missing".to_string(),
            model: "clef".to_string(),
        }),
        ..Default::default()
    };

    let error = decisions
        .set_connection(&broken)
        .expect_err("the provider is not defined");
    assert!(error.contains("`missing`"), "{error}");
    assert!(
        decisions.client().is_none(),
        "the replaced connection must not keep answering"
    );

    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    assert!(matches!(verdict, Verdict::Ask { .. }), "{verdict:?}");
    assert_eq!(mock.calls(), 0, "the old connection was not asked");
}

/// Decision models reach a network endpoint, so they stay off until the config
/// says otherwise — even when definitions and checks are configured, and even
/// when a usable connection exists. Nothing is compiled, no request is made, and
/// the configured check is reported rather than silently skipped.
#[tokio::test]
async fn decisions_stay_off_unless_the_config_enables_them() {
    let server = MockServer::start();
    let mock = noul_mock(&server, 0.97);

    // `config_with` writes the definitions and checks but leaves the switch unset.
    let mut config = config_with(safety_decision(), check("safety", &[]));
    config.decisions.disabled = None;
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    assert!(!decisions.is_enabled());
    assert!(
        decisions.client().is_none(),
        "no connection is built while the feature is off"
    );
    assert!(
        decisions.definitions().is_empty(),
        "no decision is compiled while the feature is off"
    );
    assert!(
        decisions.checks_configured(),
        "the check is still configured"
    );

    // The gate is reported, so the author is told why nothing runs.
    let problem = decisions.problems().first().expect("a problem is reported");
    assert!(problem.contains("enabled #true"), "{problem}");

    // The check follows its own `on-error` policy rather than passing.
    let verdict = decisions.check_shell("rm -rf /", Verdict::Allow).await;
    assert!(matches!(verdict, Verdict::Ask { .. }), "{verdict:?}");
    assert_eq!(mock.calls(), 0, "the provider was never asked");
}

/// The default — no decisions, no checks — is inert: nothing to do, and nothing
/// to warn about.
#[test]
fn a_config_that_uses_no_decisions_is_silently_off() {
    let decisions = Decisions::build(&Config::default(), &Connections::default());
    assert!(!decisions.is_enabled());
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
}

/// Turning the feature on without configuring any check is quiet too: the switch
/// alone is not a problem.
#[test]
fn enabling_decisions_without_checks_reports_nothing() {
    let config = Config {
        decisions: DecisionsConfig {
            disabled: Some(false),
            ..Default::default()
        },
        ..Default::default()
    };
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(decisions.is_enabled());
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
}

/// Registering a connection while the feature is off is not a user error, so the
/// service does not fail — but it says why nothing will run, which is the whole
/// point of reporting it rather than ignoring the selection.
#[test]
fn registering_a_connection_while_disabled_says_the_feature_is_off() {
    let mut config = config_with(safety_decision(), check("safety", &[]));
    config.decisions.disabled = None;
    let decisions = Decisions::build(&config, &Connections::default());

    let error = decisions
        .set_connection(&connections_for(Some(
            "http://localhost:11434/v1/systemone".into(),
        )))
        .expect_err("nothing is stored while the feature is off");
    assert!(error.contains("enabled #true"), "{error}");
    assert!(decisions.client().is_none());
}

// --- option ranking -------------------------------------------------------

/// A score rubric suitable for ranking: worst first, so the higher score is
/// the better fit.
fn fit_decision() -> DecisionConfig {
    DecisionConfig {
        kind: DecisionType::Score,
        instructions: "How well does this option fit the question?".to_string(),
        yes: None,
        no: None,
        options: Vec::new(),
        levels: vec![
            DecisionLevel {
                name: None,
                description: "Does not fit".to_string(),
            },
            DecisionLevel {
                name: None,
                description: "Fits perfectly".to_string(),
            },
        ],
    }
}

/// The global `ranking` block naming the `fit` decision.
fn ranking_on() -> RankingConfig {
    RankingConfig {
        decision: Some("fit".to_string()),
        disabled: false,
    }
}

/// A config whose `decisions` section defines `fit` (when a definition is
/// given) and whose `ranking` block is `ranking`.
fn ranking_config(definition: Option<DecisionConfig>, ranking: RankingConfig) -> Config {
    let mut decisions = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    if let Some(definition) = definition {
        decisions.decisions.insert("fit".to_string(), definition);
    }
    Config {
        decisions,
        ranking,
        ..Default::default()
    }
}

/// A labelled option with no description.
fn option(label: &str) -> QuestionOption {
    described(label, "")
}

/// A labelled option with a description.
fn described(label: &str, description: &str) -> QuestionOption {
    QuestionOption {
        label: label.to_string(),
        description: description.to_string(),
    }
}

/// One question offering the given options.
fn prompt(options: &[QuestionOption]) -> QuestionPrompt {
    QuestionPrompt {
        question: "Which layout?".to_string(),
        header: "Layout".to_string(),
        options: options.to_vec(),
        multiple: false,
        custom: false,
    }
}

/// The labels a prompt offers, in order.
fn labels(prompt: &QuestionPrompt) -> Vec<&str> {
    prompt
        .options
        .iter()
        .map(|option| option.label.as_str())
        .collect()
}

/// A mock answering every `/v1/systemone` request with one rubric score per
/// named question, so a ranking request's answers can be read back per asked
/// option.
fn score_mock_entries(server: &MockServer, scores: Vec<(String, f64)>) -> httpmock::Mock<'_> {
    let answers = scores
        .into_iter()
        .map(|(name, score)| {
            (
                name,
                serde_json::json!({
                    "type": "score",
                    "score": score,
                    "probabilities": { "0": 1.0 - score, "1": score },
                    "legend": { "0": "Does not fit", "1": "Fits perfectly" },
                    "confidence": 0.9,
                }),
            )
        })
        .collect::<BTreeMap<String, Value>>();
    server.mock(move |when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": answers.clone(),
            }));
    })
}

/// The same, for a fixed list of names: `&[("option-0-0", 0.9), …]`.
fn score_mock<'a>(server: &'a MockServer, scores: &[(&str, f64)]) -> httpmock::Mock<'a> {
    score_mock_entries(
        server,
        scores
            .iter()
            .map(|(name, score)| ((*name).to_string(), *score))
            .collect(),
    )
}

/// Ranking reorders a prompt's options by fit, with each label's description
/// travelling with it, and answers every prompt in one request.
#[tokio::test]
async fn ranking_orders_a_prompts_options_by_fit() {
    let server = MockServer::start();
    // The first option fits worst in both prompts, the second best.
    let mock = score_mock(
        &server,
        &[
            ("option-0-0", 0.1),
            ("option-0-1", 0.9),
            ("option-1-0", 0.4),
            ("option-1-1", 0.6),
        ],
    );
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    let ranked = decisions
        .rank(
            vec![
                prompt(&[
                    described("sidebar", "A column beside the chat"),
                    described("tabs", "One pane at a time"),
                ]),
                prompt(&[option("above"), option("below")]),
            ],
            None,
        )
        .await;

    mock.assert();
    assert_eq!(labels(&ranked[0]), vec!["tabs", "sidebar"]);
    // The whole option moves, not just its label.
    assert_eq!(ranked[0].options[0].description, "One pane at a time");
    assert_eq!(labels(&ranked[1]), vec!["below", "above"]);
    assert_eq!(mock.calls(), 1, "every prompt is answered in one request");
}

/// Equal scores keep the order the question offered the options in.
#[tokio::test]
async fn equal_scores_keep_the_order_the_question_offered() {
    let server = MockServer::start();
    let mock = score_mock(&server, &[("option-0-0", 0.5), ("option-0-1", 0.5)]);
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    let asked = vec![prompt(&[option("sidebar"), option("tabs")])];
    let ranked = decisions.rank(asked.clone(), None).await;

    mock.assert();
    assert_eq!(ranked, asked);
}

/// A prompt with fewer than two options has nothing to reorder, so it never
/// reaches the provider at all.
#[tokio::test]
async fn a_prompt_with_one_option_is_never_ranked() {
    let server = MockServer::start();
    let mock = score_mock(&server, &[]);
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    let asked = vec![prompt(&[option("sidebar")])];
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);
    assert_eq!(mock.calls(), 0);
}

/// An answer the provider left out (or that came back unreadable) leaves that
/// prompt exactly as asked rather than guessing an order for it.
#[tokio::test]
async fn a_missing_answer_leaves_the_prompt_as_asked() {
    let server = MockServer::start();
    // Only the first option's answer comes back.
    let mock = score_mock(&server, &[("option-0-0", 0.1)]);
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    let asked = vec![prompt(&[option("sidebar"), option("tabs")])];
    let ranked = decisions.rank(asked.clone(), None).await;

    mock.assert();
    assert_eq!(ranked, asked);
}

/// A prompt is ranked whole or not at all: one whose options would not fit the
/// request's question budget is skipped rather than partially reordered.
#[tokio::test]
async fn a_prompt_that_would_not_fit_the_request_is_skipped_whole() {
    let server = MockServer::start();
    // 33 + 32 options is one question past the protocol's budget of 64.
    let mock = score_mock_entries(
        &server,
        (0..33)
            .map(|option| (format!("option-0-{option}"), option as f64 / 100.0))
            .collect(),
    );
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with(&server.url("/v1/systemone"), "clef"),
    );

    let first = (0..33)
        .map(|index| option(&format!("first-{index}")))
        .collect::<Vec<_>>();
    let second = (0..32)
        .map(|index| option(&format!("second-{index}")))
        .collect::<Vec<_>>();
    let asked = vec![prompt(&first), prompt(&second)];
    let ranked = decisions.rank(asked.clone(), None).await;

    mock.assert();
    assert_eq!(
        mock.calls(),
        1,
        "the over-budget prompt is not a request of its own"
    );
    // The first prompt was ranked whole (worst score first, so reversed) —
    // which also says the second was left out of the request: a request over
    // the budget fails whole and would have ranked nothing at all.
    assert_eq!(labels(&ranked[0])[0], "first-32");
    assert_eq!(labels(&ranked[0])[32], "first-0");
    assert_eq!(ranked[1], asked[1]);
}

/// A provider that cannot answer leaves the question exactly as the tool asked
/// it, in every way ranking can fail — and ranking that cannot run asks
/// nothing at all.
#[tokio::test]
async fn a_ranking_that_cannot_run_leaves_the_options_as_asked() {
    let server = MockServer::start();
    let mock = score_mock(&server, &[("option-0-0", 0.9), ("option-0-1", 0.1)]);
    let endpoint = server.url("/v1/systemone");
    let asked = vec![prompt(&[option("sidebar"), option("tabs")])];

    // The config turns ranking off.
    let off = ranking_config(
        Some(fit_decision()),
        RankingConfig {
            decision: Some("fit".to_string()),
            disabled: true,
        },
    );
    let decisions = Decisions::build(&off, &connections_with(&endpoint, "clef"));
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);

    // The named decision is not defined.
    let undefined = ranking_config(None, ranking_on());
    let decisions = Decisions::build(&undefined, &connections_with(&endpoint, "clef"));
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("`fit`, which is not defined")),
        "{:?}",
        decisions.problems()
    );
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);

    // The named decision is not a rubric.
    let mut choice = fit_decision();
    choice.kind = DecisionType::Choice;
    choice.levels = Vec::new();
    choice.options = vec![
        DecisionOption {
            label: "safe".to_string(),
            description: None,
        },
        DecisionOption {
            label: "review".to_string(),
            description: None,
        },
    ];
    let wrong_kind = ranking_config(Some(choice), ranking_on());
    let decisions = Decisions::build(&wrong_kind, &connections_with(&endpoint, "clef"));
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("which is not a `score` decision")),
        "{:?}",
        decisions.problems()
    );
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);

    // A scene that turns ranking off beats the global block.
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(&config, &connections_with(&endpoint, "clef"));
    let scene = RankingConfig {
        decision: None,
        disabled: true,
    };
    assert_eq!(decisions.rank(asked.clone(), Some(&scene)).await, asked);

    // A scene that names a decision which cannot rank is left alone too.
    let unrankable = RankingConfig {
        decision: Some("safety".to_string()),
        disabled: false,
    };
    assert_eq!(
        decisions.rank(asked.clone(), Some(&unrankable)).await,
        asked
    );

    // No decision provider is selected.
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(decisions.client().is_none());
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);

    assert_eq!(mock.calls(), 0, "the provider was never asked");
}

/// A provider failure — unreachable, refused, or erroring — is a no-op too.
#[tokio::test]
async fn a_failing_provider_leaves_the_options_as_asked() {
    // A port nothing listens on.
    let config = ranking_config(Some(fit_decision()), ranking_on());
    let decisions = Decisions::build(
        &config,
        &connections_with("http://127.0.0.1:1/v1/systemone", "clef"),
    );

    let asked = vec![prompt(&[option("sidebar"), option("tabs")])];
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);
    let asked = vec![prompt(&[option("sidebar"), option("tabs")])];
    assert_eq!(decisions.rank(asked.clone(), None).await, asked);
}

/// A scene that names a decision nothing defines is reported too, named by the
/// scene: ranking fails open at question time, so startup is the only place the
/// mistake can be pointed out.
#[test]
fn a_scene_ranking_block_that_cannot_rank_is_reported() {
    let scene = |ranking: RankingConfig| {
        let mut config = ranking_config(Some(fit_decision()), RankingConfig::default());
        config.scenes.scenes.insert(
            "Plan".to_string(),
            shuvarie_config::SceneConfig {
                ranking,
                ..Default::default()
            },
        );
        config
    };

    let missing = scene(RankingConfig {
        decision: Some("typo".to_string()),
        disabled: false,
    });
    let decisions = Decisions::build(&missing, &Connections::default());
    assert_eq!(decisions.problems().len(), 1, "{:?}", decisions.problems());
    assert!(
        decisions.problems()[0].starts_with("scene `Plan`: "),
        "{:?}",
        decisions.problems()
    );
    assert!(decisions.problems()[0].contains("not defined"));

    // A scene that ranks with a usable decision is quiet, and one that is
    // `disabled` has nothing to report at all.
    let usable = scene(RankingConfig {
        decision: Some("fit".to_string()),
        disabled: false,
    });
    assert!(
        Decisions::build(&usable, &Connections::default())
            .problems()
            .is_empty()
    );
    let off = scene(RankingConfig {
        decision: Some("typo".to_string()),
        disabled: true,
    });
    assert!(
        Decisions::build(&off, &Connections::default())
            .problems()
            .is_empty()
    );
}

// --- the tool check ------------------------------------------------------

/// The `choice` decision the tool-check fixtures name.
fn risk_decision() -> DecisionConfig {
    DecisionConfig {
        kind: DecisionType::Choice,
        instructions: "Which risk class does this tool call fall into?".to_string(),
        yes: None,
        no: None,
        options: ["safe", "review", "block"]
            .into_iter()
            .map(|label| DecisionOption {
                label: label.to_string(),
                description: None,
            })
            .collect(),
        levels: Vec::new(),
    }
}

/// The mapping the fixtures use: `safe` allows, `review` asks, `block` denies,
/// and an unusable answer follows `on_error`.
fn tool_check(on_error: Verb) -> ToolCheck {
    ToolCheck {
        decision: "tool-risk".to_string(),
        rules: [
            (Verb::Allow, "safe"),
            (Verb::Ask, "review"),
            (Verb::Deny, "block"),
        ]
        .into_iter()
        .map(|(verb, label)| ToolChoiceRule {
            verb,
            label: label.to_string(),
        })
        .collect(),
        on_error,
    }
}

/// A config whose `decisions` section defines `tool-risk` (when a definition is
/// given) and whose `permissions` section carries `check`.
fn tool_check_config(definition: Option<DecisionConfig>, check: ToolCheck) -> Config {
    let mut decisions = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    if let Some(definition) = definition {
        decisions
            .decisions
            .insert("tool-risk".to_string(), definition);
    }
    let mut config = Config {
        decisions,
        ..Default::default()
    };
    config.permissions.tool_check = Some(check);
    config
}

/// The compiled `tool-risk` definition, for the pure policy tests.
fn choice_definitions() -> BTreeMap<String, Decision> {
    let mut definitions = BTreeMap::new();
    definitions.insert(
        "tool-risk".to_string(),
        compile_decision("tool-risk", &risk_decision()).expect("compiles"),
    );
    definitions
}

/// A compiled tool check with `on_error` set, for the pure policy tests.
fn compiled_tool_check(on_error: Verb) -> CompiledToolCheck {
    CompiledToolCheck::build(&tool_check(on_error), &choice_definitions())
}

// --- the tool check's policy (pure) --------------------------------------

#[test]
fn the_state_names_the_tool_and_the_call() {
    assert_eq!(
        tool_state("mcp__fs__write", "Server `fs`, tool `write`"),
        serde_json::json!({ "tool": "mcp__fs__write", "call": "Server `fs`, tool `write`" })
    );
}

#[test]
fn a_mapped_ask_label_tightens_an_allow_into_an_ask() {
    let verdict = apply_tool_choice(
        Verdict::Allow,
        &compiled_tool_check(Verb::Ask),
        Ok("review".to_string()),
    );
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("decision `tool-risk`"), "{reason}");
    assert!(reason.contains("`review`"), "{reason}");
}

#[test]
fn a_mapped_deny_label_denies_an_allow() {
    let verdict = apply_tool_choice(
        Verdict::Allow,
        &compiled_tool_check(Verb::Ask),
        Ok("block".to_string()),
    );
    let Verdict::Deny { reason } = verdict else {
        panic!("expected a deny, got {verdict:?}");
    };
    assert!(reason.contains("`block`"), "{reason}");
}

#[test]
fn a_mapped_allow_label_leaves_the_verdict_alone() {
    assert_eq!(
        apply_tool_choice(
            Verdict::Allow,
            &compiled_tool_check(Verb::Ask),
            Ok("safe".to_string()),
        ),
        Verdict::Allow
    );
}

#[test]
fn an_existing_ask_or_deny_keeps_its_own_reason() {
    let asked = Verdict::Ask {
        reason: "scene requires confirmation for `mcp__fs__write`".to_string(),
    };
    let denied = Verdict::Deny {
        reason: "permissions default: deny-all".to_string(),
    };
    // Neither a stricter answer nor a failed check may rewrite the rule that
    // already decided this call.
    assert_eq!(
        apply_tool_choice(
            asked.clone(),
            &compiled_tool_check(Verb::Deny),
            Ok("block".to_string()),
        ),
        asked
    );
    assert_eq!(
        apply_tool_choice(
            denied.clone(),
            &compiled_tool_check(Verb::Ask),
            Ok("review".to_string()),
        ),
        denied
    );
    assert_eq!(
        apply_tool_choice(
            denied.clone(),
            &compiled_tool_check(Verb::Deny),
            Err("provider unreachable".to_string()),
        ),
        denied
    );
}

#[test]
fn an_unmapped_label_follows_on_error() {
    let unmapped = || Ok("escalate".to_string());
    assert_eq!(
        apply_tool_choice(
            Verdict::Allow,
            &compiled_tool_check(Verb::Allow),
            unmapped()
        ),
        Verdict::Allow
    );
    let Verdict::Ask { reason } =
        apply_tool_choice(Verdict::Allow, &compiled_tool_check(Verb::Ask), unmapped())
    else {
        panic!("expected an ask");
    };
    assert!(reason.contains("`escalate`"), "{reason}");
    assert!(reason.contains("not mapped"), "{reason}");
    let Verdict::Deny { reason } =
        apply_tool_choice(Verdict::Allow, &compiled_tool_check(Verb::Deny), unmapped())
    else {
        panic!("expected a deny");
    };
    assert!(reason.contains("not mapped"), "{reason}");
}

#[test]
fn a_failed_tool_check_follows_its_on_error_policy() {
    let failure = || Err("provider unreachable".to_string());
    assert_eq!(
        apply_tool_choice(Verdict::Allow, &compiled_tool_check(Verb::Allow), failure()),
        Verdict::Allow
    );
    let Verdict::Ask { reason } =
        apply_tool_choice(Verdict::Allow, &compiled_tool_check(Verb::Ask), failure())
    else {
        panic!("expected an ask");
    };
    assert!(reason.contains("provider unreachable"), "{reason}");
    let Verdict::Deny { reason } =
        apply_tool_choice(Verdict::Allow, &compiled_tool_check(Verb::Deny), failure())
    else {
        panic!("expected a deny");
    };
    assert!(reason.contains("provider unreachable"), "{reason}");
}

// --- the tool check's build ----------------------------------------------

#[test]
fn a_usable_tool_check_reports_no_problems() {
    let config = tool_check_config(Some(risk_decision()), tool_check(Verb::Ask));
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
    assert!(decisions.tool_check.is_some());
}

#[test]
fn an_undefined_tool_check_decision_is_reported() {
    let mut check = tool_check(Verb::Ask);
    check.decision = "missing".to_string();
    let config = tool_check_config(Some(risk_decision()), check);
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("`missing`, which is not defined")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_non_choice_decision_is_refused_for_a_tool_check() {
    let mut check = tool_check(Verb::Ask);
    check.decision = "safety".to_string();
    let mut decisions_config = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    decisions_config
        .decisions
        .insert("safety".to_string(), safety_decision());
    let mut config = Config {
        decisions: decisions_config,
        ..Default::default()
    };
    config.permissions.tool_check = Some(check);

    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("which is not a `choice` decision")),
        "{:?}",
        decisions.problems()
    );
}

/// A label the decision does not declare could never come back from the
/// provider: the typo is reported rather than left to fall through `on-error`
/// on every call.
#[test]
fn a_label_the_decision_does_not_declare_is_reported() {
    let mut check = tool_check(Verb::Ask);
    check.rules.push(ToolChoiceRule {
        verb: Verb::Deny,
        label: "blocked".to_string(),
    });
    let config = tool_check_config(Some(risk_decision()), check);
    let decisions = Decisions::build(
        &config,
        &connections_for(Some("http://localhost:11434".into())),
    );
    let reported = decisions
        .problems()
        .iter()
        .find(|problem| problem.contains("does not declare"))
        .expect("the undeclared label is reported");
    assert!(reported.contains("`blocked`"), "{reported}");
}

/// A configured tool check must never silently disappear: with decision models
/// off it is still reported, and the call still follows its own `on-error`.
#[tokio::test]
async fn a_tool_check_is_reported_when_decision_models_are_disabled() {
    let mut config = Config::default();
    config.permissions.tool_check = Some(tool_check(Verb::Ask));
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(!decisions.is_enabled());
    let reported = decisions
        .problems()
        .iter()
        .find(|problem| problem.contains("decision models are disabled"))
        .expect("a configured check is never silently dropped");
    assert!(
        reported.contains("tool check names decision `tool-risk`"),
        "{reported}"
    );

    let verdict = decisions
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await;
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("disabled"), "{reason}");
}

// --- the tool check end to end -------------------------------------------

/// A mock answering every `/v1/systemone` request with one `choice` answer for
/// `tool-risk`.
fn choice_mock<'a>(server: &'a MockServer, choice: &str) -> httpmock::Mock<'a> {
    choice_mock_for(server, &["safe", "review", "block"], choice)
}

/// A choice answer whose labels are exactly `alternatives`, with `choice` the
/// likely one. The provider refuses a response whose labels differ from the
/// alternatives the request named, so the two have to agree.
fn choice_mock_for<'a>(
    server: &'a MockServer,
    alternatives: &[&str],
    choice: &str,
) -> httpmock::Mock<'a> {
    let choice = choice.to_string();
    // The provider refuses a distribution that does not sum to one, so the
    // likely label carries the remainder rather than a fixed weight.
    let other = 0.1;
    let top = 1.0 - other * (alternatives.len().saturating_sub(1)) as f64;
    let probabilities = alternatives
        .iter()
        .map(|label| {
            (
                label.to_string(),
                if *label == choice.as_str() {
                    top
                } else {
                    other
                },
            )
        })
        .collect::<BTreeMap<String, f64>>();
    server.mock(move |when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "tool-risk": {
                    "type": "choice",
                    "choice": choice.clone(),
                    "probabilities": probabilities.clone(),
                    "confidence": 0.8,
                } },
            }));
    })
}

/// The service for the end-to-end cases: the config's tool check against a
/// provider at `endpoint`.
fn tool_check_service(on_error: Verb, endpoint: &str) -> Decisions {
    let config = tool_check_config(Some(risk_decision()), tool_check(on_error));
    Decisions::build(&config, &connections_for(Some(endpoint.to_string())))
}

#[tokio::test]
async fn a_choice_answer_tightens_an_allow_through_the_provider() {
    let server = MockServer::start();
    let mock = choice_mock(&server, "review");
    let decisions = tool_check_service(Verb::Ask, &server.url("/v1/systemone"));

    let verdict = decisions
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await;
    mock.assert();
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("`review`"), "{reason}");
}

#[tokio::test]
async fn a_deny_answer_denies_an_allow_through_the_provider() {
    let server = MockServer::start();
    let mock = choice_mock(&server, "block");
    let decisions = tool_check_service(Verb::Ask, &server.url("/v1/systemone"));

    let verdict = decisions
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await;
    mock.assert();
    let Verdict::Deny { reason } = verdict else {
        panic!("expected a deny, got {verdict:?}");
    };
    assert!(reason.contains("`block`"), "{reason}");
}

#[tokio::test]
async fn a_safe_answer_leaves_an_allow_alone() {
    let server = MockServer::start();
    let mock = choice_mock(&server, "safe");
    let decisions = tool_check_service(Verb::Ask, &server.url("/v1/systemone"));

    assert_eq!(
        decisions
            .check_tool(
                "mcp__fs__write",
                "Server `fs`, tool `write`",
                Verdict::Allow
            )
            .await,
        Verdict::Allow
    );
    mock.assert();
}

#[tokio::test]
async fn an_already_denied_call_never_reaches_the_provider() {
    let server = MockServer::start();
    let mock = choice_mock(&server, "block");
    let decisions = tool_check_service(Verb::Ask, &server.url("/v1/systemone"));

    let denied = Verdict::Deny {
        reason: "permissions default: deny-all".to_string(),
    };
    assert_eq!(
        decisions
            .check_tool(
                "mcp__fs__write",
                "Server `fs`, tool `write`",
                denied.clone()
            )
            .await,
        denied
    );
    // Nothing to learn about a call that will not run.
    assert_eq!(mock.calls(), 0);
}

#[tokio::test]
async fn an_unmapped_label_follows_the_on_error_policy_through_the_provider() {
    let server = MockServer::start();
    // The answer is a declared alternative the config maps to nothing: the
    // provider is happy with it, but the rules have no verb for it.
    let mut definition = risk_decision();
    definition.options.push(DecisionOption {
        label: "escalate".to_string(),
        description: None,
    });
    let endpoint = server.url("/v1/systemone");
    let service = |on_error| {
        let config = tool_check_config(Some(definition.clone()), tool_check(on_error));
        Decisions::build(&config, &connections_for(Some(endpoint.clone())))
    };
    let mock = choice_mock_for(
        &server,
        &["safe", "review", "block", "escalate"],
        "escalate",
    );

    // `on-error allow`: an answer the config cannot map is ignored.
    let lenient = service(Verb::Allow);
    assert_eq!(
        lenient
            .check_tool(
                "mcp__fs__write",
                "Server `fs`, tool `write`",
                Verdict::Allow
            )
            .await,
        Verdict::Allow
    );
    mock.assert();

    // `on-error deny`: it denies, naming the option nobody mapped.
    let strict = service(Verb::Deny);
    let Verdict::Deny { reason } = strict
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await
    else {
        panic!("expected a deny");
    };
    assert!(reason.contains("escalate"), "{reason}");
}

#[tokio::test]
async fn an_answer_of_another_shape_follows_the_on_error_policy() {
    let server = MockServer::start();
    // A `noul` answer to a `choice` question: unusable either way.
    let mock = server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(serde_json::json!({
                "model": "clef",
                "answers": { "tool-risk": { "type": "noul", "noul": 0.9 } },
            }));
    });
    let decisions = tool_check_service(Verb::Deny, &server.url("/v1/systemone"));

    let verdict = decisions
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await;
    mock.assert();
    let Verdict::Deny { reason } = verdict else {
        panic!("expected a deny, got {verdict:?}");
    };
    assert!(reason.contains("could not run"), "{reason}");
}

#[tokio::test]
async fn an_unreachable_tool_check_follows_on_error() {
    // A port nothing listens on: the check cannot run.
    let unreachable = "http://127.0.0.1:1/v1/systemone";
    let verdict = tool_check_service(Verb::Deny, unreachable)
        .check_tool(
            "mcp__fs__write",
            "Server `fs`, tool `write`",
            Verdict::Allow,
        )
        .await;
    let Verdict::Deny { reason } = verdict else {
        panic!("expected a deny, got {verdict:?}");
    };
    assert!(reason.contains("could not run"), "{reason}");

    assert_eq!(
        tool_check_service(Verb::Allow, unreachable)
            .check_tool(
                "mcp__fs__write",
                "Server `fs`, tool `write`",
                Verdict::Allow
            )
            .await,
        Verdict::Allow
    );
}

// --- worker checks (prose answers) ---------------------------------------

/// A scene defining `subagents`, for the preamble-resolution tests.
fn scene_with(subagents: SubagentsConfig) -> crate::scenes::Scene {
    let mut scenes = ScenesConfig::default();
    scenes.scenes.insert(
        "review".to_string(),
        SceneConfig {
            subagents,
            ..Default::default()
        },
    );
    crate::scenes::Scene::resolve(&scenes, Some("review"))
}

fn worker(toolset: Option<SubagentToolset>, prelude: Option<&str>) -> SubagentConfig {
    SubagentConfig {
        toolset,
        system_prompts: SystemPromptsConfig {
            prelude: prelude.map(str::to_string),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn workers_of(entries: Vec<(&str, SubagentConfig)>) -> SubagentsConfig {
    SubagentsConfig {
        disabled: false,
        workers: entries
            .into_iter()
            .map(|(name, worker)| (name.to_string(), worker))
            .collect(),
    }
}

fn config_with_worker_check(worker_name: &str, scenes: ScenesConfig) -> Config {
    let mut decisions = DecisionsConfig {
        disabled: Some(false),
        ..Default::default()
    };
    decisions
        .decisions
        .insert("safety".to_string(), safety_decision());
    let mut config = Config {
        decisions,
        scenes,
        ..Default::default()
    };
    config.permissions.checks = vec![worker_check(worker_name, &[])];
    config
}

#[test]
fn the_worker_contract_is_read_strictly() {
    for (reply, expected) in [
        ("safe", CheckAnswer::Safe),
        ("SAFE", CheckAnswer::Safe),
        ("`safe`", CheckAnswer::Safe),
        ("safe.", CheckAnswer::Safe),
        // A reason on a `safe` line is still a safe answer.
        ("safe: nothing to worry about", CheckAnswer::Safe),
        (
            "suspicious: it deletes the tree",
            CheckAnswer::Suspicious(
                "reads this command as suspicious: it deletes the tree".to_string(),
            ),
        ),
        (
            "Suspicious \u{2014} pipes a secret out",
            CheckAnswer::Suspicious(
                "reads this command as suspicious: pipes a secret out".to_string(),
            ),
        ),
        (
            "suspicious",
            CheckAnswer::Suspicious("reads this command as suspicious".to_string()),
        ),
    ] {
        assert_eq!(parse_worker_verdict(reply), Ok(expected), "{reply}");
    }
}

#[test]
fn an_answer_off_the_contract_is_unusable() {
    // Each of these either buries the verdict, argues both ways, or is empty.
    // None may be read as `safe`: an unusable answer follows `on-error`.
    for reply in [
        "",
        "   \n  \n",
        "safely",
        "maybe",
        "This command looks suspicious, but on balance it is safe.",
        "I cannot tell without more context.",
    ] {
        assert!(parse_worker_verdict(reply).is_err(), "{reply:?}");
    }
}

#[test]
fn only_the_first_non_empty_line_is_read() {
    assert_eq!(
        parse_worker_verdict("\n\nsafe\n\nIt only lists files."),
        Ok(CheckAnswer::Safe)
    );
}

#[test]
fn a_long_reason_is_capped_for_the_prompt() {
    let reply = format!("suspicious: {}", "x".repeat(500));
    let Ok(CheckAnswer::Suspicious(reason)) = parse_worker_verdict(&reply) else {
        panic!("expected a suspicious answer");
    };
    assert!(reason.ends_with('\u{2026}'), "{reason}");
    assert!(
        reason.chars().count() <= MAX_WORKER_REASON_CHARS + 48,
        "{}",
        reason.chars().count()
    );
}

#[test]
fn a_suspicious_worker_answer_tightens_an_allow() {
    let verdict = apply(
        Verdict::Allow,
        &compiled_worker(Verb::Ask),
        Ok(CheckAnswer::Suspicious(
            "reads this command as suspicious: rm -rf /".to_string(),
        )),
    );
    let Verdict::Ask { reason } = verdict else {
        panic!("expected an ask, got {verdict:?}");
    };
    assert!(reason.contains("worker `command-checker`"), "{reason}");
    assert!(reason.contains("rm -rf /"), "{reason}");
}

#[test]
fn a_safe_worker_answer_leaves_the_verdict_alone() {
    assert_eq!(
        apply(
            Verdict::Allow,
            &compiled_worker(Verb::Ask),
            Ok(CheckAnswer::Safe)
        ),
        Verdict::Allow
    );
}

#[test]
fn a_worker_that_cannot_run_follows_on_error() {
    assert!(matches!(
        apply(
            Verdict::Allow,
            &compiled_worker(Verb::Ask),
            Err("no turn connection".into())
        ),
        Verdict::Ask { .. }
    ));
    assert_eq!(
        apply(
            Verdict::Allow,
            &compiled_worker(Verb::Allow),
            Err("no turn connection".into())
        ),
        Verdict::Allow
    );
    assert!(matches!(
        apply(
            Verdict::Allow,
            &compiled_worker(Verb::Deny),
            Err("no turn connection".into())
        ),
        Verdict::Deny { .. }
    ));
}

#[test]
fn a_worker_answer_never_loosens_an_existing_deny() {
    let denied = Verdict::Deny {
        reason: "shell-patterns: deny \"rm\"".to_string(),
    };
    // Even a `safe` answer leaves a rule's deny exactly as it was.
    assert_eq!(
        apply(
            denied.clone(),
            &compiled_worker(Verb::Ask),
            Ok(CheckAnswer::Safe)
        ),
        denied
    );
}

#[test]
fn a_scene_hands_its_workers_to_the_check() {
    let scene = scene_with(workers_of(vec![
        (
            "command-checker",
            worker(Some(SubagentToolset::None), Some("Be terse.")),
        ),
        (
            "off",
            SubagentConfig {
                disabled: true,
                ..Default::default()
            },
        ),
    ]));
    let preambles = worker_preambles(&scene);
    assert_eq!(
        preambles.get("command-checker").map(String::as_str),
        Some("Be terse.")
    );
    assert!(
        !preambles.contains_key("off"),
        "a disabled worker is not offered"
    );
}

#[test]
fn a_worker_without_its_own_prelude_gets_the_check_contract() {
    let scene = scene_with(workers_of(vec![(
        "plain",
        worker(Some(SubagentToolset::None), None),
    )]));
    assert_eq!(
        worker_preambles(&scene).get("plain").map(String::as_str),
        Some(DEFAULT_CHECK_PREAMBLE)
    );
}

#[test]
fn a_scene_with_subagents_off_hands_over_nothing() {
    let scene = scene_with(SubagentsConfig {
        disabled: true,
        workers: workers_of(vec![(
            "command-checker",
            worker(Some(SubagentToolset::None), None),
        )])
        .workers,
    });
    assert!(worker_preambles(&scene).is_empty());
}

#[test]
fn the_built_in_default_scene_hands_over_nothing() {
    let scene = crate::scenes::Scene::resolve(&ScenesConfig::default(), None);
    assert!(worker_preambles(&scene).is_empty());
}

#[test]
fn a_worker_no_scene_defines_is_reported_at_startup() {
    let config = config_with_worker_check("command-checker", ScenesConfig::default());
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("which no scene's `subagents` defines")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_worker_the_scene_defines_is_not_reported() {
    let mut scenes = ScenesConfig::default();
    scenes.scenes.insert(
        "review".to_string(),
        SceneConfig {
            subagents: workers_of(vec![(
                "command-checker",
                worker(Some(SubagentToolset::None), Some("Be terse.")),
            )]),
            ..Default::default()
        },
    );
    let config = config_with_worker_check("command-checker", scenes);
    let decisions = Decisions::build(&config, &Connections::default());
    // A worker check needs no decision connection, so the absent one is not a
    // problem it should be told about.
    assert!(
        decisions.problems().is_empty(),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_worker_toolset_the_check_will_ignore_is_reported() {
    let mut scenes = ScenesConfig::default();
    scenes.scenes.insert(
        "review".to_string(),
        SceneConfig {
            subagents: workers_of(vec![(
                "command-checker",
                worker(Some(SubagentToolset::Read), Some("Be terse.")),
            )]),
            ..Default::default()
        },
    );
    let config = config_with_worker_check("command-checker", scenes);
    let decisions = Decisions::build(&config, &Connections::default());
    assert!(
        decisions
            .problems()
            .iter()
            .any(|problem| problem.contains("whose `toolset` the check ignores")),
        "{:?}",
        decisions.problems()
    );
}

#[test]
fn a_disabled_worker_check_reports_the_gate_by_name() {
    let mut decisions = DecisionsConfig {
        disabled: Some(true),
        ..Default::default()
    };
    decisions
        .decisions
        .insert("safety".to_string(), safety_decision());
    let mut config = Config {
        decisions,
        ..Default::default()
    };
    config.permissions.checks = vec![worker_check("command-checker", &[])];
    let service = Decisions::build(&config, &Connections::default());
    assert!(
        service
            .problems()
            .iter()
            .any(|problem| problem.contains("worker `command-checker`")),
        "{:?}",
        service.problems()
    );
}

/// A worker check with no turn connection yet cannot run, and says so instead
/// of passing: the verdict then follows `on-error`.
#[tokio::test]
async fn a_worker_check_without_a_turn_connection_cannot_run() {
    let config = config_with_worker_check("command-checker", ScenesConfig::default());
    let decisions = Decisions::build(&config, &Connections::default());
    let error = decisions
        .ask_worker("rm -rf /", "command-checker")
        .await
        .expect_err("no turn has supplied a connection");
    assert!(error.contains("no completion connection"), "{error}");
}
