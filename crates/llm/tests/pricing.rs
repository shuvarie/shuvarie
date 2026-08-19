use shuvarie_llm::{Provider, ProviderClient, TokenUsage};

fn estimate_cost(provider: Provider, usage: &TokenUsage) -> f64 {
    ProviderClient::build(provider, Some("sk-test"), None)
        .expect("build client")
        .estimate_cost(usage)
}

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
    assert_eq!(estimate_cost(Provider::OllamaCloud, &usage), 0.0);
}

#[test]
fn openai_cost_matches_rates() {
    let usage = TokenUsage {
        input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        ..TokenUsage::default()
    };
    let expected = 2.50 + 10.00;
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
        estimate_cost(Provider::Anthropic, &cached) > estimate_cost(Provider::Anthropic, &plain)
    );
}
