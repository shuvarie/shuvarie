use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

use crate::model::ModelInfo;
use crate::provider::Provider;
use crate::usage::TokenUsage;

const MODELS_TOML: &str = include_str!("../data/models.toml");
const PROVIDER_TOMLS: &[(&str, &str)] = &[
    (
        "open_ai_compatible",
        include_str!("../data/providers/openai_compatible.toml"),
    ),
    (
        "open_router",
        include_str!("../data/providers/openrouter.toml"),
    ),
    ("groq", include_str!("../data/providers/groq.toml")),
    ("together", include_str!("../data/providers/together.toml")),
    ("deep_seek", include_str!("../data/providers/deepseek.toml")),
    (
        "anthropic",
        include_str!("../data/providers/anthropic.toml"),
    ),
    ("gemini", include_str!("../data/providers/gemini.toml")),
    ("ollama", include_str!("../data/providers/ollama.toml")),
    (
        "ollama_cloud",
        include_str!("../data/providers/ollama_cloud.toml"),
    ),
];

const CACHE_READ_FACTOR: f64 = 0.1;
const REASONING_FACTOR: f64 = 0.6;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelEntry {
    pub context_length: u64,
    pub rates: Rates,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub id: String,
    pub kind: String,
    pub model: String,
}

#[derive(Debug)]
struct CatalogData {
    models: HashMap<String, ModelEntry>,
    providers: HashMap<Provider, ProviderCatalog>,
}

#[derive(Debug)]
struct ProviderCatalog {
    fallback_rates: Rates,
    aliases: HashMap<String, Alias>,
    variants: Vec<Variant>,
    prefix_keys: Vec<String>,
}

#[derive(Debug)]
struct Alias {
    model: String,
    context_length: Option<u64>,
    rates: Option<Rates>,
}

#[derive(Debug, Deserialize)]
struct ModelsToml {
    #[serde(default)]
    models: HashMap<String, ModelToml>,
}

#[derive(Debug, Deserialize)]
struct ModelToml {
    context_length: Option<u64>,
    input_per_mtok: Option<f64>,
    output_per_mtok: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ProviderToml {
    input_per_mtok: Option<f64>,
    output_per_mtok: Option<f64>,
    #[serde(default)]
    models: Vec<ProviderModelToml>,
}

#[derive(Debug, Deserialize)]
struct ProviderModelToml {
    model: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    variants: Vec<ProviderVariantToml>,
    context_length: Option<u64>,
    input_per_mtok: Option<f64>,
    output_per_mtok: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ProviderVariantToml {
    kind: String,
    id: String,
    context_length: Option<u64>,
    input_per_mtok: Option<f64>,
    output_per_mtok: Option<f64>,
}

#[derive(Debug, Default)]
struct ProviderBuilder {
    aliases: HashMap<String, Alias>,
    variants: Vec<Variant>,
}

fn provider_key(provider: Provider) -> &'static str {
    match provider {
        Provider::OpenAiCompatible => "open_ai_compatible",
        Provider::OpenRouter => "open_router",
        Provider::Groq => "groq",
        Provider::Together => "together",
        Provider::DeepSeek => "deep_seek",
        Provider::Anthropic => "anthropic",
        Provider::Gemini => "gemini",
        Provider::Ollama => "ollama",
        Provider::OllamaCloud => "ollama_cloud",
    }
}

fn provider_from_key(key: &str) -> Option<Provider> {
    Provider::ALL
        .iter()
        .copied()
        .find(|p| provider_key(*p) == key)
}

fn build_data() -> CatalogData {
    let raw_models: ModelsToml =
        toml::from_str(MODELS_TOML).expect("shuvarie-catalog: data/models.toml must parse");
    let models = raw_models
        .models
        .into_iter()
        .filter_map(|(id, m)| {
            let context_length = m.context_length?;
            let rates = Rates {
                input_per_mtok: m.input_per_mtok?,
                output_per_mtok: m.output_per_mtok?,
            };
            Some((
                id,
                ModelEntry {
                    context_length,
                    rates,
                },
            ))
        })
        .collect();

    let providers = PROVIDER_TOMLS
        .iter()
        .map(|(name, contents)| {
            let provider = provider_from_key(name)
                .unwrap_or_else(|| panic!("shuvarie-catalog: invalid provider key '{name}'"));
            let raw: ProviderToml = toml::from_str(contents)
                .unwrap_or_else(|_| panic!("shuvarie-catalog: {name}.toml must parse"));
            let fallback_rates = Rates {
                input_per_mtok: raw.input_per_mtok.unwrap_or(0.0),
                output_per_mtok: raw.output_per_mtok.unwrap_or(0.0),
            };
            let mut builder = ProviderBuilder::default();
            for m in raw.models {
                let rates = match (m.input_per_mtok, m.output_per_mtok) {
                    (Some(input), Some(output)) => Some(Rates {
                        input_per_mtok: input,
                        output_per_mtok: output,
                    }),
                    _ => None,
                };
                for alias in m.aliases {
                    builder.aliases.insert(
                        alias,
                        Alias {
                            model: m.model.clone(),
                            context_length: m.context_length,
                            rates,
                        },
                    );
                }
                for v in m.variants {
                    let variant_rates = match (v.input_per_mtok, v.output_per_mtok) {
                        (Some(input), Some(output)) => Some(Rates {
                            input_per_mtok: input,
                            output_per_mtok: output,
                        }),
                        _ => rates,
                    };
                    builder.aliases.insert(
                        v.id.clone(),
                        Alias {
                            model: m.model.clone(),
                            context_length: v.context_length.or(m.context_length),
                            rates: variant_rates,
                        },
                    );
                    builder.variants.push(Variant {
                        id: v.id,
                        kind: v.kind,
                        model: m.model.clone(),
                    });
                }
            }
            let mut prefix_keys: Vec<String> = builder.aliases.keys().cloned().collect();
            prefix_keys.sort_unstable_by_key(|k| std::cmp::Reverse(k.len()));
            let mut pc = ProviderCatalog {
                fallback_rates,
                aliases: builder.aliases,
                variants: builder.variants,
                prefix_keys,
            };
            pc.aliases.shrink_to_fit();
            pc.variants.shrink_to_fit();
            (provider, pc)
        })
        .collect();

    CatalogData { models, providers }
}

fn catalog() -> &'static CatalogData {
    static CATALOG: OnceLock<CatalogData> = OnceLock::new();
    CATALOG.get_or_init(build_data)
}

