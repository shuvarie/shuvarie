use std::sync::{Mutex, OnceLock};

use selune::{Client, Provider};
use shuvarie_config::{Connections, ProviderConfig};
use shuvarie_llm::TokenUsage;

const CACHE_READ_FACTOR: f64 = 0.1;
const REASONING_FACTOR: f64 = 0.6;

/// Process-global provider catalog. The embedded snapshot is fixed at first
/// use; the remote snapshot holds the latest successful hosted fetch (see
/// [`fetch_remote`]). Metadata lookups ([`providers`]) see the remote snapshot
/// once loaded, else the embedded one; the registry popups pick a source
/// explicitly via [`registry_providers`].
fn state() -> &'static Mutex<CatalogState> {
    static STATE: OnceLock<Mutex<CatalogState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CatalogState {
            embedded: selune::embedded::all(),
            remote: None,
        })
    })
}

struct CatalogState {
    embedded: Vec<Provider>,
    remote: Option<Vec<Provider>>,
}

impl CatalogState {
    fn effective(&self) -> Vec<Provider> {
        self.remote.clone().unwrap_or_else(|| self.embedded.clone())
    }
}

/// Snapshot of the currently effective provider catalog (remote once loaded,
/// else embedded).
pub fn providers() -> Vec<Provider> {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
        .effective()
}

/// The offline embedded registry.
pub fn embedded_registry() -> Vec<Provider> {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
        .embedded
        .clone()
}

/// The remote registry snapshot, when a hosted fetch has succeeded this
/// session.
pub fn remote_registry() -> Option<Vec<Provider>> {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
        .remote
        .clone()
}

/// Whether a remote registry snapshot is loaded.
pub fn remote_registry_loaded() -> bool {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
        .remote
        .is_some()
}

/// Providers for the registry popups: the remote snapshot when `remote` is
/// wanted (empty until a fetch succeeds), else the embedded one.
pub fn registry_providers(remote: bool) -> Vec<Provider> {
    if remote {
        remote_registry().unwrap_or_default()
    } else {
        embedded_registry()
    }
}

/// Store the remote registry snapshot.
pub fn set_remote_registry(providers: Vec<Provider>) {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
        .remote = Some(providers);
}

/// Fetch the provider catalog from the service, falling back to the current
/// effective snapshot (i.e. embedded) on any failure. Returns the effective
/// providers. Used by the bounded startup refresh; the on-demand path uses
/// [`fetch_remote`], which surfaces errors instead.
pub fn refresh() -> Vec<Provider> {
    match Client::new().get_providers() {
        Ok(providers) if !providers.is_empty() => {
            set_remote_registry(providers.clone());
            providers
        }
        _ => providers(),
    }
}

/// Fetch the hosted registry, storing a successful result as the remote
/// snapshot. Unlike [`refresh`] there is no fallback: an empty or failed
/// fetch is an error so the caller can report it.
pub fn fetch_remote() -> Result<Vec<Provider>, String> {
    match Client::new().get_providers() {
        Ok(providers) if !providers.is_empty() => {
            set_remote_registry(providers.clone());
            Ok(providers)
        }
        Ok(_) => Err("the hosted registry returned no providers".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Look up a provider by id in the given catalog.
pub fn find_provider<'a>(providers: &'a [Provider], id: &str) -> Option<&'a Provider> {
    providers.iter().find(|p| p.id.0 == id)
}

/// Look up a model by id within a provider. Matches the exact catalog id, a
/// vendor-prefixed connection id (`vendor/model`), a dated snapshot alias in
/// either direction (`claude-sonnet-4-5` ↔ `claude-sonnet-4-5-20250929`), or a
/// unique tagged variant (`glm-5.3-flash` ↔ `glm-5.3-flash:cloud`).
pub fn find_model<'a>(provider: &'a Provider, model_id: &str) -> Option<&'a selune::Model> {
    provider
        .models
        .iter()
        .find(|m| m.id == model_id)
        .or_else(|| vendor_tail_match(provider, model_id))
        .or_else(|| dated_alias_match(provider, model_id))
        .or_else(|| tag_match(provider, model_id))
}

