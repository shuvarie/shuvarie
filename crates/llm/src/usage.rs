pub use rig::completion::Usage as TokenUsage;

/// The approximate context footprint of one completed provider request: the
/// tokens the provider processed for that exchange (the full prompt including
/// cache reads/writes, plus the completion). This is what a request leaves
/// behind in the conversation, so the latest main-stream request's footprint
/// is the best measure of current context occupancy.
///
/// Uses the provider-reported `total_tokens` when non-zero. Falls back to the
/// component sum, which over-counts cached input on providers that fold it
/// into `input_tokens` — a conservative (over-)estimate for a pressure
/// display.
pub fn context_footprint(usage: &TokenUsage) -> u64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage
            .input_tokens
            .saturating_add(usage.output_tokens)
            .saturating_add(usage.cached_input_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_prefers_reported_total() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 200,
            total_tokens: 1_500,
            cached_input_tokens: 300,
            cache_creation_input_tokens: 0,
            ..TokenUsage::default()
        };
        assert_eq!(context_footprint(&usage), 1_500);
    }

    #[test]
    fn footprint_sums_components_without_total() {
        let usage = TokenUsage {
            input_tokens: 1_000,
            output_tokens: 200,
            total_tokens: 0,
            cached_input_tokens: 300,
            cache_creation_input_tokens: 50,
            ..TokenUsage::default()
        };
        assert_eq!(context_footprint(&usage), 1_550);
    }

    #[test]
    fn footprint_of_empty_usage_is_zero() {
        assert_eq!(context_footprint(&TokenUsage::default()), 0);
    }
}