fn provider_catalog(provider: Provider) -> &'static ProviderCatalog {
    catalog().providers.get(&provider).unwrap_or_else(|| {
        static EMPTY: OnceLock<ProviderCatalog> = OnceLock::new();
        EMPTY.get_or_init(|| ProviderCatalog {
            fallback_rates: Rates::default(),
            aliases: HashMap::new(),
            variants: Vec::new(),
            prefix_keys: Vec::new(),
        })
    })
}

fn find_alias<'a>(pc: &'a ProviderCatalog, model_id: &str) -> Option<&'a Alias> {
    if let Some(alias) = pc.aliases.get(model_id) {
        return Some(alias);
    }
    pc.prefix_keys
        .iter()
        .filter(|key| model_id.starts_with(key.as_str()))
        .map(|key| &pc.aliases[key.as_str()])
        .next()
}

pub fn resolve(provider: Provider, model_id: &str) -> Option<ModelEntry> {
    let pc = provider_catalog(provider);
    let alias = find_alias(pc, model_id)?;
    let entry = catalog().models.get(&alias.model)?;
    Some(ModelEntry {
        context_length: alias.context_length.unwrap_or(entry.context_length),
        rates: alias.rates.unwrap_or(entry.rates),
    })
}

pub fn fallback_rates(provider: Provider) -> Rates {
    provider_catalog(provider).fallback_rates
}

pub fn variants(provider: Provider, canonical_model: &str) -> Vec<&'static Variant> {
    provider_catalog(provider)
        .variants
        .iter()
        .filter(|v| v.model == canonical_model)
        .collect()
}

pub fn estimate_cost(provider: Provider, model_id: &str, usage: &TokenUsage) -> f64 {
    let (input_rate, output_rate) = resolve(provider, model_id)
        .map(|entry| (entry.rates.input_per_mtok, entry.rates.output_per_mtok))
        .unwrap_or_else(|| {
            let rates = fallback_rates(provider);
            (rates.input_per_mtok, rates.output_per_mtok)
        });
    let input = usage.input_tokens as f64 / 1e6 * input_rate;
    let cached = usage.cached_input_tokens as f64 / 1e6 * input_rate * CACHE_READ_FACTOR;
    let output = usage.output_tokens as f64 / 1e6 * output_rate;
    let reasoning = usage.reasoning_tokens as f64 / 1e6 * output_rate * REASONING_FACTOR;
    input + cached + output + reasoning
}

