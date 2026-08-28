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
/// catalog endpoint.
pub fn effective_base_url(provider: &Provider, override_url: Option<&str>) -> String {
    match override_url {
        Some(u) if !u.trim().is_empty() => u.to_string(),
        _ => api_endpoint(provider).unwrap_or_default(),
    }
}

/// Resolve a provider config's `kind` (a catalog id) to its [`selune::ProviderType`],
/// falling back to a built-in mapping for known local providers (e.g. `ollama`,
/// which the Selune catalog omits) and then to [`selune::ProviderType::OpenaiCompat`].
pub fn provider_type(providers: &[Provider], kind: &str) -> selune::ProviderType {
    find_provider(providers, kind)
        .and_then(|p| p.r#type)
        .or_else(|| known_provider_type(kind))
        .unwrap_or(selune::ProviderType::OpenaiCompat)
}

/// Built-in type mapping for provider ids not present in the catalog.
fn known_provider_type(kind: &str) -> Option<selune::ProviderType> {
    match kind {
        "ollama" => Some(selune::ProviderType::Ollama),
        _ => None,
    }
}

/// The effective base URL for a provider config's `kind`, using an explicit
/// override, else the catalog endpoint, else a built-in default for known
/// local providers.
pub fn base_url_for(providers: &[Provider], kind: &str, override_url: Option<&str>) -> String {
    if let Some(u) = override_url.filter(|u| !u.trim().is_empty()) {
        return u.to_string();
    }
    if let Some(p) = find_provider(providers, kind) {
        return effective_base_url(p, None);
    }
    known_base_url(kind).unwrap_or_default()
}

/// Built-in base URL for provider ids not present in the catalog.
fn known_base_url(kind: &str) -> Option<String> {
    match kind {
        "ollama" => Some("http://localhost:11434".to_string()),
        _ => None,
    }
}
