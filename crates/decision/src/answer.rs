//! What came back: the answer to a decision, with its distribution preserved.

use std::collections::BTreeMap;

use rig_typesafeai::types::Answer;

use crate::TokenUsage;
use crate::error::{DecisionError, Result};

/// A decision's answer, with its distribution preserved.
#[derive(Debug, Clone, PartialEq)]
pub enum DecisionAnswer {
    /// The probability the answer is yes, in `[0, 1]`. A threshold is the
    /// caller's policy, not the protocol's.
    Noul { probability: f64 },
    /// The most probable alternative and the full distribution over labels.
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    /// The probability-weighted rubric position (zero-based, fractional, not
    /// rounded to a level), the distribution over level indices, and the
    /// distribution's concentration.
    Score {
        score: f64,
        probabilities: BTreeMap<usize, f64>,
        confidence: f64,
    },
}

impl DecisionAnswer {
    /// The `noul` probability, when this is a yes/no answer.
    pub fn probability(&self) -> Option<f64> {
        match self {
            Self::Noul { probability } => Some(*probability),
            _ => None,
        }
    }

    /// The rubric position, when this is a score answer. Lower is earlier on
    /// the rubric, so ranking ascending puts the best-fitting first when the
    /// rubric runs worst to best.
    pub fn score(&self) -> Option<f64> {
        match self {
            Self::Score { score, .. } => Some(*score),
            _ => None,
        }
    }

    /// The selected label, when this is a choice answer.
    pub fn choice(&self) -> Option<&str> {
        match self {
            Self::Choice { choice, .. } => Some(choice),
            _ => None,
        }
    }

    /// Map one wire answer onto its crate-local form.
    pub(crate) fn from_wire(name: &str, answer: Answer) -> Result<Self> {
        let response = |message: String| DecisionError::Invalid {
            name: name.to_string(),
            message,
        };
        match answer {
            Answer::Noul { noul } => Ok(Self::Noul { probability: noul }),
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => Ok(Self::Choice {
                choice,
                probabilities,
                confidence,
            }),
            Answer::Score {
                score,
                probabilities,
                confidence,
                ..
            } => Ok(Self::Score {
                score,
                probabilities: probabilities
                    .into_iter()
                    .map(|(index, weight)| {
                        index
                            .parse::<usize>()
                            .map(|index| (index, weight))
                            .map_err(|_| response(format!("invalid score index `{index}`")))
                    })
                    .collect::<Result<_>>()?,
                confidence,
            }),
        }
    }
}

/// The answers to one request, with the provider metadata that came back with
/// them.
#[derive(Debug, Clone)]
pub struct DecisionOutcome {
    /// Answers keyed by decision name. The map orders by name, not by the
    /// order the questions were defined in.
    pub answers: BTreeMap<String, DecisionAnswer>,
    /// The model identifier the provider reported.
    pub model: String,
    /// Token accounting, when the provider reports it.
    pub usage: Option<TokenUsage>,
    /// The transport request id, for diagnostics.
    pub request_id: Option<String>,
}

impl DecisionOutcome {
    /// One answer by decision name.
    pub fn answer(&self, name: &str) -> Option<&DecisionAnswer> {
        self.answers.get(name)
    }
}