pub fn enrich(provider: Provider, info: &mut ModelInfo) {
    if info.context_length.is_none()
        && let Some(entry) = resolve(provider, &info.id)
    {
        info.context_length = Some(entry.context_length);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_catalog_and_exposes_fallback_rates() {
        let rates = fallback_rates(Provider::OpenAiCompatible);
        assert_eq!(rates.input_per_mtok, 2.50);
        assert_eq!(rates.output_per_mtok, 10.00);
        assert_eq!(fallback_rates(Provider::Anthropic).input_per_mtok, 3.00);
        assert_eq!(fallback_rates(Provider::Ollama).input_per_mtok, 0.0);
        assert_eq!(fallback_rates(Provider::OllamaCloud).output_per_mtok, 0.0);
    }

    #[test]
    fn resolves_exact_model_alias() {
        let entry = resolve(Provider::Anthropic, "claude-sonnet-4-5-20250929").unwrap();
        assert_eq!(entry.context_length, 200_000);
        assert_eq!(entry.rates.input_per_mtok, 3.00);
        assert_eq!(entry.rates.output_per_mtok, 15.00);
    }

    #[test]
    fn resolves_alias_to_canonical_stats() {
        let entry = resolve(Provider::OpenAiCompatible, "gpt-4o-2024-08-06").unwrap();
        assert_eq!(entry.context_length, 128_000);
        assert_eq!(entry.rates.input_per_mtok, 2.50);
        let entry = resolve(Provider::OpenAiCompatible, "gpt-3.5-turbo-0125").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 0.50);
    }

    #[test]
    fn resolves_base_id_alias() {
        let entry = resolve(Provider::OpenAiCompatible, "gpt-4o").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 2.50);
        let entry = resolve(Provider::Gemini, "gemini-2.5-pro").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 1.25);
    }

    #[test]
    fn prefix_match_disambiguates_families() {
        let mini = resolve(Provider::OpenAiCompatible, "gpt-4o-mini-2024-07-18").unwrap();
        assert_eq!(mini.rates.input_per_mtok, 0.15);
        let base = resolve(Provider::OpenAiCompatible, "gpt-4o-2024-08-06").unwrap();
        assert_eq!(base.rates.input_per_mtok, 2.50);
        let sonnet = resolve(Provider::Anthropic, "claude-3-5-sonnet-20241022").unwrap();
        assert_eq!(sonnet.rates.input_per_mtok, 3.00);
        let haiku = resolve(Provider::Anthropic, "claude-3-5-haiku-20241022").unwrap();
        assert_eq!(haiku.rates.input_per_mtok, 0.80);
    }

    #[test]
    fn canonical_model_resolves_to_own_entry() {
        let entry = resolve(Provider::OpenAiCompatible, "openai/gpt-4o").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 2.50);
        assert_eq!(entry.rates.output_per_mtok, 10.00);
    }

    #[test]
    fn canonical_ids_resolve_via_provider_alias() {
        let entry = resolve(Provider::OpenRouter, "openai/gpt-4o").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 2.50);
        let entry = resolve(Provider::OpenRouter, "anthropics/claude-sonnet-4-5").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 3.00);
        let entry = resolve(Provider::Groq, "meta-llama/llama-3.3-70b").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 0.59);
    }

    #[test]
    fn canonical_model_without_alias_skipped() {
        assert_eq!(resolve(Provider::Groq, "deepseek-ai/deepseek-chat"), None);
    }

    #[test]
    fn unknown_model_falls_back_to_provider_defaults() {
        assert_eq!(
            resolve(Provider::OpenAiCompatible, "some-custom-model"),
            None
        );
        assert_eq!(resolve(Provider::Ollama, "llama3.1"), None);
        assert_eq!(
            estimate_cost(
                Provider::OpenAiCompatible,
                "some-custom-model",
                &TokenUsage {
                    input_tokens: 1_000_000,
                    output_tokens: 0,
                    ..TokenUsage::default()
                },
            ),
            2.50
        );
    }

    #[test]
    fn openrouter_prefixed_ids_resolve() {
        let entry = resolve(Provider::OpenRouter, "anthropic/claude-sonnet-4.5").unwrap();
        assert_eq!(entry.context_length, 200_000);
        assert_eq!(entry.rates.input_per_mtok, 3.00);
        let entry = resolve(Provider::OpenRouter, "deepseek/deepseek-reasoner").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 0.55);
    }

    #[test]
    fn deepseek_v3_aliases_to_chat_canonical() {
        let entry = resolve(Provider::DeepSeek, "deepseek-v3").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 0.27);
        assert_eq!(entry.rates.output_per_mtok, 1.10);
    }

    #[test]
    fn variant_id_resolves_with_variant_rates() {
        let entry = resolve(Provider::OpenRouter, "openai/gpt-5-pro").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 15.0);
        assert_eq!(entry.rates.output_per_mtok, 120.0);
    }

    #[test]
    fn variant_id_without_overrides_uses_canonical_stats() {
        let entry = resolve(Provider::OpenRouter, "moonshotai/kimi-k2-thinking").unwrap();
        assert_eq!(entry.rates.input_per_mtok, 0.60);
        assert_eq!(entry.rates.output_per_mtok, 2.50);
        assert_eq!(entry.context_length, 262_144);
    }

    #[test]
    fn variants_lists_tiers() {
        let v = variants(Provider::OpenRouter, "openai/gpt-5");
        let kinds: Vec<&str> = v.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, ["pro"]);
        let v = variants(Provider::OpenRouter, "moonshotai/kimi-k2");
        let kinds: Vec<&str> = v.iter().map(|x| x.kind.as_str()).collect();
        assert_eq!(kinds, ["thinking"]);
        assert!(variants(Provider::OpenRouter, "openai/gpt-4o").is_empty());
        assert!(variants(Provider::OpenAiCompatible, "openai/gpt-5").is_empty());
    }

    #[test]
    fn all_model_entries_reference_known_canonical_ids() {
        let data = build_data();
        let missing: Vec<&String> = data
            .providers
            .values()
            .flat_map(|pc| pc.aliases.values())
            .filter(|alias| !data.models.contains_key(&alias.model))
            .map(|alias| &alias.model)
            .collect();
        assert!(
            missing.is_empty(),
            "provider aliases reference unknown canonical models: {missing:?}"
        );
    }

    #[test]
    fn no_duplicate_alias_across_providers() {
        for pc in catalog().providers.values() {
            let mut seen = HashMap::new();
            for (id, alias) in &pc.aliases {
                let prev = seen.insert(id, &alias.model);
                assert_eq!(prev, None, "alias '{id}' listed twice");
            }
        }
    }

    #[test]
    fn local_providers_cost_nothing() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            ..TokenUsage::default()
        };
        assert_eq!(estimate_cost(Provider::Ollama, "llama3.1", &usage), 0.0);
        assert_eq!(
            estimate_cost(Provider::OllamaCloud, "llama3.1", &usage),
            0.0
        );
    }

    #[test]
    fn cost_accounts_for_cache_and_reasoning() {
        let plain = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            reasoning_tokens: 100_000,
            ..TokenUsage::default()
        };
        let cached = TokenUsage {
            cached_input_tokens: 1_000_000,
            ..plain
        };
        let plain_cost = estimate_cost(Provider::Anthropic, "claude-sonnet-4-5", &plain);
        let cached_cost = estimate_cost(Provider::Anthropic, "claude-sonnet-4-5", &cached);
        assert!(cached_cost > plain_cost);
        assert!(
            cached_cost
                - estimate_cost(
                    Provider::Anthropic,
                    "claude-sonnet-4-5",
                    &TokenUsage {
                        cached_input_tokens: 1_000_000,
                        ..TokenUsage::default()
                    }
                )
                > 0.0
        );
    }

    #[test]
    fn enrich_fills_missing_context_length() {
        let mut info = ModelInfo {
            id: "gpt-4o".into(),
            name: Some("GPT-4o".into()),
            context_length: None,
        };
        enrich(Provider::OpenAiCompatible, &mut info);
        assert_eq!(info.context_length, Some(128_000));

        let mut unknown = ModelInfo {
            id: "custom-model".into(),
            name: None,
            context_length: None,
        };
        enrich(Provider::OpenAiCompatible, &mut unknown);
        assert_eq!(unknown.context_length, None);
    }

    #[test]
    fn enrich_keeps_runtime_context_length() {
        let mut info = ModelInfo {
            id: "gpt-4o".into(),
            name: None,
            context_length: Some(999),
        };
        enrich(Provider::OpenAiCompatible, &mut info);
        assert_eq!(info.context_length, Some(999));
    }

    #[test]
    fn provider_serde_round_trip() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        struct ProviderTomlTest {
            kind: Provider,
        }
        let toml = "kind = \"open_ai_compatible\"";
        let pc: ProviderTomlTest = toml::from_str(toml).unwrap();
        assert_eq!(pc.kind, Provider::OpenAiCompatible);
        let back = toml::to_string(&pc).unwrap();
        assert_eq!(back, "kind = \"open_ai_compatible\"\n");
    }
}