/// Match a connection id like `vendor/model` against a catalog entry whose id
/// is just the bare model name.
fn vendor_tail_match<'a>(provider: &'a Provider, model_id: &str) -> Option<&'a selune::Model> {
    let (_, tail) = model_id.split_once('/')?;
    provider.models.iter().find(|m| m.id == tail)
}

/// Match a model id to a dated snapshot of the same model: the differing
/// suffix must be digits/dashes only (a date), so `claude-sonnet-4-5` matches
/// `claude-sonnet-4-5-20250929` while `gpt-5.4` never matches `gpt-5.4-mini`.
fn dated_alias_match<'a>(provider: &'a Provider, model_id: &str) -> Option<&'a selune::Model> {
    provider
        .models
        .iter()
        .find(|m| is_dated_alias(&m.id, model_id) || is_dated_alias(model_id, &m.id))
}

/// Match an Ollama-style tagged id against the catalog by its tag-stripped
/// base (`glm-5.3-flash` ↔ `glm-5.3-flash:cloud`). The base must be unique in
/// the catalog: `gpt-oss:20b` and `gpt-oss:120b` share a base but are
/// different models, so an ambiguous base matches nothing.
fn tag_match<'a>(provider: &'a Provider, model_id: &str) -> Option<&'a selune::Model> {
    let base = strip_tag(model_id);
    let mut candidates = provider.models.iter().filter(|m| strip_tag(&m.id) == base);
    let first = candidates.next()?;
    if candidates.next().is_some() {
        return None;
    }
    Some(first)
}

fn strip_tag(id: &str) -> &str {
    id.split(':').next().unwrap_or(id)
}

