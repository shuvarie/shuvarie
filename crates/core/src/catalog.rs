use std::sync::{Mutex, OnceLock};

use selune::{Client, Provider};
use shuvarie_llm::TokenUsage;

const CACHE_READ_FACTOR: f64 = 0.1;
const REASONING_FACTOR: f64 = 0.6;

/// Process-global provider catalog. Initialized from the embedded providers and
/// optionally refreshed from the catalog service at runtime (see [`refresh`]).
fn state() -> &'static Mutex<Vec<Provider>> {
    static STATE: OnceLock<Mutex<Vec<Provider>>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(selune::embedded::all()))
}

/// Snapshot of the currently active provider catalog.
pub fn providers() -> Vec<Provider> {
    state().lock().unwrap().clone()
}

/// Replace the provider catalog with a fresh snapshot (used by the runtime
/// refresh path).
pub fn set_providers(providers: Vec<Provider>) {
    *state().lock().unwrap() = providers;
}

/// Fetch the provider catalog from the service, falling back to the current
/// snapshot (i.e. embedded) on any failure. Returns the providers in effect.
pub fn refresh() -> Vec<Provider> {
    match Client::new().get_providers() {
        Ok(providers) if !providers.is_empty() => {
            set_providers(providers.clone());
            providers
        }
        _ => providers(),
    }
}

/// Look up a provider by id in the given catalog.
pub fn find_provider<'a>(providers: &'a [Provider], id: &str) -> Option<&'a Provider> {
    providers.iter().find(|p| p.id.0 == id)
}

/// Look up a model by id within a provider.
pub fn find_model<'a>(provider: &'a Provider, model_id: &str) -> Option<&'a selune::Model> {
    provider.models.iter().find(|m| m.id == model_id)
}

/// Resolve a model's context length for the given provider, if known.
pub fn context_length(provider: &Provider, model_id: &str) -> Option<i64> {
    find_model(provider, model_id).and_then(|m| m.limit.context)
}

/// Fill a model's missing runtime context length from the catalog, if known.
pub fn enrich(provider: &Provider, info: &mut shuvarie_llm::Model) {
    if info.context_length.is_none()
        && let Some(ctx) = context_length(provider, &info.id)
    {
        info.context_length = Some(ctx.max(0) as u32);
    }
}

/// Estimate the cost of a model call from the catalog's per-mtok rates.
/// Falls back to the provider's first model's rates (or 0) when unknown.
pub fn estimate_cost(provider: &Provider, model_id: &str, usage: &TokenUsage) -> f64 {
    let (input_rate, output_rate, cache_read_rate) = find_model(provider, model_id)
        .map(|m| {
            (
                m.cost.input.unwrap_or(0.0),
                m.cost.output.unwrap_or(0.0),
                m.cost.cache_read.unwrap_or(0.0),
            )
        })
        .unwrap_or_else(|| default_rates(provider));
    let input = usage.input_tokens as f64 / 1e6 * input_rate;
    let cached = usage.cached_input_tokens as f64 / 1e6 * cache_read_rate * CACHE_READ_FACTOR;
    let output = usage.output_tokens as f64 / 1e6 * output_rate;
    let reasoning = usage.reasoning_tokens as f64 / 1e6 * output_rate * REASONING_FACTOR;
    input + cached + output + reasoning
}

/// Default per-mtok rates for a provider, used when a model is unknown. Derives
/// from the provider's first model, falling back to zero when there are none.
fn default_rates(provider: &Provider) -> (f64, f64, f64) {
    provider
        .models
        .first()
        .map(|m| {
            (
                m.cost.input.unwrap_or(0.0),
                m.cost.output.unwrap_or(0.0),
                m.cost.cache_read.unwrap_or(0.0),
            )
        })
        .unwrap_or((0.0, 0.0, 0.0))
}

/// Whether an API key is required for the given provider, based on the catalog
/// entry's `api_key` field.
pub fn requires_api_key(provider: &Provider) -> bool {
    provider.api_key.is_some()
}

