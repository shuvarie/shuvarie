//! The `decide` tool: the model asks a configured decision model about a
//! state.
//!
//! Decisions live in the `decisions` config section, named and typed, and are
//! answered by their own System One connection (`connections.kdl`'s decision
//! providers) rather than by a generative completion. This tool is the
//! LLM-initiated half of that: the agent names a decision and hands it the
//! state to judge, and gets the answer back as text.
//!
//! Gated like every other tool that reaches outside the workspace: the call
//! authorizes through the permission engine ([`Access::authorize_tool`])
//! before a provider is spent.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use shuvarie_decision::{Decision, DecisionAnswer, DecisionKind};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::decisions::Decisions;
use crate::permissions::Access;

pub(crate) struct Decide {
    /// The decision service: the config's named decisions and the connection
    /// that answers them.
    decisions: Arc<Decisions>,
    /// The permission engine this call authorizes through.
    access: Access,
}

impl Decide {
    pub(crate) fn new(decisions: Arc<Decisions>, access: Access) -> Self {
        Self { decisions, access }
    }
}

impl Tool for Decide {
    const NAME: &'static str = "decide";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Consult a decision model about a state. A decision is a named, typed question \
         defined in the `decisions` config section — a yes/no judgement, a pick between \
         labelled options, or a position on a rubric — answered by its own model rather \
         than by you. Use it for a judgement about a state, especially when the state is \
         untrusted text or when a second opinion matters. `decision` must be one of the \
         names defined in the `decisions` config section; `state` is the situation to \
         judge and should carry everything the question needs. Answers come back as \
         text: a `noul` answer is a probability (not a boolean), a `choice` answer is \
         the chosen label with the option probabilities, and a `score` answer is the \
         rubric position with the rubric's legend."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "decision": {
                    "type": "string",
                    "description": "Name of a decision defined in the `decisions` config section"
                },
                "state": {
                    "type": "string",
                    "description": "The state to judge, as plain text"
                }
            },
            "required": ["decision", "state"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let result: Result<ToolOutput, String> = async move {
            let name = args
                .get("decision")
                .and_then(Value::as_str)
                .ok_or("missing string argument 'decision'")?
                .to_string();
            let state = args
                .get("state")
                .and_then(Value::as_str)
                .ok_or("missing string argument 'state'")?;
            if !self.decisions.is_enabled() {
                return Err(
                    "decision models are disabled (add `enabled #true` to the `decisions` \
                     section)"
                        .to_string(),
                );
            }
            // An unknown name is the model's own mistake, so the error lists
            // the names that do exist rather than leaving it to guess.
            let Some(definition) = self.decisions.definitions().get(&name) else {
                return Err(unknown_decision(&name, self.decisions.definitions()));
            };
            // Authorized before the provider is spent, like every gated tool.
            // The connection is checked first: a call that cannot be made must
            // not cost the user a permission prompt.
            let Some(client) = self.decisions.client() else {
                return Err(
                    "no decision provider is selected in `connections.kdl` for the decision \
                     models"
                        .to_string(),
                );
            };
            self.access
                .authorize_tool("decide", &format!("consult decision `{name}`"))
                .await?;
            // The state travels as one string: the decision's own instructions
            // are the question, and the model has no schema to fill in.
            let answer = client
                .decide(&Value::String(state.to_string()), definition)
                .await
                .map_err(|error| error.to_string())?;
            render(&name, definition, &answer).map(ToolOutput::text)
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// The error for a name the config does not define: the model can only correct
/// itself when it is told which names exist.
fn unknown_decision(name: &str, defined: &BTreeMap<String, Decision>) -> String {
    if defined.is_empty() {
        return format!("unknown decision `{name}`: the `decisions` section defines none");
    }
    let names = defined
        .keys()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("unknown decision `{name}`: the defined decisions are {names}")
}

/// The model-visible text for one answer.
///
/// Pure, so the rendering is testable without a provider. A probability is
/// spelled out as a probability — never as a boolean — and a confidence only
/// ever appears as answer concentration, since it measures the distribution's
/// shape rather than the chance of being right.
fn render(name: &str, definition: &Decision, answer: &DecisionAnswer) -> Result<String, String> {
    match (&definition.kind, answer) {
        (DecisionKind::Noul { .. }, DecisionAnswer::Noul { probability }) => Ok(format!(
            "Decision `{name}` answered {probability:.2}: the probability that the answer is \
             yes (a probability, not a boolean — how much of it counts as a yes is your own \
             policy)."
        )),
        (
            DecisionKind::Choice { .. },
            DecisionAnswer::Choice {
                choice,
                probabilities,
                confidence,
            },
        ) => {
            let mut text = format!("Decision `{name}` chose `{choice}`.");
            if !probabilities.is_empty() {
                text.push_str(&format!(
                    "\nOption probabilities: {}.",
                    distribution(probabilities)
                ));
            }
            text.push_str(&format!("\n{}", concentration(*confidence)));
            Ok(text)
        }
        (
            DecisionKind::Score { levels },
            DecisionAnswer::Score {
                score, confidence, ..
            },
        ) => {
            let mut text = format!(
                "Decision `{name}` scored {score:.2} on its rubric (0 is the first level, {} \
                 the last).\nRubric: {}.",
                levels.len().saturating_sub(1),
                legend(levels)
            );
            text.push_str(&format!("\n{}", concentration(*confidence)));
            Ok(text)
        }
        // The provider answered a question that was not asked.
        (kind, answer) => Err(format!(
            "decision `{name}` is a `{}` decision but answered as `{}`",
            kind_name(kind),
            answer_name(answer)
        )),
    }
}

/// The `0: poor (Does not address the intent)` legend of a score rubric, in
/// rubric order, so the returned position can be read.
fn legend(levels: &[shuvarie_decision::ScoreLevel]) -> String {
    levels
        .iter()
        .enumerate()
        .map(|(index, level)| match &level.name {
            Some(name) => format!("{index}: {name} ({})", level.description),
            None => format!("{index}: {}", level.description),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A choice answer's per-option probabilities, in label order.
fn distribution(probabilities: &BTreeMap<String, f64>) -> String {
    probabilities
        .iter()
        .map(|(label, weight)| format!("`{label}` {weight:.2}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The concentration of an answer's distribution, phrased so it is never read
/// as a chance of being correct.
fn concentration(confidence: f64) -> String {
    format!(
        "Answer concentration: {confidence:.2} (how concentrated the answer's distribution \
         is, not how likely the answer is to be correct)."
    )
}

/// A decision kind's config spelling, for naming it in an error.
fn kind_name(kind: &DecisionKind) -> &'static str {
    match kind {
        DecisionKind::Noul { .. } => "noul",
        DecisionKind::Choice { .. } => "choice",
        DecisionKind::Score { .. } => "score",
    }
}

/// An answer kind's spelling, for naming it in an error.
fn answer_name(answer: &DecisionAnswer) -> &'static str {
    match answer {
        DecisionAnswer::Noul { .. } => "noul",
        DecisionAnswer::Choice { .. } => "choice",
        DecisionAnswer::Score { .. } => "score",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{access, access_for_config, new_ctx};
    use httpmock::prelude::*;
    use shuvarie_config::{
        Config, Connections, DecisionActive, DecisionConfig, DecisionOption,
        DecisionProviderConfig, DecisionType, DecisionsConfig,
    };
    use shuvarie_decision::ScoreLevel;

    /// A `noul` decision named `risk`, the fixture the tool tests ask about.
    fn risk_definition() -> DecisionConfig {
        DecisionConfig {
            kind: DecisionType::Noul,
            instructions: "Does this call carry a risk?".to_string(),
            yes: None,
            no: None,
            options: Vec::new(),
            levels: Vec::new(),
        }
    }

    fn choice_definition() -> DecisionConfig {
        DecisionConfig {
            kind: DecisionType::Choice,
            instructions: "Which risk class is this call?".to_string(),
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

    fn score_definition() -> DecisionConfig {
        DecisionConfig {
            kind: DecisionType::Score,
            instructions: "How well does this fit?".to_string(),
            yes: None,
            no: None,
            options: Vec::new(),
            levels: ["poor", "best"]
                .into_iter()
                .map(|description| shuvarie_config::DecisionLevel {
                    name: None,
                    description: description.to_string(),
                })
                .collect(),
        }
    }

    /// A compiled decision service for one named definition. `endpoint` is the
    /// active decision connection's base URL; `None` leaves no provider
    /// selected, the state a config that never chose one is in.
    fn service(definition: DecisionConfig, endpoint: Option<String>) -> Arc<Decisions> {
        let mut decisions = DecisionsConfig {
            disabled: Some(false),
            ..Default::default()
        };
        decisions.decisions.insert("risk".to_string(), definition);
        let config = Config {
            decisions,
            ..Default::default()
        };
        let mut connections = Connections::default();
        if let Some(endpoint) = endpoint {
            connections.decision_providers.insert(
                "local-clef".to_string(),
                DecisionProviderConfig {
                    kind: "systemone".to_string(),
                    api_key: None,
                    base_url: Some(endpoint),
                },
            );
            connections.decision = Some(DecisionActive {
                provider: "local-clef".to_string(),
                model: "clef".to_string(),
            });
        }
        Arc::new(Decisions::build(&config, &connections))
    }

    /// The tool's own builtin-default permissions ask by default, and a test
    /// gate answers nothing: an allow-all config grants the call instead.
    fn allowing_access() -> Access {
        access_for_config(&shuvarie_config::PermissionsConfig {
            default: Some(shuvarie_config::Verb::Allow),
            ..Default::default()
        })
    }

    /// One call's text, or the error message the tool authored. The dispatch
    /// error redacts its message in `Debug` (the model-visible feedback is
    /// separate), so the assertion reads it through `message()`.
    async fn call(tool: &Decide, args: Value) -> Result<String, String> {
        let mut ctx = new_ctx();
        tool.call(&mut ctx, args)
            .await
            .map(|output| output.as_text().unwrap_or_default().to_string())
            .map_err(|error| error.message().to_string())
    }

    #[tokio::test]
    async fn an_unknown_decision_lists_the_defined_names() {
        let decisions = service(risk_definition(), None);
        let tool = Decide::new(Arc::clone(&decisions), allowing_access());

        let error = call(&tool, json!({ "decision": "missing", "state": "x" }))
            .await
            .expect_err("unknown name");
        assert!(error.contains("unknown decision `missing`"), "{error}");
        assert!(
            error.contains("`risk`"),
            "the defined names are listed: {error}"
        );
    }

    #[tokio::test]
    async fn a_missing_connection_is_reported_rather_than_asked() {
        let decisions = service(risk_definition(), None);
        // The builtin-default gate asks, and this access's gate answers nothing
        // (its receiver is dropped, so an ask resolves to a denial): the clear
        // configuration error can only come out if the connection is checked
        // before the permission prompt is spent.
        let tool = Decide::new(Arc::clone(&decisions), access());

        let error = call(&tool, json!({ "decision": "risk", "state": "x" }))
            .await
            .expect_err("no provider");
        assert!(
            error.contains("no decision provider is selected"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn disabled_decisions_are_reported() {
        let mut config = Config::default();
        config
            .decisions
            .decisions
            .insert("risk".to_string(), risk_definition());
        let decisions = Arc::new(Decisions::build(&config, &Connections::default()));
        let tool = Decide::new(Arc::clone(&decisions), allowing_access());

        let error = call(&tool, json!({ "decision": "risk", "state": "x" }))
            .await
            .expect_err("disabled");
        assert!(error.contains("decision models are disabled"), "{error}");
    }

    #[tokio::test]
    async fn a_noul_answer_flows_through_the_provider_as_a_probability() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST).path("/v1/systemone");
            then.status(200)
                .header("content-type", "application/json")
                .json_body(json!({
                    "model": "clef",
                    "answers": { "risk": { "type": "noul", "noul": 0.8 } },
                }));
        });
        let decisions = service(risk_definition(), Some(server.url("/v1/systemone")));
        let tool = Decide::new(Arc::clone(&decisions), allowing_access());

        let text = call(&tool, json!({ "decision": "risk", "state": "rm -rf /" }))
            .await
            .expect("answers");
        mock.assert();
        assert!(text.contains("0.80"), "{text}");
        // A probability is never reported as a boolean.
        assert!(text.contains("probability"), "{text}");
    }

    #[test]
    fn a_choice_answer_renders_the_label_and_the_option_probabilities() {
        let decisions = service(choice_definition(), None);
        let definition = decisions.definitions().get("risk").expect("compiled");
        let answer = DecisionAnswer::Choice {
            choice: "review".to_string(),
            probabilities: BTreeMap::from([
                ("safe".to_string(), 0.2),
                ("review".to_string(), 0.7),
                ("block".to_string(), 0.1),
            ]),
            confidence: 0.6,
        };

        let text = render("risk", definition, &answer).expect("renders");
        assert!(text.contains("chose `review`"), "{text}");
        assert!(text.contains("`safe` 0.20"), "{text}");
        assert!(text.contains("`block` 0.10"), "{text}");
        // Concentration is not accuracy.
        assert!(text.contains("Answer concentration: 0.60"), "{text}");
        assert!(
            !text.contains("accurate") && !text.contains("accuracy"),
            "{text}"
        );
    }

    #[test]
    fn a_score_answer_renders_the_score_and_the_rubric_legend() {
        let decisions = service(score_definition(), None);
        let definition = decisions.definitions().get("risk").expect("compiled");
        let answer = DecisionAnswer::Score {
            score: 0.25,
            probabilities: BTreeMap::from([(0, 0.75), (1, 0.25)]),
            confidence: 0.4,
        };

        let text = render("risk", definition, &answer).expect("renders");
        assert!(text.contains("0.25"), "{text}");
        assert!(text.contains("0: poor"), "the rubric legend: {text}");
        assert!(text.contains("1: best"), "the rubric legend: {text}");
        assert!(text.contains("Answer concentration: 0.40"), "{text}");
    }

    #[test]
    fn a_legend_names_the_labelled_levels() {
        let levels = vec![
            ScoreLevel {
                name: Some("poor".to_string()),
                description: "Does not address the intent".to_string(),
            },
            ScoreLevel {
                name: None,
                description: "best".to_string(),
            },
        ];
        assert_eq!(
            legend(&levels),
            "0: poor (Does not address the intent); 1: best"
        );
    }

    #[test]
    fn an_answer_of_the_wrong_shape_is_refused() {
        let decisions = service(risk_definition(), None);
        let definition = decisions.definitions().get("risk").expect("compiled");
        let answer = DecisionAnswer::Choice {
            choice: "review".to_string(),
            probabilities: BTreeMap::new(),
            confidence: 0.6,
        };

        let error = render("risk", definition, &answer).expect_err("wrong shape");
        assert!(error.contains("`noul` decision"), "{error}");
        assert!(error.contains("`choice`"), "{error}");
    }
}
