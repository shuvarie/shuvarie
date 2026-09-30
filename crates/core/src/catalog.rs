use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use selune::{Client, Provider};
use shuvarie_config::{Connections, CustomRegistry, ProviderConfig, RegistriesConfig};
use shuvarie_llm::TokenUsage;

const CACHE_READ_FACTOR: f64 = 0.1;
const REASONING_FACTOR: f64 = 0.6;

/// The registry id of the built-in hosted registry.
pub const SELUNE_REGISTRY: &str = "selune";

/// How long a remote fetch may run before the HTTP client gives up. The
/// caller wraps the blocking fetch in a task timeout too; this bounds the
/// underlying thread either way.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// Where a registry's online snapshot comes from. The built-in registry goes
/// through the selune client (honoring `CATALOG_URL`); custom registries fetch
/// their own URL with configured headers.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RemoteSource {
    /// The hosted selune service.
    Hosted,
    /// A custom registry's URL plus the headers to send with the fetch.
    Url {
        url: String,
        headers: Vec<(String, String)>,
    },
}

impl RemoteSource {
    fn fetch(&self) -> Result<Vec<Provider>, String> {
        match self {
            RemoteSource::Hosted => fetch_hosted(),
            RemoteSource::Url { url, headers } => fetch_url(url, headers),
        }
    }
}

/// Per-registry catalog state. Only enabled registries exist here: a disabled
/// registry is never registered (config `init` drops it), so it never loads,
/// fetches, or appears in lookups. The local snapshot is always available (for
/// the built-in registry it is the compile-time embedded catalog; for custom
/// registries the `path` file, loaded lazily on first use and cached, load
/// errors included). The remote snapshot holds the latest successful fetch
/// and is `None` until one succeeds.
struct RegistryState {
    id: String,
    /// Fetch the remote source at startup instead of on demand.
    remote_first: bool,
    /// The online source, when the registry has one.
    source: Option<RemoteSource>,
    /// The offline snapshot, kept after a successful load (or error).
    local: Vec<Provider>,
    /// The `path` file backing the local snapshot, when set.
    local_path: Option<std::path::PathBuf>,
    /// Whether the local snapshot has been loaded (`path` registries only).
    local_loaded: bool,
    /// Why the local snapshot failed to load, cached so a broken file is
    /// read once instead of on every access.
    local_error: Option<String>,
    /// The latest successful remote fetch.
    remote: Option<Vec<Provider>>,
}

impl RegistryState {
    /// The built-in registry: the embedded offline catalog and the hosted
    /// service as its online source.
    fn selune() -> Self {
        Self {
            id: SELUNE_REGISTRY.to_string(),
            remote_first: false,
            source: Some(RemoteSource::Hosted),
            local: selune::embedded::all(),
            local_path: None,
            local_loaded: true,
            local_error: None,
            remote: None,
        }
    }

    /// A custom registry from config. The local snapshot stays unloaded until
    /// first use.
    fn from_config(config: &CustomRegistry) -> Self {
        let source = config.url.as_ref().map(|url| RemoteSource::Url {
            url: url.clone(),
            headers: config.headers.clone(),
        });
        Self {
            id: config.name.clone(),
            remote_first: config.remote_first,
            source,
            local: Vec::new(),
            local_path: config.path.clone(),
            local_loaded: config.path.is_none(),
            local_error: None,
            remote: None,
        }
    }

    fn has_source(&self) -> bool {
        self.source.is_some()
    }

    /// Load the `path` snapshot on first use. A load error is cached: a
    /// broken file is reported once instead of being retried on every read.
    fn ensure_local(&mut self) {
        if self.local_loaded {
            return;
        }
        self.local_loaded = true;
        let path = self
            .local_path
            .as_deref()
            .map(|p| expand_tilde_in(Path::new(p), dirs::home_dir().as_deref()))
            .unwrap_or_default();
        match load_local_file(&path) {
            Ok(providers) => self.local = providers,
            Err(error) => self.local_error = Some(error),
        }
    }

    /// The effective providers of one registry: the remote snapshot once a
    /// fetch succeeded, else the local one.
    fn effective(&self) -> Vec<Provider> {
        self.remote.clone().unwrap_or_else(|| self.local.clone())
    }
}