/// Whether `long` is `base` followed by a date-like suffix (`20250929`,
/// `2024-11-20`): only digits and dashes, at least six digits total.
fn is_dated_alias(long: &str, base: &str) -> bool {
    if base.is_empty() {
        return false;
    }
    let Some(rest) = long.strip_prefix(base) else {
        return false;
    };
    let digits = rest.chars().filter(|c| c.is_ascii_digit()).count();
    digits >= 6 && rest.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// Resolve a model's context length for the given provider, if known.
pub fn context_length(provider: &Provider, model_id: &str) -> Option<i64> {
    find_model(provider, model_id).and_then(|m| m.limit.context)
}

/// The reasoning-effort variants of a catalog model, in declared order.
/// Empty for models without an effort-typed option carrying values.
pub fn model_variants(model: &selune::Model) -> &[String] {
    model
        .reasoning_options
        .iter()
        .find(|o| o.r#type == "effort" && !o.values.is_empty())
        .map(|o| o.values.as_slice())
        .unwrap_or(&[])
}

/// The variant a cycle step lands on: one of the model's declared
/// reasoning-effort values or the unset default the cycle wraps back to
/// after the last declared one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NextVariant {
    /// Clear the variant — the cycle wrapped past the last declared option.
    Default,
    /// The next declared reasoning-effort variant.
    Set(String),
}

/// The reasoning-effort variant following `current` for the catalog model
/// matching `model_id` under `provider`'s catalog id. The cycle runs unset →
/// first variant → … → last variant → unset; an unset or unknown current
/// starts at the first variant, and a stale current on a model without
/// declared variants cycles back to the default. `None` when the provider or
/// model is not in the catalog (a no-op).
pub fn next_variant(
    provider: &ProviderConfig,
    model_id: &str,
    current: Option<&str>,
) -> Option<NextVariant> {
    next_variant_in(&providers(), provider, model_id, current)
}

fn next_variant_in(
    providers: &[Provider],
    provider: &ProviderConfig,
    model_id: &str,
    current: Option<&str>,
) -> Option<NextVariant> {
    let id = provider.catalog_id()?;
    let entry = find_provider(providers, id)?;
    let model = find_model(entry, model_id)?;
    let variants = model_variants(model);
    match current.and_then(|c| variants.iter().position(|v| v == c)) {
        Some(i) => match variants.get(i + 1) {
            Some(next) => Some(NextVariant::Set(next.clone())),
            None => Some(NextVariant::Default),
        },
        None => match (current.is_some(), variants.first()) {
            (_, Some(first)) => Some(NextVariant::Set(first.clone())),
            (true, None) => Some(NextVariant::Default),
            (false, None) => None,
        },
    }
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

/// Whether the provider is configured enough to open a connection. An
/// explicit `catalog` entry governs (its `api_key` requirement); otherwise
/// the transport decides: a key is required unless it's a local one
/// (`ollama`). A `kind` that parses as neither is treated as a legacy
/// catalog id.
pub fn is_connectable(provider: &ProviderConfig) -> bool {
    let has_key = provider
        .api_key
        .as_ref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    if let Some(catalog) = &provider.catalog {
        return catalog_requires_key(catalog, has_key);
    }
    match parse_provider_type(&provider.kind) {
        Some(selune::ProviderType::Ollama) => true,
        Some(_) => has_key,
        None => catalog_requires_key(&provider.kind, has_key),
    }
}

/// Whether the active provider (if any) is configured and connectable.
pub fn has_connected_providers(connections: &Connections) -> bool {
    if connections.providers.is_empty() {
        return false;
    }
    match &connections.active {
        None => false,
        Some(active) => match connections.providers.get(&active.provider) {
            None => false,
            Some(p) => is_connectable(p),
        },
    }
}

/// Whether a connection to the catalog entry `id` may proceed given whether a
/// non-empty API key is present. Unknown catalog ids fall back to `has_key`.
fn catalog_requires_key(id: &str, has_key: bool) -> bool {
    let catalog = providers();
    match catalog.iter().find(|p| p.id.0 == id) {
        Some(p) => match &p.api_key {
            Some(_) => has_key,
            None => true,
        },
        None => has_key,
    }
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

    fn test_provider(id: &str, models: impl IntoIterator<Item = (&'static str, i64)>) -> Provider {
        Provider {
            models: models
                .into_iter()
                .map(|(model, context)| selune::Model {
                    id: model.to_string(),
                    name: model.to_string(),
                    reasoning: false,
                    reasoning_options: Vec::new(),
                    attachment: false,
                    limit: selune::ModelLimit {
                        context: Some(context),
                        ..Default::default()
                    },
                    cost: selune::ModelCost::default(),
                    options: None,
                })
                .collect(),
            ..catalog_provider(id, None, None)
        }
    }

    #[test]
    fn find_model_matches_exact_vendor_tail_and_dated_aliases() {
        let models = [
            ("gpt-5.4", 128_000),
            ("gpt-5.4-mini", 400_000),
            ("claude-sonnet-4-5-20250929", 200_000),
            ("moonshotai/kimi-k2-instruct", 131_072),
        ];
        let provider = test_provider("acme", models);
        fn id_of(m: Option<&selune::Model>) -> Option<&str> {
            m.map(|m| m.id.as_str())
        }

        assert_eq!(id_of(find_model(&provider, "gpt-5.4")), Some("gpt-5.4"));
        assert_eq!(
            id_of(find_model(&provider, "moonshotai/kimi-k2-instruct")),
            Some("moonshotai/kimi-k2-instruct")
        );
        assert_eq!(
            id_of(find_model(&provider, "moonshotai/gpt-5.4")),
            Some("gpt-5.4")
        );
        assert_eq!(
            id_of(find_model(&provider, "claude-sonnet-4-5")),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(
            id_of(find_model(&provider, "claude-sonnet-4-5-20250929")),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(
            id_of(find_model(&provider, "gpt-5.4-20250101")),
            Some("gpt-5.4")
        );
        assert_eq!(
            id_of(find_model(&provider, "gpt-5.4-2025-01-01")),
            Some("gpt-5.4")
        );
        assert_eq!(
            id_of(find_model(&provider, "gpt-5.4-mini")),
            Some("gpt-5.4-mini")
        );
        assert_eq!(id_of(find_model(&provider, "gpt-5.4-preview")), None);
        assert_eq!(id_of(find_model(&provider, "unknown")), None);
        assert_eq!(id_of(find_model(&provider, "")), None);
    }

    #[test]
    fn context_length_resolves_through_dated_alias() {
        let provider = test_provider("anthropic", [("claude-sonnet-4-5-20250929", 200_000)]);
        assert_eq!(
            context_length(&provider, "claude-sonnet-4-5"),
            Some(200_000)
        );
    }

    #[test]
    fn find_model_matches_unique_tagged_variant() {
        let models = [
            ("glm-5.3", 1_310_720),
            ("glm-5.3-flash:cloud", 1_310_720),
            ("gpt-oss:20b", 131_072),
            ("gpt-oss:120b", 131_072),
            ("kimi-k3", 1_048_576),
            ("deepseek-v4-flash", 1_048_576),
            ("deepseek-v4-flash:0731", 1_048_576),
        ];
        let provider = test_provider("ollama-cloud", models.iter().copied());
        fn id_of(m: Option<&selune::Model>) -> Option<&str> {
            m.map(|m| m.id.as_str())
        }

        assert_eq!(
            id_of(find_model(&provider, "glm-5.3-flash")),
            Some("glm-5.3-flash:cloud")
        );
        assert_eq!(
            id_of(find_model(&provider, "kimi-k3:cloud")),
            Some("kimi-k3")
        );
        assert_eq!(
            id_of(find_model(&provider, "gpt-oss:120b")),
            Some("gpt-oss:120b")
        );
        assert_eq!(id_of(find_model(&provider, "gpt-oss")), None);
        assert_eq!(
            id_of(find_model(&provider, "deepseek-v4-flash:cloud")),
            None
        );
        assert_eq!(id_of(find_model(&provider, "glm-5.3")), Some("glm-5.3"));
    }

    #[test]
    fn context_length_resolves_through_tagged_variant() {
        let provider = test_provider(
            "ollama-cloud",
            [("glm-5.3", 1_310_720), ("glm-5.3-flash:cloud", 1_310_720)],
        );
        assert_eq!(context_length(&provider, "glm-5.3-flash"), Some(1_310_720));
    }

    fn effort_model(id: &str, values: &[&str]) -> selune::Model {
        let mut model = plain_model(id);
        model.reasoning = true;
        model.reasoning_options = vec![selune::ReasoningOption {
            r#type: "effort".to_string(),
            values: values.iter().map(|v| v.to_string()).collect(),
            min: None,
            max: None,
        }];
        model
    }

    fn plain_model(id: &str) -> selune::Model {
        selune::Model {
            id: id.to_string(),
            name: id.to_string(),
            reasoning: false,
            reasoning_options: Vec::new(),
            attachment: false,
            limit: selune::ModelLimit::default(),
            cost: selune::ModelCost::default(),
            options: None,
        }
    }

    fn variant_provider(models: Vec<selune::Model>) -> Provider {
        Provider {
            models,
            ..catalog_provider("acme", None, None)
        }
    }

    #[test]
    fn model_variants_reads_the_effort_option() {
        let model = effort_model("gpt-5.4", &["low", "medium", "high"]);
        assert_eq!(
            model_variants(&model),
            ["low", "medium", "high"].map(str::to_string).as_slice()
        );
    }

    #[test]
    fn next_variant_cycles_through_the_default() {
        let providers = vec![variant_provider(vec![effort_model(
            "gpt-5.4",
            &["low", "medium", "high"],
        )])];
        let pc = ProviderConfig::new("acme", "openai", None, None).with_catalog(Some("acme"));
        let next = |current: Option<&str>| next_variant_in(&providers, &pc, "gpt-5.4", current);
        assert_eq!(
            next(None),
            Some(NextVariant::Set("low".into())),
            "unset current starts at the first"
        );
        assert_eq!(next(Some("low")), Some(NextVariant::Set("medium".into())));
        assert_eq!(next(Some("medium")), Some(NextVariant::Set("high".into())));
        assert_eq!(
            next(Some("high")),
            Some(NextVariant::Default),
            "last wraps to the default"
        );
        assert_eq!(
            next(Some("xhigh")),
            Some(NextVariant::Set("low".into())),
            "unknown current restarts the cycle"
        );
    }

    #[test]
    fn next_variant_matches_model_ids_loosely() {
        let providers = vec![variant_provider(vec![effort_model(
            "claude-opus-4-6-20251101",
            &["low", "medium", "high"],
        )])];
        let pc = ProviderConfig::new("acme", "openai", None, None).with_catalog(Some("acme"));
        assert_eq!(
            next_variant_in(&providers, &pc, "claude-opus-4-6", None),
            Some(NextVariant::Set("low".into())),
            "the dated alias resolves"
        );
    }

    #[test]
    fn next_variant_is_none_without_catalog_variants() {
        let providers = vec![variant_provider(vec![plain_model("gpt-4o")])];
        let pc = ProviderConfig::new("acme", "openai", None, None).with_catalog(Some("acme"));
        assert_eq!(next_variant_in(&providers, &pc, "gpt-4o", None), None);
        assert_eq!(
            next_variant_in(&providers, &pc, "unknown-model", None),
            None
        );
    }

    #[test]
    fn next_variant_clears_a_stale_variant_without_catalog_variants() {
        let providers = vec![variant_provider(vec![plain_model("gpt-4o")])];
        let pc = ProviderConfig::new("acme", "openai", None, None).with_catalog(Some("acme"));
        assert_eq!(
            next_variant_in(&providers, &pc, "gpt-4o", Some("high")),
            Some(NextVariant::Default),
            "a variant the model no longer declares cycles back to the default"
        );
    }

    #[test]
    fn next_variant_is_none_for_unknown_catalog_id() {
        let providers = vec![variant_provider(vec![effort_model(
            "gpt-5.4",
            &["low", "high"],
        )])];
        let pc = ProviderConfig::new("acme", "openai", None, None).with_catalog(Some("elsewhere"));
        assert_eq!(next_variant_in(&providers, &pc, "gpt-5.4", None), None);
        let pc = ProviderConfig::new("acme", "ollama", None, None);
        assert_eq!(
            next_variant_in(&providers, &pc, "gpt-5.4", None),
            None,
            "no catalog id and `ollama` is not a catalog entry here"
        );
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

    #[test]
    fn is_connectable_transport_rules() {
        let local = ProviderConfig::new("local", "ollama", None, None);
        assert!(is_connectable(&local));
        let remote = ProviderConfig::new("remote", "openai-compat", None, None);
        assert!(!is_connectable(&remote));
        let keyed = ProviderConfig::new("remote", "openai", Some("sk-x".into()), None);
        assert!(is_connectable(&keyed));
        let blank_key = ProviderConfig::new("remote", "anthropic", Some("  ".into()), None);
        assert!(!is_connectable(&blank_key));
    }

    #[test]
    fn is_connectable_legacy_catalog_kinds() {
        let groq = ProviderConfig::new("groq", "groq", None, None);
        assert!(!is_connectable(&groq), "catalog entry requires a key");
        let groq = ProviderConfig::new("groq", "groq", Some("gsk-x".into()), None);
        assert!(is_connectable(&groq));
    }

    #[test]
    fn is_connectable_explicit_catalog_governs() {
        let pc = ProviderConfig::new("groq-compat", "openai-compat", Some("gsk-x".into()), None)
            .with_catalog(Some("groq"));
        assert!(is_connectable(&pc));
        let pc = ProviderConfig::new("copilot", "openai-compat", None, None)
            .with_catalog(Some("copilot"));
        assert!(is_connectable(&pc), "catalog entry needs no key");
    }
}