/// The provider's configured API endpoint, with `$ENV_VAR` placeholders
/// resolved from the environment. Returns `None` when no endpoint is known.
pub fn api_endpoint(provider: &Provider) -> Option<String> {
    provider.api_endpoint.as_deref().map(resolve_env)
}

/// Resolve `$NAME` / `${NAME}` placeholders in a string against the
/// environment. Unknown variables are left as-is.
pub fn resolve_env(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                if let Some(end) = s[i + 2..].find('}') {
                    let name = &s[i + 2..i + 2 + end];
                    match std::env::var(name) {
                        Ok(v) => out.push_str(&v),
                        Err(_) => out.push_str(&s[i..i + 2 + end + 1]),
                    }
                    i += 2 + end + 1;
                    continue;
                }
            } else {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                if j > i + 1 {
                    let name = &s[i + 1..j];
                    match std::env::var(name) {
                        Ok(v) => out.push_str(&v),
                        Err(_) => out.push_str(&s[i..j]),
                    }
                    i = j;
                    continue;
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// The provider's effective base URL, using an explicit override or the
/// catalog endpoint. Unresolved `$ENV_VAR` placeholders count as unknown.
pub fn effective_base_url(provider: &Provider, override_url: Option<&str>) -> Option<String> {
    match override_url {
        Some(u) if !u.trim().is_empty() => Some(u.to_string()),
        _ => api_endpoint(provider).filter(|u| !u.contains('$')),
    }
}

/// Parse a connection `kind` as a rig transport ([`selune::ProviderType`] in
/// kebab-case, e.g. `openai`, `openai-compat`, `google-vertex`).
pub fn parse_provider_type(kind: &str) -> Option<selune::ProviderType> {
    serde_json::from_value(serde_json::Value::String(kind.to_string())).ok()
}

/// The kebab-case name of a provider type, as accepted by
/// [`parse_provider_type`].
pub fn provider_type_name(kind: selune::ProviderType) -> &'static str {
    match kind {
        selune::ProviderType::Openai => "openai",
        selune::ProviderType::OpenaiCompat => "openai-compat",
        selune::ProviderType::Openrouter => "openrouter",
        selune::ProviderType::Vercel => "vercel",
        selune::ProviderType::Anthropic => "anthropic",
        selune::ProviderType::Google => "google",
        selune::ProviderType::Azure => "azure",
        selune::ProviderType::Bedrock => "bedrock",
        selune::ProviderType::GoogleVertex => "google-vertex",
        selune::ProviderType::Ollama => "ollama",
    }
}

/// Resolve a provider config's `kind` to its [`selune::ProviderType`]. The
/// `kind` is the rig transport (e.g. `openai`, `ollama`); as a fallback for
/// older configs it may also be a Selune catalog id, resolved through the
/// catalog. Defaults to [`selune::ProviderType::OpenaiCompat`].
pub fn provider_type(kind: &str) -> selune::ProviderType {
    provider_type_in(&providers(), kind)
}

fn provider_type_in(providers: &[Provider], kind: &str) -> selune::ProviderType {
    parse_provider_type(kind)
        .or_else(|| find_provider(providers, kind).and_then(|p| p.r#type))
        .unwrap_or(selune::ProviderType::OpenaiCompat)
}

/// The effective base URL for a provider config's `kind`, using an explicit
/// override, else a built-in default for the transport, else the catalog
/// endpoint (for legacy catalog-id kinds). `None` lets rig use its own
/// per-provider default.
pub fn base_url_for(kind: &str, override_url: Option<&str>) -> Option<String> {
    base_url_for_in(&providers(), kind, override_url)
}

fn base_url_for_in(
    providers: &[Provider],
    kind: &str,
    override_url: Option<&str>,
) -> Option<String> {
    if let Some(u) = override_url.filter(|u| !u.trim().is_empty()) {
        return Some(u.to_string());
    }
    match parse_provider_type(kind) {
        Some(selune::ProviderType::Ollama) => return Some("http://localhost:11434".to_string()),
        Some(_) => return None,
        None => {}
    }
    find_provider(providers, kind).and_then(|p| effective_base_url(p, None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{InferenceProvider, ProviderType};

    fn catalog_provider(
        id: &str,
        r#type: Option<ProviderType>,
        endpoint: Option<&str>,
    ) -> Provider {
        Provider {
            name: id.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: None,
            api_endpoint: endpoint.map(str::to_string),
            r#type,
            doc: None,
            default_large_model_id: None,
            default_small_model_id: None,
            models: Vec::new(),
            default_headers: None,
        }
    }

    #[test]
    fn parses_all_transport_kinds() {
        for (name, expected) in [
            ("openai", ProviderType::Openai),
            ("openai-compat", ProviderType::OpenaiCompat),
            ("openrouter", ProviderType::Openrouter),
            ("vercel", ProviderType::Vercel),
            ("anthropic", ProviderType::Anthropic),
            ("google", ProviderType::Google),
            ("azure", ProviderType::Azure),
            ("bedrock", ProviderType::Bedrock),
            ("google-vertex", ProviderType::GoogleVertex),
            ("ollama", ProviderType::Ollama),
        ] {
            assert_eq!(parse_provider_type(name), Some(expected), "{name}");
            assert_eq!(provider_type_name(expected), name);
        }
        assert_eq!(parse_provider_type("ollama-cloud"), None);
        assert_eq!(parse_provider_type(""), None);
    }

    #[test]
    fn provider_type_prefers_transport_then_legacy_catalog_id() {
        let providers = vec![catalog_provider(
            "ollama-cloud",
            Some(ProviderType::Ollama),
            None,
        )];
        assert_eq!(provider_type_in(&providers, "ollama"), ProviderType::Ollama);
        assert_eq!(
            provider_type_in(&providers, "ollama-cloud"),
            ProviderType::Ollama
        );
        assert_eq!(
            provider_type_in(&providers, "gemini"),
            ProviderType::OpenaiCompat
        );
    }

    #[test]
    fn base_url_prefers_override_then_type_default_then_catalog() {
        let providers = vec![catalog_provider(
            "acme",
            Some(ProviderType::OpenaiCompat),
            Some("https://acme.example/v1"),
        )];
        assert_eq!(
            base_url_for_in(&providers, "ollama", None).as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(
            base_url_for_in(&providers, "ollama", Some("  ")).as_deref(),
            Some("http://localhost:11434")
        );
        assert_eq!(base_url_for_in(&providers, "anthropic", None), None);
        assert_eq!(base_url_for_in(&providers, "openai", None), None);
        assert_eq!(
            base_url_for_in(&providers, "acme", None).as_deref(),
            Some("https://acme.example/v1")
        );
        assert_eq!(
            base_url_for_in(&providers, "acme", Some("https://x.dev/v1")).as_deref(),
            Some("https://x.dev/v1")
        );
        assert_eq!(base_url_for_in(&providers, "unknown", None), None);
    }

    #[test]
    fn base_url_skips_unresolved_env_placeholders() {
        let providers = vec![catalog_provider(
            "anthropic",
            Some(ProviderType::Anthropic),
            Some("$ANTHROPIC_API_ENDPOINT"),
        )];
        assert_eq!(base_url_for_in(&providers, "anthropic", None), None);
    }

    #[test]
    fn effective_base_url_resolves_env() {
        unsafe { std::env::set_var("SHUVARIE_TEST_ENDPOINT", "https://env.example/v1") };
        assert_eq!(
            effective_base_url(
                &catalog_provider("x", None, Some("$SHUVARIE_TEST_ENDPOINT")),
                None,
            )
            .as_deref(),
            Some("https://env.example/v1")
        );
        unsafe { std::env::remove_var("SHUVARIE_TEST_ENDPOINT") };
        assert_eq!(
            effective_base_url(
                &catalog_provider("x", None, Some("$SHUVARIE_TEST_ENDPOINT")),
                None,
            ),
            None
        );
    }
}
