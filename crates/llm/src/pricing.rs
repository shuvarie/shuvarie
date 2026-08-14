use crate::Provider;
use crate::usage::TokenUsage;

const OPENAI_INPUT: f64 = 2.50;
const OPENAI_OUTPUT: f64 = 10.00;
const ANTHROPIC_INPUT: f64 = 3.00;
const ANTHROPIC_OUTPUT: f64 = 15.00;
const GEMINI_INPUT: f64 = 1.25;
const GEMINI_OUTPUT: f64 = 5.00;
const CACHE_READ_FACTOR: f64 = 0.1;
const REASONING_FACTOR: f64 = 0.6;

fn rate(provider: Provider) -> (f64, f64) {
    match provider {
        Provider::OpenAiCompatible
        | Provider::OpenRouter
        | Provider::Groq
        | Provider::Together
        | Provider::DeepSeek => (OPENAI_INPUT, OPENAI_OUTPUT),
        Provider::Anthropic => (ANTHROPIC_INPUT, ANTHROPIC_OUTPUT),
        Provider::Gemini => (GEMINI_INPUT, GEMINI_OUTPUT),
        Provider::Ollama => (0.0, 0.0),
    }
}

pub fn estimate_cost(provider: Provider, usage: &TokenUsage) -> f64 {
    let (input_rate, output_rate) = rate(provider);
    let input = usage.input_tokens as f64 / 1e6 * input_rate;
    let cached = usage.cached_input_tokens as f64 / 1e6 * input_rate * CACHE_READ_FACTOR;
    let output = usage.output_tokens as f64 / 1e6 * output_rate;
    let reasoning = usage.reasoning_tokens as f64 / 1e6 * output_rate * REASONING_FACTOR;
    input + cached + output + reasoning
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_usage_is_free() {
        let usage = TokenUsage::default();
        for kind in Provider::ALL {
            assert_eq!(estimate_cost(kind, &usage), 0.0);
        }
    }

    #[test]
    fn ollama_is_free() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 2000,
            ..TokenUsage::default()
        };
        assert_eq!(estimate_cost(Provider::Ollama, &usage), 0.0);
    }

    #[test]
    fn openai_cost_matches_rates() {
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..TokenUsage::default()
        };
        let expected = OPENAI_INPUT + OPENAI_OUTPUT;
        assert!((estimate_cost(Provider::OpenAiCompatible, &usage) - expected).abs() < 1e-9);
    }

    #[test]
    fn cached_tokens_are_cheaper() {
        let plain = TokenUsage {
            input_tokens: 1_000_000,
            ..TokenUsage::default()
        };
        let cached = TokenUsage {
            input_tokens: 1_000_000,
            cached_input_tokens: 1_000_000,
            ..TokenUsage::default()
        };
        assert!(
            estimate_cost(Provider::Anthropic, &cached)
                > estimate_cost(Provider::Anthropic, &plain)
        );
    }
}
