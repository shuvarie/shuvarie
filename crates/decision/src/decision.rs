//! What to ask: the named decision, its kind, and the protocol's limits.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::error::{DecisionError, Result};

/// The System One request budget: one to 64 questions.
pub const MAX_QUESTIONS: usize = 64;

/// Choice and score rubrics both need at least two alternatives.
pub const MIN_ALTERNATIVES: usize = 2;

/// The protocol's own ceiling for choice labels and score levels.
pub const PROTOCOL_MAX_ALTERNATIVES: usize = 26;

/// The score-level ceiling this backend actually enforces. `rig-typesafeai`'s
/// definition validator caps a score rubric at ten levels while the protocol
/// allows twenty-six, so a rubric above ten is refused locally with a message
/// that names both numbers rather than surfacing the wire's anonymous error.
pub const MAX_SCORE_LEVELS: usize = 10;

/// The largest request body the protocol accepts without images (64 KiB).
pub const MAX_STATE_BYTES: usize = 64 * 1024;

/// The wire protocol a decision connection speaks. The dialog and config always
/// write `systemone`; the type exists so a future protocol is a new variant
/// rather than a reinterpretation of the stored string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DecisionApiType {
    /// TypeSafe's System One API, also served by Ollama and Cloudflare.
    #[default]
    SystemOne,
}

impl DecisionApiType {
    /// The config spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SystemOne => "systemone",
        }
    }

    /// Parse a config spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "systemone" => Some(Self::SystemOne),
            _ => None,
        }
    }
}

/// The two descriptions a `noul` question may carry. Omitting them leaves the
/// protocol's own No/Yes wording in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoulCriteria {
    /// What a high probability means.
    pub yes: String,
    /// What a low probability means.
    pub no: String,
}

/// One labelled alternative of a [`DecisionKind::Choice`], with an optional
/// description. The protocol reads a missing description as the label itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub label: String,
    pub description: Option<String>,
}

/// One level of a [`DecisionKind::Score`] rubric. The wire carries only the
/// description, ordered lowest first, so `name` (when set) is a local label for
/// reading the result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreLevel {
    pub name: Option<String>,
    pub description: String,
}

/// The shape of a decision's answer. The wire form is `Decision::question`,
/// which stays crate-private along with the `rig-typesafeai` type it returns.
/// It is not a serde encoding of this type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionKind {
    /// Yes/no, answered as a probability that the answer is yes.
    Noul { criteria: Option<NoulCriteria> },
    /// Exactly one of two to 26 labelled alternatives.
    Choice { options: Vec<ChoiceOption> },
    /// A position on an ordered rubric of two to [`MAX_SCORE_LEVELS`] levels,
    /// lowest first.
    Score { levels: Vec<ScoreLevel> },
}

/// A named, typed question about a state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The question id. It keys the answer and so must be unique within one
    /// request and non-empty.
    pub name: String,
    /// The instruction the model answers. Phrase it so the intended answer is
    /// explicit: a `noul` question whose high probability means "harmful"
    /// should ask about harm directly.
    pub instructions: String,
    pub kind: DecisionKind,
}

impl Decision {
    /// A yes/no decision.
    pub fn noul(name: impl Into<String>, instructions: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: DecisionKind::Noul { criteria: None },
        }
    }

    /// A decision over labelled alternatives.
    pub fn choice(
        name: impl Into<String>,
        instructions: impl Into<String>,
        options: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: DecisionKind::Choice {
                options: options
                    .into_iter()
                    .map(|label| ChoiceOption {
                        label: label.into(),
                        description: None,
                    })
                    .collect(),
            },
        }
    }

    /// A decision over an ordered rubric, lowest first.
    pub fn score(
        name: impl Into<String>,
        instructions: impl Into<String>,
        levels: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            name: name.into(),
            instructions: instructions.into(),
            kind: DecisionKind::Score {
                levels: levels
                    .into_iter()
                    .map(|description| ScoreLevel {
                        name: None,
                        description: description.into(),
                    })
                    .collect(),
            },
        }
    }

    /// Validate against the System One protocol before a request is built, so a
    /// malformed definition reports its own decision name instead of the wire's
    /// anonymous "request error".
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(self.invalid("name must not be empty"));
        }
        if self.instructions.trim().is_empty() {
            return Err(self.invalid("instructions must not be empty"));
        }
        match &self.kind {
            DecisionKind::Noul { criteria } => {
                if let Some(criteria) = criteria
                    && (criteria.yes.trim().is_empty() || criteria.no.trim().is_empty())
                {
                    return Err(self.invalid("noul criteria descriptions must not be empty"));
                }
            }
            DecisionKind::Choice { options } => {
                if !(MIN_ALTERNATIVES..=PROTOCOL_MAX_ALTERNATIVES).contains(&options.len()) {
                    return Err(self.invalid(format!(
                        "choice requires {MIN_ALTERNATIVES} to {PROTOCOL_MAX_ALTERNATIVES} \
                         options, found {}",
                        options.len()
                    )));
                }
                let mut seen = BTreeSet::new();
                for option in options {
                    if option.label.trim().is_empty() {
                        return Err(self.invalid("choice option labels must not be empty"));
                    }
                    if !seen.insert(option.label.as_str()) {
                        return Err(
                            self.invalid(format!("duplicate choice option `{}`", option.label))
                        );
                    }
                }
            }
            DecisionKind::Score { levels } => {
                if levels.len() < MIN_ALTERNATIVES {
                    return Err(self.invalid(format!(
                        "score requires at least {MIN_ALTERNATIVES} levels, found {}",
                        levels.len()
                    )));
                }
                if levels.len() > MAX_SCORE_LEVELS {
                    return Err(self.invalid(format!(
                        "score supports at most {MAX_SCORE_LEVELS} levels with this backend, \
                         found {} (the protocol allows {PROTOCOL_MAX_ALTERNATIVES})",
                        levels.len()
                    )));
                }
                for level in levels {
                    if level.description.trim().is_empty() {
                        return Err(self.invalid("score level descriptions must not be empty"));
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) fn invalid(&self, message: impl Into<String>) -> DecisionError {
        DecisionError::Invalid {
            name: self.name.clone(),
            message: message.into(),
        }
    }

    /// The protocol's representation of this decision.
    pub(crate) fn question(&self) -> rig_typesafeai::types::Question {
        let instructions = Value::String(self.instructions.clone());
        match &self.kind {
            DecisionKind::Noul { criteria } => rig_typesafeai::types::Question::Noul {
                instructions,
                criteria: criteria.as_ref().map(|criteria| {
                    BTreeMap::from([
                        ("false".to_string(), Value::String(criteria.no.clone())),
                        ("true".to_string(), Value::String(criteria.yes.clone())),
                    ])
                }),
            },
            DecisionKind::Choice { options } => rig_typesafeai::types::Question::Choice {
                instructions,
                criteria: options
                    .iter()
                    .map(|option| {
                        (
                            option.label.clone(),
                            option.description.clone().map(Value::String),
                        )
                    })
                    .collect(),
            },
            DecisionKind::Score { levels } => rig_typesafeai::types::Question::Score {
                instructions,
                criteria: levels
                    .iter()
                    .map(|level| Value::String(level.description.clone()))
                    .collect(),
            },
        }
    }
}

#[cfg(test)]
mod tests;
