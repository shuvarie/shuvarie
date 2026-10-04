//! `ranking { … }` — which `decisions` entry ranks the options a `question`
//! asks about.
//!
//! Ranking is cosmetic: it only reorders the alternatives a question offers,
//! so a named decision that is missing, of the wrong kind, or unreachable
//! leaves the options exactly as the tool asked them — nothing about ranking
//! is allowed to change or block a question.

/// A `ranking { decision "name" }` block.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RankingConfig {
    /// `None` = unspecified.
    pub decision: Option<String>,
    /// `disabled #true` turns ranking off.
    pub disabled: bool,
}

impl RankingConfig {
    /// Whether the block carries anything worth writing back.
    pub fn is_empty(&self) -> bool {
        self.decision.is_none() && !self.disabled
    }

    /// Merge a lower-priority layer under this one (highest explicit wins).
    pub fn stack(&mut self, other: Self) {
        if self.decision.is_none() {
            self.decision = other.decision;
        }
        self.disabled |= other.disabled;
    }

    /// The decision to rank with: a scene that names one wins, a scene that is
    /// `disabled` turns ranking off, otherwise the global block decides (and a
    /// global that is `disabled` leaves it off). A scene that both names one
    /// and is `disabled` is off: the switch is the last word.
    pub fn resolve<'a>(global: &'a Self, scene: Option<&'a Self>) -> Option<&'a str> {
        if let Some(scene) = scene {
            if scene.disabled {
                return None;
            }
            if scene.decision.is_some() {
                return scene.decision.as_deref();
            }
        }
        if global.disabled {
            return None;
        }
        global.decision.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block naming one decision.
    fn named(decision: &str) -> RankingConfig {
        RankingConfig {
            decision: Some(decision.to_string()),
            disabled: false,
        }
    }

    /// A block that turns ranking off.
    fn off() -> RankingConfig {
        RankingConfig {
            decision: None,
            disabled: true,
        }
    }

    #[test]
    fn the_global_block_decides_when_no_scene_says_anything() {
        let global = named("fit");
        assert_eq!(RankingConfig::resolve(&global, None), Some("fit"));

        // A scene that is silent leaves the global block standing.
        let silent = RankingConfig::default();
        assert_eq!(RankingConfig::resolve(&global, Some(&silent)), Some("fit"));
    }

    #[test]
    fn a_scene_that_names_one_overrides_the_global_block() {
        let global = named("fit");
        let scene = named("strict-fit");
        assert_eq!(
            RankingConfig::resolve(&global, Some(&scene)),
            Some("strict-fit")
        );
    }

    #[test]
    fn a_disabled_scene_beats_a_global_decision() {
        let global = named("fit");
        assert_eq!(RankingConfig::resolve(&global, Some(&off())), None);

        // The switch is the last word, even next to a scene-level name.
        let both = RankingConfig {
            decision: Some("strict-fit".to_string()),
            disabled: true,
        };
        assert_eq!(RankingConfig::resolve(&global, Some(&both)), None);
    }

    #[test]
    fn nothing_set_ranks_nothing() {
        let nothing = RankingConfig::default();
        let silent = RankingConfig::default();
        assert_eq!(RankingConfig::resolve(&nothing, None), None);
        assert_eq!(RankingConfig::resolve(&nothing, Some(&silent)), None);

        // A global block that is `disabled` stays off.
        let refused = RankingConfig {
            decision: Some("fit".to_string()),
            disabled: true,
        };
        assert_eq!(RankingConfig::resolve(&refused, None), None);

        assert!(RankingConfig::default().is_empty());
        assert!(!refused.is_empty());
    }

    #[test]
    fn stacking_keeps_the_higher_priority_name_and_any_off_switch() {
        let mut high = named("high");
        high.stack(named("low"));
        assert_eq!(high.decision.as_deref(), Some("high"));

        // A lower layer that says nothing leaves the name standing.
        let mut high = RankingConfig::default();
        high.stack(named("low"));
        assert_eq!(high.decision.as_deref(), Some("low"));

        // Off anywhere in the chain is off.
        let mut high = named("high");
        high.stack(off());
        assert!(high.disabled);
    }
}
