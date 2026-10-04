use super::*;
use serde_json::json;

#[test]
fn api_type_parses_its_config_spelling() {
    assert_eq!(
        DecisionApiType::parse("systemone"),
        Some(DecisionApiType::SystemOne)
    );
    assert_eq!(
        DecisionApiType::parse(" systemone "),
        Some(DecisionApiType::SystemOne)
    );
    assert_eq!(DecisionApiType::parse("jev"), None);
    assert_eq!(DecisionApiType::SystemOne.as_str(), "systemone");
}

#[test]
fn a_noul_question_omits_its_criteria_unless_given() {
    let decision = Decision::noul("harm", "Could this tool call cause harm?");
    assert_eq!(
        serde_json::to_value(decision.question()).expect("serializes"),
        json!({
            "type": "noul",
            "instructions": "Could this tool call cause harm?",
        })
    );
    let decision = Decision {
        kind: DecisionKind::Noul {
            criteria: Some(NoulCriteria {
                yes: "harmful".into(),
                no: "benign".into(),
            }),
        },
        ..decision
    };
    assert_eq!(
        serde_json::to_value(decision.question()).expect("serializes"),
        json!({
            "type": "noul",
            "instructions": "Could this tool call cause harm?",
            "criteria": { "false": "benign", "true": "harmful" },
        })
    );
}

#[test]
fn a_choice_question_encodes_labelled_alternatives() {
    let decision = Decision {
        kind: DecisionKind::Choice {
            options: vec![
                ChoiceOption {
                    label: "safe".into(),
                    // A missing description reads as the label itself.
                    description: None,
                },
                ChoiceOption {
                    label: "block".into(),
                    description: Some("Must not run unattended".into()),
                },
            ],
        },
        ..Decision::choice("risk", "Which risk class is this?", Vec::<String>::new())
    };
    assert_eq!(
        serde_json::to_value(decision.question()).expect("serializes"),
        json!({
            "type": "choice",
            "instructions": "Which risk class is this?",
            "criteria": { "safe": null, "block": "Must not run unattended" },
        })
    );
}

#[test]
fn a_score_question_encodes_an_ordered_description_array() {
    let decision = Decision::score("fit", "How well does this fit?", ["poor", "fair", "best"]);
    assert_eq!(
        serde_json::to_value(decision.question()).expect("serializes"),
        json!({
            "type": "score",
            "instructions": "How well does this fit?",
            // Positional and lowest-first: the wire carries no level names.
            "criteria": ["poor", "fair", "best"],
        })
    );
}

#[test]
fn choice_bounds_are_checked_locally() {
    let one = Decision::choice("risk", "Which?", ["only"]);
    assert!(matches!(
        one.validate(),
        Err(DecisionError::Invalid { ref name, .. }) if name == "risk"
    ));

    let duplicate = Decision::choice("risk", "Which?", ["safe", "safe"]);
    let error = duplicate.validate().expect_err("duplicates are refused");
    assert!(error.to_string().contains("duplicate choice option `safe`"));

    let most = Decision::choice(
        "risk",
        "Which?",
        (0..PROTOCOL_MAX_ALTERNATIVES).map(|i| format!("option-{i}")),
    );
    assert!(most.validate().is_ok());

    let too_many = Decision::choice(
        "risk",
        "Which?",
        (0..=PROTOCOL_MAX_ALTERNATIVES).map(|i| format!("option-{i}")),
    );
    assert!(too_many.validate().is_err());
}

#[test]
fn a_score_rubric_over_the_backend_ceiling_names_both_limits() {
    let decision = Decision::score(
        "fit",
        "How well does this fit?",
        (0..MAX_SCORE_LEVELS + 1).map(|i| format!("level-{i}")),
    );
    let error = decision.validate().expect_err("over the ceiling");
    let message = error.to_string();
    assert!(message.contains(&MAX_SCORE_LEVELS.to_string()), "{message}");
    assert!(
        message.contains(&PROTOCOL_MAX_ALTERNATIVES.to_string()),
        "{message}"
    );
}

#[test]
fn empty_instructions_and_names_are_refused() {
    let decision = Decision::noul("", "Which?");
    assert!(decision.validate().is_err());
    let decision = Decision::noul("harm", "   ");
    assert!(decision.validate().is_err());
}