/// Expand a leading `~` to the home directory; the rest is kept as written
/// (the same convention the trust store uses).
fn expand_tilde_in(path: &Path, home: Option<&Path>) -> std::path::PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        home.map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf())
    } else if let Some(rest) = text.strip_prefix("~/") {
        match home {
            Some(home) => home.join(rest),
            None => path.to_path_buf(),
        }
    } else {
        path.to_path_buf()
    }
}

struct CatalogState {
    registries: Vec<RegistryState>,
}

impl CatalogState {
    fn find(&self, id: &str) -> Option<&RegistryState> {
        self.registries.iter().find(|r| r.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut RegistryState> {
        self.registries.iter_mut().find(|r| r.id == id)
    }
}

/// Process-global provider catalog: the built-in registry plus every custom
/// one from `[registries]`, in config order. Metadata lookups ([`providers`])
/// see the union of all registries, each contributing its remote snapshot
/// once loaded, else its local snapshot — so a registry that was never
/// fetched contributes its offline data. First match wins.
fn state() -> &'static Mutex<CatalogState> {
    static STATE: OnceLock<Mutex<CatalogState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CatalogState {
            registries: vec![RegistryState::selune()],
        })
    })
}

fn lock() -> std::sync::MutexGuard<'static, CatalogState> {
    state()
        .lock()
        .expect("catalog mutex should not be poisoned")
}

/// Register the configured registries: the built-in one first, then every
/// custom `registry { … }` in config order. Flag-only reserved names have no
/// sources and are skipped, and a disabled registry is not registered at all —
/// it does not load, fetch, or appear in the union. Called once at core
/// startup; re-calls merge by id, keeping any loaded snapshots.
pub fn init(registries: &RegistriesConfig) {
    let mut state = lock();
    let selune_entry = registries.selune();
    if selune_entry.disabled {
        state.registries.retain(|r| r.id != SELUNE_REGISTRY);
    } else if let Some(existing) = state.find_mut(SELUNE_REGISTRY) {
        existing.remote_first = selune_entry.remote_first;
    } else {
        let mut built_in = RegistryState::selune();
        built_in.remote_first = selune_entry.remote_first;
        state.registries.insert(0, built_in);
    }
    for custom in &registries.custom {
        if custom.disabled {
            state.registries.retain(|r| r.id != custom.name);
            continue;
        }
        let fresh = RegistryState::from_config(custom);
        if let Some(existing) = state.find_mut(&custom.name) {
            if existing.source != fresh.source {
                existing.source = fresh.source;
                existing.remote = None;
            }
            if existing.local_path != fresh.local_path {
                existing.local_path = fresh.local_path;
                existing.local = Vec::new();
                existing.local_loaded = fresh.local_loaded;
                existing.local_error = None;
            }
            existing.remote_first = fresh.remote_first;
        } else {
            state.registries.push(fresh);
        }
    }
}

/// The configured registry ids in catalog order: the built-in one first,
/// then the custom ones in config order.
pub fn registry_ids() -> Vec<String> {
    lock().registries.iter().map(|r| r.id.clone()).collect()
}

/// A point-in-time view of one registry: what it offers offline and online.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistrySnapshot {
    /// The registry id (`selune` for the built-in one).
    pub id: String,
    /// The offline snapshot (the embedded catalog for the built-in registry,
    /// the `path` file for custom ones; empty when it failed to load).
    pub local: Vec<Provider>,
    /// Why the offline snapshot failed to load, when it did.
    pub local_error: Option<String>,
    /// The online snapshot, loaded once a fetch has succeeded.
    pub remote: Option<Vec<Provider>>,
    /// Whether the registry has an online source at all.
    pub remote_configured: bool,
}

impl RegistrySnapshot {
    /// The snapshot the registry popups show: remote once loaded, else local.
    pub fn effective(&self) -> &[Provider] {
        match &self.remote {
            Some(remote) => remote,
            None => &self.local,
        }
    }
}

