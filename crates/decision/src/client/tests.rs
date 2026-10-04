use super::*;
use httpmock::prelude::*;
use serde_json::json;

fn system_one(label: &str, model: &str, endpoint: String, key: Option<&str>) -> DecisionClient {
    DecisionClient::build(DecisionApiType::SystemOne, label, model, endpoint, key)
        .expect("client builds")
}

#[test]
fn a_host_root_gains_the_protocol_path() {
    assert_eq!(
        system_one_endpoint("http://localhost:11434"),
        "http://localhost:11434/v1/systemone"
    );
    assert_eq!(
        system_one_endpoint("http://localhost:11434/"),
        "http://localhost:11434/v1/systemone"
    );
}

#[test]
fn a_url_with_a_path_is_used_as_written() {
    assert_eq!(
        system_one_endpoint("https://api.typesafe.ai/v1/systemone"),
        "https://api.typesafe.ai/v1/systemone"
    );
    // Cloudflare's Workers AI route is not a `/v1/systemone` suffix.
    assert_eq!(
        system_one_endpoint(
            "https://api.cloudflare.com/client/v4/accounts/abc/ai/run/@cf/cloudflare/clef"
        ),
        "https://api.cloudflare.com/client/v4/accounts/abc/ai/run/@cf/cloudflare/clef"
    );
}

#[test]
fn a_missing_credential_sends_the_placeholder() {
    let client = system_one("local", "clef", "http://localhost:11434".into(), None);
    assert_eq!(client.token, PLACEHOLDER_TOKEN);
    assert!(!client.authenticated);

    let client = system_one(
        "hosted",
        "jev-latest",
        "https://api.typesafe.ai".into(),
        Some("  "),
    );
    assert_eq!(client.token, PLACEHOLDER_TOKEN);

    let client = system_one(
        "hosted",
        "jev-latest",
        "https://api.typesafe.ai".into(),
        Some("key"),
    );
    assert_eq!(client.token, "key");
    assert!(client.authenticated);
}

#[test]
fn an_empty_model_or_endpoint_is_refused() {
    let error = DecisionClient::build(
        DecisionApiType::SystemOne,
        "local",
        "  ",
        "http://localhost:11434",
        None,
    )
    .expect_err("no model");
    assert!(error.to_string().contains("no model is selected"));

    let error = DecisionClient::build(DecisionApiType::SystemOne, "local", "clef", "  ", None)
        .expect_err("no endpoint");
    assert!(error.to_string().contains("no endpoint is configured"));
}

#[tokio::test]
async fn evaluate_sends_one_system_one_request_and_reads_the_answer() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/v1/systemone")
            .header("authorization", format!("Bearer {PLACEHOLDER_TOKEN}"))
            .json_body(json!({
                "model": "clef",
                "state": "rm -rf /",
                "questions": {
                    "shell-safety": {
                        "type": "noul",
                        "instructions": "Is this command harmful?",
                    }
                }
            }));
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "model": "clef",
                "answers": { "shell-safety": { "type": "noul", "noul": 0.97 } },
                "usage": { "input_tokens": 12, "output_tokens": 3, "total_tokens": 15 },
            }));
    });

    let client = system_one("local", "clef", server.url("/v1/systemone"), None);
    let decision = Decision::noul("shell-safety", "Is this command harmful?");
    let outcome = client
        .evaluate(&json!("rm -rf /"), std::slice::from_ref(&decision))
        .await
        .expect("evaluates");

    mock.assert();
    assert_eq!(outcome.model, "clef");
    assert_eq!(
        outcome.answer("shell-safety"),
        Some(&DecisionAnswer::Noul { probability: 0.97 })
    );
    assert_eq!(outcome.usage.and_then(|usage| usage.total_tokens), Some(15));
}

#[tokio::test]
async fn evaluate_reads_a_score_distribution_onto_level_indices() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(200)
            .header("content-type", "application/json")
            .json_body(json!({
                "model": "clef",
                "answers": {
                    "fit": {
                        "type": "score",
                        "score": 0.25,
                        "probabilities": { "0": 0.75, "1": 0.25 },
                        "legend": { "0": "poor", "1": "best" },
                        "confidence": 0.4
                    }
                },
            }));
    });

    let client = system_one("local", "clef", server.url("/v1/systemone"), None);
    let decision = Decision::score("fit", "How well does this fit?", ["poor", "best"]);
    let outcome = client
        .evaluate(&json!({ "option": "a" }), std::slice::from_ref(&decision))
        .await
        .expect("evaluates");

    let Some(DecisionAnswer::Score {
        score,
        probabilities,
        confidence,
    }) = outcome.answer("fit")
    else {
        panic!("expected a score answer");
    };
    assert_eq!(*score, 0.25);
    assert_eq!(probabilities.get(&0), Some(&0.75));
    assert_eq!(probabilities.get(&1), Some(&0.25));
    assert_eq!(*confidence, 0.4);
}

#[tokio::test]
async fn a_rejected_request_carries_the_provider_and_its_reply() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(400)
            .header("content-type", "application/json")
            .body(r#"{"error":"cloud models are not supported"}"#);
    });

    let client = system_one(
        "local-clef",
        "gpt-oss:120b",
        server.url("/v1/systemone"),
        None,
    );
    let decision = Decision::noul("harm", "Is this harmful?");
    let error = client
        .evaluate(&json!("ls"), std::slice::from_ref(&decision))
        .await
        .expect_err("the provider refuses");

    let message = error.to_string();
    // The configured connection leads, not rig's hardcoded `typesafeai`.
    assert!(message.contains("`local-clef`"), "{message}");
    assert!(
        message.contains("cloud models are not supported"),
        "{message}"
    );
}

#[tokio::test]
async fn an_authentication_failure_without_a_key_says_so() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/v1/systemone");
        then.status(401).body("unauthorized");
    });

    let client = system_one("hosted", "jev-latest", server.url("/v1/systemone"), None);
    let decision = Decision::noul("harm", "Is this harmful?");
    let error = client
        .evaluate(&json!("ls"), std::slice::from_ref(&decision))
        .await
        .expect_err("unauthorized");

    assert!(
        error
            .to_string()
            .contains("no api-key is configured for this decision provider"),
        "{error}"
    );
}

#[tokio::test]
async fn an_oversized_state_is_refused_rather_than_truncated() {
    let server = MockServer::start();
    let client = system_one("local", "clef", server.url("/v1/systemone"), None);
    let decision = Decision::noul("harm", "Is this harmful?");
    let state = json!("x".repeat(MAX_STATE_BYTES));

    let error = client
        .evaluate(&state, std::slice::from_ref(&decision))
        .await
        .expect_err("over the budget");
    assert!(matches!(
        error,
        DecisionError::StateTooLarge { limit, .. } if limit == MAX_STATE_BYTES
    ));
}

#[tokio::test]
async fn duplicate_decision_names_in_one_request_are_refused() {
    let server = MockServer::start();
    let client = system_one("local", "clef", server.url("/v1/systemone"), None);
    let decisions = [
        Decision::noul("harm", "Is this harmful?"),
        Decision::noul("harm", "Is this harmful, really?"),
    ];

    let error = client
        .evaluate(&json!("ls"), &decisions)
        .await
        .expect_err("duplicate ids");
    assert!(error.to_string().contains("duplicate decision name"));
}

#[test]
fn a_long_provider_body_is_truncated_on_a_character_boundary() {
    let text = "é".repeat(600);
    let cut = truncate(&text, 512);
    assert!(cut.ends_with('…'));
    assert_eq!(cut.chars().count(), 512 / 2 + 1);
}
