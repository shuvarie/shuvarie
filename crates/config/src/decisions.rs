//! `decisions { … }` — named, reusable questions for decision models.
//!
//! A decision is a typed question that a System One model answers about a
//! state: a yes/no judgement, a pick from labelled alternatives, or a position
//! on an ordered rubric. Definitions live here so several call sites — the
//! shell check, option ranking, tool gating, the `decide` tool — can name the
//! same question instead of restating it.
//!
//! Numeric and protocol bounds are enforced when the runtime compiles these
//! into requests ([`shuvarie_decision::Decision::validate`] holds the single source
//! of truth); this layer owns structure: the declared type must match the
//! children actually given, labels and descriptions must be non-empty, and
//! names must be unique.

use std::collections::BTreeMap;

/// The answer shape of a decision, from its `type` child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionType {
    /// Yes/no, answered as a probability.
    Noul,
    /// Exactly one of the declared options.
    Choice,
    /// A position on the declared rubric, lowest first.
    Score,
}

impl DecisionType {
    /// The config spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Noul => "noul",
            Self::Choice => "choice",
            Self::Score => "score",
        }
    }

    /// Parse a config spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "noul" => Some(Self::Noul),
            "choice" => Some(Self::Choice),
            "score" => Some(Self::Score),
            _ => None,
        }
    }
}

/// One `decision "name" { … }` entry.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionConfig {
    /// The declared answer shape; the children below must match it.
    pub kind: DecisionType,
    /// The instruction the model answers. Phrase it so the intended answer is
    /// explicit — a `noul` question whose high probability means "harmful"
    /// should ask about harm directly.
    pub instructions: String,
    /// `noul` only: what a high probability means.
    pub yes: Option<String>,
    /// `noul` only: what a low probability means.
    pub no: Option<String>,
    /// `choice` only.
    pub options: Vec<DecisionOption>,
    /// `score` only, lowest first.
    pub levels: Vec<DecisionLevel>,
}

/// One `option "<label>"` of a `choice` decision, with an optional
/// description.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionOption {
    pub label: String,
    /// The protocol reads a missing description as the label itself.
    pub description: Option<String>,
}

/// One `level "<name>" { description "…" }` of a `score` decision. The wire
/// carries only the description in rubric order, so the name is a local label
/// for reading a result.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionLevel {
    pub name: Option<String>,
    pub description: String,
}

/// The `decisions { … }` section: definitions by name, plus the switch that
/// turns the whole feature on.
///
/// Decision models are **off by default**: they reach a network endpoint — and
/// for the shell check, once per command the rules do not already deny — so
/// nothing is asked until the config opts in with `enabled #true`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionsConfig {
    /// `None` = unspecified (off, the default), `Some(true)` = disabled,
    /// `Some(false)` = enabled (`enabled #true` in config). Kept three-valued so
    /// a higher-priority layer that says nothing does not override a lower one
    /// that opts in.
    pub disabled: Option<bool>,
    pub decisions: BTreeMap<String, DecisionConfig>,
}

impl DecisionsConfig {
    /// One definition by name.
    pub fn get(&self, name: &str) -> Option<&DecisionConfig> {
        self.decisions.get(name)
    }

    /// Whether decision models may be asked. Unspecified means off, so a config
    /// that only defines decisions but never enables them asks nothing.
    pub fn is_enabled(&self) -> bool {
        !self.disabled.unwrap_or(true)
    }

    /// Whether the section carries anything worth writing back: neither a
    /// decision nor the switch itself.
    pub fn is_empty(&self) -> bool {
        self.disabled.is_none() && self.decisions.is_empty()
    }

    /// Merge a lower-priority layer under this one: entries here win by name,
    /// a name only the other layer defines is added, and the switch comes from
    /// the highest layer that sets it.
    pub fn stack(&mut self, other: Self) {
        if self.disabled.is_none() {
            self.disabled = other.disabled;
        }
        for (name, decision) in other.decisions {
            self.decisions.entry(name).or_insert(decision);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_type_parses_its_spellings() {
        assert_eq!(DecisionType::parse("noul"), Some(DecisionType::Noul));
        assert_eq!(DecisionType::parse("score"), Some(DecisionType::Score));
        assert_eq!(DecisionType::parse("choice"), Some(DecisionType::Choice));
        assert_eq!(DecisionType::parse("ranking"), None);
        assert_eq!(DecisionType::Noul.as_str(), "noul");
    }

    #[test]
    fn the_switch_comes_from_the_highest_layer_that_sets_it() {
        let enabled = DecisionsConfig {
            disabled: Some(false),
            ..Default::default()
        };
        let disabled = DecisionsConfig {
            disabled: Some(true),
            ..Default::default()
        };

        // Off unless something opts in.
        assert!(!DecisionsConfig::default().is_enabled());

        // A layer that says nothing leaves the lower layer's opt-in standing.
        let mut high = DecisionsConfig::default();
        high.stack(enabled.clone());
        assert!(high.is_enabled());

        // An explicit switch wins over a lower layer.
        let mut high = disabled;
        high.stack(enabled);
        assert!(!high.is_enabled());
    }

    #[test]
    fn stacking_keeps_the_higher_priority_definition() {
        let decision = |instructions: &str| DecisionConfig {
            kind: DecisionType::Noul,
            instructions: instructions.to_string(),
            yes: None,
            no: None,
            options: Vec::new(),
            levels: Vec::new(),
        };
        let mut high = DecisionsConfig::default();
        high.decisions.insert("shared".into(), decision("high"));
        high.decisions.insert("only-high".into(), decision("high"));
        let mut low = DecisionsConfig::default();
        low.decisions.insert("shared".into(), decision("low"));
        low.decisions.insert("only-low".into(), decision("low"));

        high.stack(low);
        assert_eq!(high.decisions.len(), 3);
        assert_eq!(
            high.get("shared").map(|entry| entry.instructions.as_str()),
            Some("high")
        );
        assert_eq!(
            high.get("only-low")
                .map(|entry| entry.instructions.as_str()),
            Some("low")
        );
    }
}
