//! `shuvarie-decision`: decision models over the System One protocol.
//!
//! A *decision* is a named, typed question about a state: a yes/no judgement
//! ([`DecisionKind::Noul`]), a pick from labelled alternatives
//! ([`DecisionKind::Choice`]), or a position on an ordered rubric
//! ([`DecisionKind::Score`]). It is not a generative completion — no streaming,
//! no tools, no free text — which is why it lives in its own crate rather than
//! on the LLM provider path.
//!
//! The wire protocol is TypeSafe's System One API (`POST /v1/systemone`), which
//! Ollama's Clef models and Cloudflare's Clef deployment also serve behind the
//! same request and response schema. [`DecisionClient`] speaks that one
//! protocol for every backend; a backend therefore differs only in URL and
//! credential. `rig-typesafeai`'s types stay private to this crate: callers see
//! [`Decision`] in and [`DecisionAnswer`] out.
//!
//! Probabilities are preserved, never normalized or thresholded here. In
//! particular `noul` is a probability in `[0, 1]`, *not* a boolean, and
//! `confidence` measures distribution concentration — not the chance the answer
//! is correct. Threshold policy belongs to the caller.

mod answer;
mod client;
mod decision;
mod error;

pub use answer::{DecisionAnswer, DecisionOutcome};
pub use client::{DecisionClient, PLACEHOLDER_TOKEN};
pub use decision::{
    ChoiceOption, Decision, DecisionApiType, DecisionKind, MAX_QUESTIONS, MAX_SCORE_LEVELS,
    MAX_STATE_BYTES, MIN_ALTERNATIVES, NoulCriteria, PROTOCOL_MAX_ALTERNATIVES, ScoreLevel,
};
pub use error::{DecisionError, Result};

/// Token accounting, as the provider reports it. Re-exported so a
/// [`DecisionOutcome`]'s usage is nameable without depending on `rig-core`.
pub use rig_core::completion::Usage as TokenUsage;
