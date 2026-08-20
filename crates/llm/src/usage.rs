use shuvarie_catalog::TokenUsage;

pub fn token_usage_from_rig(usage: rig::completion::Usage) -> TokenUsage {
    TokenUsage {
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        total_tokens: usage.total_tokens,
        cached_input_tokens: usage.cached_input_tokens,
        reasoning_tokens: usage.reasoning_tokens,
    }
}