/// Snapshot of one registry, loading its `path` file on first access.
/// Unknown ids yield an empty default snapshot.
pub fn registry_catalog(id: &str) -> RegistrySnapshot {
    let mut state = lock();
    let Some(registry) = state.find_mut(id) else {
        return RegistrySnapshot {
            id: id.to_string(),
            ..RegistrySnapshot::default()
        };
    };
    registry.ensure_local();
    RegistrySnapshot {
        id: registry.id.clone(),
        local: registry.local.clone(),
        local_error: registry.local_error.clone(),
        remote: registry.remote.clone(),
        remote_configured: registry.has_source(),
    }
}

/// Snapshot of the effective provider catalog: the union of all registries in
/// order, each contributing its remote snapshot once loaded, else its local
/// one. The built-in registry comes first, so its entries win id collisions
/// against custom registries.
pub fn providers() -> Vec<Provider> {
    let mut state = lock();
    for registry in &mut state.registries {
        registry.ensure_local();
    }
    state
        .registries
        .iter()
        .flat_map(RegistryState::effective)
        .collect()
}

/// Fetch one registry's online source, storing a successful result as its
/// remote snapshot. An empty response counts as a failure so a wrong URL
/// serving an empty list is reported instead of silently clearing the view.
/// A registry without an online source (or an unknown/disabled id) errors.
pub fn fetch_registry(id: &str) -> Result<Vec<Provider>, String> {
    let source = {
        let state = lock();
        match state.find(id) {
            Some(registry) => registry.source.clone(),
            None => return Err(format!("unknown registry `{id}`")),
        }
    };
    let Some(source) = source else {
        return Err(format!("registry `{id}` has no online source"));
    };
    let providers = source.fetch()?;
    if providers.is_empty() {
        return Err(format!("registry `{id}` returned no providers"));
    }
    store_remote(id, providers.clone());
    Ok(providers)
}

/// Store a registry's remote snapshot.
fn store_remote(id: &str, providers: Vec<Provider>) {
    if let Some(registry) = lock().find_mut(id) {
        registry.remote = Some(providers);
    }
}

/// Fetch a registry's remote source for the bounded startup refresh,
/// swallowing errors (the union falls back to the offline snapshot).
pub fn refresh_registry(id: &str) {
    let _ = fetch_registry(id);
}

/// The ids of registries with an online source that should be fetched at
/// startup (`remote-first`). Disabled registries are never registered, so
/// they cannot appear here.
pub fn remote_first_ids() -> Vec<String> {
    lock()
        .registries
        .iter()
        .filter(|r| r.remote_first && r.has_source())
        .map(|r| r.id.clone())
        .collect()
}

/// Fetch the hosted selune registry. An empty response is an error so the
/// caller can report it.
fn fetch_hosted() -> Result<Vec<Provider>, String> {
    match Client::new().get_providers() {
        Ok(providers) if !providers.is_empty() => Ok(providers),
        Ok(_) => Err("the hosted registry returned no providers".to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Fetch a custom registry from its URL, sending the configured headers with
/// `$ENV_VAR` placeholders (in the URL too) expanded from the environment.
fn fetch_url(url: &str, headers: &[(String, String)]) -> Result<Vec<Provider>, String> {
    let url = resolve_env(url);
    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.get(&url);
    for (name, value) in headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("invalid header name `{name}`: {e}"))?;
        let value = reqwest::header::HeaderValue::from_str(&resolve_env(value))
            .map_err(|e| format!("invalid header value for `{name}`: {e}"))?;
        request = request.header(name, value);
    }
    let response = request.send().map_err(|e| e.to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("unexpected status code: {}", status.as_u16()));
    }
    response.json::<Vec<Provider>>().map_err(|e| e.to_string())
}

/// Load a registry's offline snapshot from a local `providers.json` file
/// (the same format the registries serve).
fn load_local_file(path: &Path) -> Result<Vec<Provider>, String> {
    let json =
        std::fs::read_to_string(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_str(&json).map_err(|e| format!("parse {}: {e}", path.display()))
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

/// The transports rig implements the OAuth2 device flow on (ChatGPT,
/// Copilot). A capability check — what the client can run — not the auth
/// policy; see [`oauth_device_login_kind`] for that.
pub fn supports_device_flow(kind: selune::ProviderType) -> bool {
    matches!(
        kind,
        selune::ProviderType::Chatgpt | selune::ProviderType::Copilot
    )
}

/// Whether sign-in for a connection runs the OAuth2 device flow instead of
/// asking for an API key. The Selune catalog entry for the connection's
/// catalog id governs via its `auth` field; a kind with no resolvable catalog
/// entry falls back to [`supports_device_flow`].
pub fn oauth_device_login(pc: &ProviderConfig) -> bool {
    oauth_device_login_kind(&pc.kind, pc.catalog.as_deref())
}

/// [`oauth_device_login`] for a raw kind + optional Selune catalog id.
pub fn oauth_device_login_kind(kind: &str, catalog: Option<&str>) -> bool {
    oauth_device_login_kind_in(&providers(), kind, catalog)
}

fn oauth_device_login_kind_in(providers: &[Provider], kind: &str, catalog: Option<&str>) -> bool {
    match find_provider(providers, catalog.unwrap_or(kind)) {
        Some(entry) => entry.oauth_device_login(),
        None => parse_provider_type(kind).is_some_and(supports_device_flow),
    }
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
        Some(selune::ProviderType::Ollama | selune::ProviderType::Llamafile) => true,
        // OAuth-backed subscription providers: connectable without a pasted
        // API key; sign-in resolves lazily through rig's device-flow cache.
        Some(_) if oauth_device_login(provider) => true,
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
        selune::ProviderType::Chatgpt => "chatgpt",
        selune::ProviderType::Copilot => "copilot",
        selune::ProviderType::Cohere => "cohere",
        selune::ProviderType::Deepseek => "deepseek",
        selune::ProviderType::Doubleword => "doubleword",
        selune::ProviderType::Groq => "groq",
        selune::ProviderType::Huggingface => "huggingface",
        selune::ProviderType::Hyperbolic => "hyperbolic",
        selune::ProviderType::Llamafile => "llamafile",
        selune::ProviderType::Minimax => "minimax",
        selune::ProviderType::Mira => "mira",
        selune::ProviderType::Mistral => "mistral",
        selune::ProviderType::Moonshot => "moonshot",
        selune::ProviderType::Perplexity => "perplexity",
        selune::ProviderType::Together => "together",
        selune::ProviderType::Venice => "venice",
        selune::ProviderType::Voyageai => "voyageai",
        selune::ProviderType::Xai => "xai",
        selune::ProviderType::Xiaomimimo => "xiaomimimo",
        selune::ProviderType::Zai => "zai",
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
    use shuvarie_config::RegistryEntry;

    fn catalog_provider(
        id: &str,
        r#type: Option<ProviderType>,
        endpoint: Option<&str>,
    ) -> Provider {
        Provider {
            name: id.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: None,
            auth: None,
            api_endpoint: endpoint.map(str::to_string),
            r#type,
            doc: None,
            default_large_model_id: None,
            default_small_model_id: None,
            models: Vec::new(),
            default_headers: None,
        }
    }

    fn model_code(id: &str) -> selune::ModelCode {
        let (body, variant) = id.split_once(':').unwrap_or((id, ""));
        let (org, model) = body.split_once('/').unwrap_or(("test-org", body));
        let variant = (!variant.is_empty()).then_some(variant);
        selune::ModelCode::new(org, model, variant).unwrap()
    }

    fn test_provider(id: &str, models: impl IntoIterator<Item = (&'static str, i64)>) -> Provider {
        Provider {
            models: models
                .into_iter()
                .map(|(model, context)| selune::Model {
                    id: model.to_string(),
                    model_code: model_code(model),
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
            model_code: model_code(id),
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
            ("chatgpt", ProviderType::Chatgpt),
            ("copilot", ProviderType::Copilot),
            ("cohere", ProviderType::Cohere),
            ("deepseek", ProviderType::Deepseek),
            ("doubleword", ProviderType::Doubleword),
            ("groq", ProviderType::Groq),
            ("huggingface", ProviderType::Huggingface),
            ("hyperbolic", ProviderType::Hyperbolic),
            ("llamafile", ProviderType::Llamafile),
            ("minimax", ProviderType::Minimax),
            ("mira", ProviderType::Mira),
            ("mistral", ProviderType::Mistral),
            ("moonshot", ProviderType::Moonshot),
            ("perplexity", ProviderType::Perplexity),
            ("together", ProviderType::Together),
            ("venice", ProviderType::Venice),
            ("voyageai", ProviderType::Voyageai),
            ("xai", ProviderType::Xai),
            ("xiaomimimo", ProviderType::Xiaomimimo),
            ("zai", ProviderType::Zai),
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
        let chatgpt = ProviderConfig::new("chatgpt", "chatgpt", None, None);
        assert!(is_connectable(&chatgpt), "OAuth-backed: no key required");
        let copilot = ProviderConfig::new("copilot", "copilot", None, None);
        assert!(is_connectable(&copilot), "OAuth-backed: no key required");
        let copilot_keyed = ProviderConfig::new("copilot", "copilot", Some("ghp-x".into()), None);
        assert!(is_connectable(&copilot_keyed));
        let llamafile = ProviderConfig::new("llamafile", "llamafile", None, None);
        assert!(is_connectable(&llamafile), "local runtime: no key required");
        let deepseek = ProviderConfig::new("deepseek", "deepseek", None, None);
        assert!(!is_connectable(&deepseek), "keyed transport needs a key");
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

    #[test]
    fn oauth_device_login_reads_the_catalog_auth_field() {
        let mut chatgpt = catalog_provider("chatgpt", Some(ProviderType::Chatgpt), None);
        chatgpt.auth = Some(selune::AuthMethod::Oauth2Device);
        let providers = vec![
            chatgpt,
            catalog_provider("openai", Some(ProviderType::Openai), None),
        ];
        assert!(oauth_device_login_kind_in(&providers, "chatgpt", None));
        assert!(oauth_device_login_kind_in(
            &providers,
            "chatgpt",
            Some("chatgpt")
        ));
        assert!(!oauth_device_login_kind_in(&providers, "openai", None));
        // An unmarked entry governs even when the transport is device-flow
        // capable.
        assert!(!oauth_device_login_kind_in(
            &providers,
            "chatgpt",
            Some("openai")
        ));
        // No resolvable entry falls back to the transport capability.
        assert!(oauth_device_login_kind_in(
            &providers,
            "chatgpt",
            Some("missing")
        ));
        assert!(!oauth_device_login_kind_in(
            &providers,
            "openai",
            Some("missing")
        ));
    }

    #[test]
    fn oauth_device_login_embedded_catalog_marks_the_subscription_providers() {
        assert!(oauth_device_login_kind("chatgpt", None));
        assert!(oauth_device_login_kind("copilot", None));
        assert!(!oauth_device_login_kind("anthropic", None));
        assert!(!oauth_device_login_kind("not-a-kind", None));
    }

    // --- multi-registry state -------------------------------------------

    use std::sync::Arc;

    /// Serializes the tests that mutate the process-global catalog. Locks
    /// through poison: one test's panic must not cascade into unrelated
    /// failures.
    static GLOBAL: Mutex<()> = Mutex::new(());

    fn global_lock() -> std::sync::MutexGuard<'static, ()> {
        match GLOBAL.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Drops every registry but the built-in one, clearing its remote.
    fn reset_catalog() {
        let mut state = lock();
        state.registries = vec![RegistryState::selune()];
    }

    fn custom_registry(
        name: &str,
        url: Option<&str>,
        path: Option<&std::path::Path>,
    ) -> CustomRegistry {
        CustomRegistry {
            name: name.to_string(),
            url: url.map(str::to_string),
            path: path.map(Path::to_path_buf),
            headers: Vec::new(),
            disabled: false,
            remote_first: false,
        }
    }

    fn config_with(customs: &[CustomRegistry]) -> RegistriesConfig {
        RegistriesConfig {
            entries: Default::default(),
            custom: customs.to_vec(),
        }
    }

    fn provider_json(id: &str, name: &str) -> String {
        format!("[{{\"name\":\"{name}\",\"id\":\"{id}\",\"models\":[]}}]")
    }

    /// A mock registry server: serves one fixed body, recording every
    /// request's head (method, path, headers) for assertions.
    struct MockRegistry {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn spawn_mock_registry(body: &'static str, status: u16) -> MockRegistry {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let requests: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = requests.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    break;
                };
                use std::io::{Read, Write};
                let mut buf = [0u8; 8192];
                let n = stream.read(&mut buf).unwrap_or(0);
                sink.lock()
                    .expect("request sink poisoned")
                    .push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
                let response = format!(
                    "HTTP/1.1 {status} test\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        MockRegistry {
            url: format!("http://{addr}/providers.json"),
            requests,
        }
    }

    #[test]
    fn expand_tilde_resolves_the_home_prefix() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(
            expand_tilde_in(Path::new("~"), home),
            std::path::PathBuf::from("/home/u")
        );
        assert_eq!(
            expand_tilde_in(Path::new("~/providers.json"), home),
            std::path::PathBuf::from("/home/u/providers.json")
        );
        assert_eq!(
            expand_tilde_in(Path::new("/abs/providers.json"), home),
            std::path::PathBuf::from("/abs/providers.json")
        );
        assert_eq!(
            expand_tilde_in(Path::new("~"), None),
            std::path::PathBuf::from("~"),
            "without a home the path is kept as written"
        );
    }

    #[test]
    fn load_local_file_parses_a_provider_json_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(&path, provider_json("acme", "Acme")).unwrap();
        let providers = load_local_file(&path).expect("loads");
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id.0, "acme");
    }

    #[test]
    fn load_local_file_reports_missing_and_broken_files() {
        let missing =
            load_local_file(Path::new("/nonexistent/providers.json")).expect_err("missing file");
        assert!(missing.contains("read"), "{missing}");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(&path, "{\"name\":\"x\"}").unwrap();
        let broken = load_local_file(&path).expect_err("an object is not a registry");
        assert!(broken.contains("parse"), "{broken}");
    }

    #[test]
    fn fetch_url_sends_configured_headers_with_env_expanded() {
        unsafe { std::env::set_var("SHUVARIE_TEST_TOKEN", "bearer-token") };
        let mock = spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme\",\"models\":[]}]", 200);
        let providers = fetch_url(
            &mock.url,
            &[
                (
                    "Authorization".to_string(),
                    "Bearer $SHUVARIE_TEST_TOKEN".to_string(),
                ),
                ("X-Static".to_string(), "static".to_string()),
            ],
        )
        .expect("fetches");
        assert_eq!(providers.len(), 1);
        unsafe { std::env::remove_var("SHUVARIE_TEST_TOKEN") };

        let requests = mock.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "one request");
        assert!(
            requests[0].starts_with("get /providers.json"),
            "{}",
            requests[0]
        );
        assert!(
            requests[0].contains("authorization: bearer bearer-token"),
            "expanded env var: {}",
            requests[0]
        );
        assert!(requests[0].contains("x-static: static"), "{}", requests[0]);
    }

    #[test]
    fn fetch_url_reports_status_and_decode_errors() {
        let mock = spawn_mock_registry("[]", 500);
        let error = fetch_url(&mock.url, &[]).expect_err("status error");
        assert_eq!(error, "unexpected status code: 500");

        let mock = spawn_mock_registry("<html>not json</html>", 200);
        let error = fetch_url(&mock.url, &[]).expect_err("decode error");
        assert!(error.contains("decoding"), "{}", error);
    }

    #[test]
    fn init_registers_selune_then_customs_in_config_order() {
        let _guard = global_lock();
        reset_catalog();
        init(&config_with(&[
            custom_registry("first", Some("https://example.test/providers.json"), None),
            custom_registry("second", None, None),
        ]));
        assert_eq!(
            registry_ids(),
            vec!["selune", "first", "second"],
            "built-in first, then config order"
        );
    }

    #[test]
    fn init_merges_by_id_keeping_loaded_snapshots() {
        let _guard = global_lock();
        reset_catalog();
        let custom = custom_registry("acme", Some("https://example.test/x.json"), None);
        init(&config_with(&[custom.clone()]));
        store_remote("acme", vec![test_provider("acme-entry", [])]);
        let mut changed = custom.clone();
        changed.remote_first = true;
        init(&config_with(&[changed]));
        assert_eq!(
            registry_ids(),
            vec!["selune", "acme"],
            "no duplicate registration"
        );
        assert!(registry_catalog("acme").remote.is_some(), "snapshot kept");
        let flags = {
            let state = lock();
            state.find("acme").map(|r| r.remote_first)
        };
        assert_eq!(flags, Some(true), "flags refreshed from config");
    }

    #[test]
    fn init_does_not_register_disabled_registries() {
        let _guard = global_lock();
        reset_catalog();
        let mut disabled = custom_registry("sleepy", Some("https://example.test/x.json"), None);
        disabled.disabled = true;
        let mut config = config_with(&[disabled]);
        config.entries.insert(
            SELUNE_REGISTRY.to_string(),
            RegistryEntry {
                disabled: true,
                remote_first: false,
            },
        );
        init(&config);
        assert_eq!(registry_ids(), Vec::<String>::new(), "disabled ids vanish");
        assert!(
            providers().is_empty(),
            "nothing loads from a disabled registry"
        );
        assert_eq!(
            registry_catalog("sleepy").local,
            Vec::new(),
            "an unregistered registry has no snapshot data"
        );

        // Re-enabling brings the built-in registry (and its embedded local
        // snapshot) back.
        config.entries.insert(
            SELUNE_REGISTRY.to_string(),
            RegistryEntry {
                disabled: false,
                remote_first: false,
            },
        );
        init(&config);
        assert_eq!(registry_ids(), vec![SELUNE_REGISTRY]);
        assert!(!registry_catalog(SELUNE_REGISTRY).local.is_empty());
    }

    #[test]
    fn union_joins_registries_in_order_remote_over_local() {
        let _guard = global_lock();
        reset_catalog();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(&path, provider_json("acme-local", "Acme Local")).unwrap();
        init(&config_with(&[custom_registry("acme", None, Some(&path))]));

        let ids =
            |providers: &[Provider]| providers.iter().map(|p| p.id.0.clone()).collect::<Vec<_>>();
        let custom_first = ids(&providers())
            .into_iter()
            .rev()
            .take(1)
            .collect::<Vec<_>>()
            .pop();
        assert_eq!(
            custom_first.as_deref(),
            Some("acme-local"),
            "path loads lazily"
        );
        let selune_len = registry_catalog(SELUNE_REGISTRY).local.len();
        let custom_at = providers()
            .iter()
            .position(|p| p.id.0 == "acme-local")
            .expect("the custom registry's local snapshot is in the union");
        assert!(
            custom_at >= selune_len,
            "the built-in registry leads the union"
        );

        let remote = vec![test_provider("acme-remote", [])];
        store_remote("acme", remote);
        assert!(
            providers().iter().any(|p| p.id.0 == "acme-remote"),
            "the loaded remote replaces the local snapshot"
        );
        assert!(
            !providers().iter().any(|p| p.id.0 == "acme-local"),
            "per registry, remote wins over local"
        );
    }

    #[test]
    fn registry_catalog_loads_the_path_snapshot_once() {
        let _guard = global_lock();
        reset_catalog();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        std::fs::write(&path, provider_json("acme-local", "Acme Local")).unwrap();
        init(&config_with(&[custom_registry("acme", None, Some(&path))]));

        let first = registry_catalog("acme");
        assert_eq!(first.local.len(), 1, "the path file loads on first access");
        assert_eq!(first.local_error, None);
        assert_eq!(first.remote_configured, false, "path-only registry");
        assert_eq!(first.effective().len(), 1);

        std::fs::write(&path, "<html>").unwrap();
        let second = registry_catalog("acme");
        assert_eq!(
            second.local.len(),
            1,
            "the loaded snapshot is kept; the file is not re-read"
        );
    }

    #[test]
    fn registry_catalog_caches_a_load_error() {
        let _guard = global_lock();
        reset_catalog();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("providers.json");
        init(&config_with(&[custom_registry("acme", None, Some(&path))]));

        let first = registry_catalog("acme");
        let error = first.local_error.expect("missing file is an error");
        assert!(error.contains("read"), "{error}");
        assert!(first.local.is_empty());

        std::fs::write(&path, provider_json("acme-local", "Acme")).unwrap();
        let second = registry_catalog("acme");
        assert!(
            second.local_error.is_some(),
            "the error is cached; the file is not retried"
        );
        assert!(second.local.is_empty());
    }

    #[test]
    fn registry_catalog_unknown_id_is_an_empty_snapshot() {
        let _guard = global_lock();
        reset_catalog();
        let snapshot = registry_catalog("nowhere");
        assert_eq!(snapshot.id, "nowhere");
        assert!(snapshot.local.is_empty());
        assert!(snapshot.remote.is_none());
        assert!(!snapshot.remote_configured);
    }

    #[test]
    fn fetch_registry_gates_unknown_and_sourceless() {
        let _guard = global_lock();
        reset_catalog();
        init(&config_with(&[custom_registry("local-only", None, None)]));

        assert_eq!(
            fetch_registry("nowhere"),
            Err("unknown registry `nowhere`".to_string())
        );
        assert_eq!(
            fetch_registry("local-only"),
            Err("registry `local-only` has no online source".to_string())
        );
    }

    #[test]
    fn fetch_registry_reports_a_disabled_registry_as_unknown() {
        let _guard = global_lock();
        reset_catalog();
        let mut disabled = custom_registry("acme", Some("https://example.test/x.json"), None);
        disabled.disabled = true;
        init(&config_with(&[disabled]));
        assert_eq!(
            fetch_registry("acme"),
            Err("unknown registry `acme`".to_string()),
            "a disabled registry is not registered, so it cannot fetch"
        );
    }

    #[test]
    fn fetch_registry_stores_the_remote_snapshot() {
        let _guard = global_lock();
        reset_catalog();
        let mock = spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme\",\"models\":[]}]", 200);
        let mut custom = custom_registry("acme", Some(&mock.url), None);
        custom.headers = vec![("X-Custom".to_string(), "1".to_string())];
        init(&config_with(&[custom]));

        let providers = fetch_registry("acme").expect("fetches");
        assert_eq!(providers.len(), 1);
        assert_eq!(
            registry_catalog("acme").remote.as_ref().map(|p| p.len()),
            Some(1)
        );
        assert!(
            mock.requests.lock().unwrap()[0].contains("x-custom: 1"),
            "config headers are sent"
        );
    }

    #[test]
    fn fetch_registry_reports_an_empty_remote() {
        let _guard = global_lock();
        reset_catalog();
        let mock = spawn_mock_registry("[]", 200);
        init(&config_with(&[custom_registry(
            "acme",
            Some(&mock.url),
            None,
        )]));
        assert_eq!(
            fetch_registry("acme"),
            Err("registry `acme` returned no providers".to_string())
        );
        assert!(registry_catalog("acme").remote.is_none());
    }

    #[test]
    fn remote_first_ids_list_sourced_registries() {
        let _guard = global_lock();
        reset_catalog();
        let mut remote_first = custom_registry("rf", Some("https://example.test/x.json"), None);
        remote_first.remote_first = true;
        let mut offline = custom_registry("local-only", None, None);
        offline.remote_first = true;
        init(&config_with(&[remote_first, offline]));
        assert_eq!(
            remote_first_ids(),
            vec!["rf"],
            "sourceless registries never fetch at startup"
        );
    }

    #[test]
    fn selune_registry_snapshot_carries_the_embedded_catalog() {
        let _guard = global_lock();
        reset_catalog();
        let snapshot = registry_catalog(SELUNE_REGISTRY);
        assert_eq!(snapshot.id, SELUNE_REGISTRY);
        assert!(snapshot.remote_configured);
        assert!(snapshot.local_error.is_none());
        assert!(
            !snapshot.local.is_empty(),
            "the embedded catalog is preloaded"
        );
        assert_eq!(snapshot.effective().len(), snapshot.local.len());
    }
}
