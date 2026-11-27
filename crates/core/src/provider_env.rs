//! Scanning the environment for provider API keys.
//!
//! Selune's catalog names the variable each provider reads its key from (its
//! `api_key` field holds a reference such as `$OPENAI_API_KEY`, never a
//! secret), so a key exported in the shell is enough to configure that
//! provider without writing anything into `connections.kdl`. Both
//! `/scan-providers` and `shuvarie provider scan` work from [`scan`], then
//! register what it found with [`plan`] and [`apply`] — the same two steps, so
//! the dialog and the CLI cannot drift apart.
//!
//! Detection is read-only and deliberately limited to what the catalog
//! declares: a variable no catalog entry names is not guessed at, because a
//! connection needs a transport and an endpoint to be of any use.

use selune::Provider;
use shuvarie_config::{Active, Connections, ProviderConfig, normalize_env_name};
use shuvarie_llm::ProviderKind;

use crate::catalog;

/// A catalog provider whose key variable holds a value in this process and
/// which `connections` has no connection for yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedProvider {
    /// The Selune catalog id (`anthropic`, `ollama-cloud`).
    pub catalog_id: String,
    /// The catalog's display name, reused as the connection's name.
    pub name: String,
    /// The variable holding the key, as a bare name (the sigil stripped).
    pub env: String,
    /// The transport the connection is built on, in kebab-case.
    pub kind: String,
    /// The catalog's endpoint, when it is a literal one. A transport like
    /// `ollama` has its own default (a local server), so a hosted entry riding
    /// that transport — Ollama Cloud — is only reachable through the catalog's
    /// URL. A templated endpoint (`$AZURE_API_ENDPOINT`) is left out: the same
    /// choice the add-provider form makes, since an unresolved placeholder is
    /// not a URL.
    pub base_url: Option<String>,
}

impl DetectedProvider {
    /// The connection this detection registers: the catalog entry as its
    /// origin, the variable as its key source, and the catalog's endpoint when
    /// it names one.
    pub fn provider_config(&self) -> ProviderConfig {
        ProviderConfig::new(
            self.name.clone(),
            self.kind.clone(),
            None,
            self.base_url.clone(),
        )
        .with_catalog(Some(self.catalog_id.clone()))
        .with_api_key_env(Some(&self.env))
    }
}

/// Every catalog provider whose key variable is set here, minus the ones
/// `connections` already registers — by catalog id, or by the variable it
/// reads.
pub fn scan(connections: &Connections) -> Vec<DetectedProvider> {
    scan_in(&catalog::providers(), connections)
}

/// [`scan`] against an explicit catalog, so tests do not depend on which
/// providers the embedded registry happens to ship.
fn scan_in(providers: &[Provider], connections: &Connections) -> Vec<DetectedProvider> {
    let mut detected: Vec<DetectedProvider> = Vec::new();
    for provider in providers {
        // A provider that takes no key has nothing to look for.
        let Some(env) = provider.api_key.as_deref().and_then(normalize_env_name) else {
            continue;
        };
        if !env_is_set(&env) {
            continue;
        }
        let catalog_id = provider.id.0.as_str();
        // The union of the registries can carry one id twice (a custom registry
        // repeating a built-in entry), and two entries may name the same
        // variable: one connection per key is enough.
        if detected
            .iter()
            .any(|d| d.env == env || d.catalog_id == catalog_id)
        {
            continue;
        }
        if connections
            .providers
            .values()
            .any(|pc| registers(pc, catalog_id, &env))
        {
            continue;
        }
        detected.push(DetectedProvider {
            catalog_id: catalog_id.to_string(),
            name: provider.name.clone(),
            kind: transport(provider),
            env,
            base_url: catalog::api_endpoint(provider).filter(|url| !url.contains('$')),
        });
    }
    detected
}

/// Whether a registered connection already covers this catalog entry or key
/// variable.
fn registers(pc: &ProviderConfig, catalog_id: &str, env: &str) -> bool {
    // `catalog_id()` falls back to `kind`, which is where older pacts kept the
    // catalog id, so a config written before the `catalog` field existed still
    // counts as covering its provider.
    if pc.catalog_id() == Some(catalog_id) {
        return true;
    }
    pc.api_key_env
        .as_deref()
        .and_then(normalize_env_name)
        .as_deref()
        == Some(env)
}

/// Whether a variable holds a non-blank value here.
fn env_is_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.trim().is_empty())
}

/// The connection kind for a catalog entry: its transport in kebab-case, or —
/// for an entry that names no type — the catalog id itself, which
/// [`catalog::parse_provider_kind`] resolves back through the catalog. This is
/// the same kind the add-provider dialog prefill uses.
fn transport(provider: &Provider) -> String {
    match provider.r#type {
        Some(kind) => catalog::provider_kind_name(ProviderKind::new(kind, provider.dialect)),
        None => provider.id.0.clone(),
    }
}

/// The connection rows to register for a scan: a fresh id (the `connections.kdl`
/// key) per detection, and the config behind it.
pub fn plan(detected: &[DetectedProvider]) -> Vec<(String, ProviderConfig)> {
    detected
        .iter()
        .map(|d| (uuid::Uuid::now_v7().to_string(), d.provider_config()))
        .collect()
}

/// Insert [`plan`]'s rows into `connections`, replacing any row with the same
/// id. The first row becomes the active provider when none is active yet: a
/// scan that registers the only provider on the machine should leave the user
/// ready to send, not waiting on a picker they did not ask for.
pub fn apply(connections: &mut Connections, rows: &[(String, ProviderConfig)]) {
    let first = rows.first().map(|(id, _)| id.clone());
    for (id, config) in rows {
        connections.providers.insert(id.clone(), config.clone());
    }
    if connections.active.is_none()
        && let Some(provider) = first
    {
        connections.active = Some(Active {
            provider,
            model: None,
            variant: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use selune::{InferenceProvider, ProviderType};

    fn key_provider(
        id: &str,
        name: &str,
        api_key: Option<&str>,
        kind: Option<ProviderType>,
    ) -> Provider {
        Provider {
            name: name.to_string(),
            id: InferenceProvider(id.to_string()),
            api_key: api_key.map(str::to_string),
            auth: None,
            api_endpoint: None,
            r#type: kind,
            dialect: None,
            doc: None,
            peaks: None,
            default_large_model_id: None,
            default_small_model_id: None,
            models: Vec::new(),
            default_headers: None,
        }
    }

    /// The environment is process-wide and tests run in parallel, so every test
    /// that exports a variable owns one of its own.
    const SET: &str = "SHUVARIE_TEST_SCAN_KEY";
    const REGISTERED: &str = "SHUVARIE_TEST_SCAN_KEY_REGISTERED";
    const REGISTERED_OTHER: &str = "SHUVARIE_TEST_SCAN_KEY_REGISTERED_OTHER";
    const DUPLICATE: &str = "SHUVARIE_TEST_SCAN_KEY_DUPLICATE";
    const KINDLESS: &str = "SHUVARIE_TEST_SCAN_KEY_KINDLESS";
    const ENDPOINT_KEY: &str = "SHUVARIE_TEST_SCAN_KEY_ENDPOINT";

    fn connections_with(catalog: Option<&str>, env: Option<&str>) -> Connections {
        let mut connections = Connections::default();
        let pc = ProviderConfig::new("existing", "openai", None, None)
            .with_catalog(catalog)
            .with_api_key_env(env);
        connections.providers.insert("existing".into(), pc);
        connections
    }

    /// The provider with a catalog endpoint, as the registry serves it.
    fn endpoint(mut provider: Provider, url: &str) -> Provider {
        provider.api_endpoint = Some(url.to_string());
        provider
    }

    #[test]
    fn scan_reports_a_set_key_and_skips_unset_ones() {
        // SAFETY: the test owns these variables for its whole body.
        unsafe { std::env::set_var(SET, "sk-live") };
        let providers = vec![
            key_provider(
                "acme",
                "Acme",
                Some(&format!("${SET}")),
                Some(ProviderType::OpenaiCompat),
            ),
            key_provider(
                "quiet",
                "Quiet",
                Some("$SHUVARIE_TEST_SCAN_KEY_UNSET"),
                Some(ProviderType::OpenaiCompat),
            ),
            key_provider("local", "Local", None, Some(ProviderType::Ollama)),
        ];

        let detected = scan_in(&providers, &Connections::default());
        assert_eq!(
            detected,
            vec![DetectedProvider {
                catalog_id: "acme".into(),
                name: "Acme".into(),
                env: SET.into(),
                kind: "openai-compat".into(),
                base_url: None,
            }],
            "only the provider with a set key is reported"
        );

        unsafe { std::env::remove_var(SET) };
        assert!(
            scan_in(&providers, &Connections::default()).is_empty(),
            "the key is gone, so nothing is left to add"
        );
    }

    #[test]
    fn scan_skips_a_provider_that_is_already_registered() {
        // SAFETY: the test owns these variables for its whole body.
        unsafe {
            std::env::set_var(REGISTERED, "sk-live");
            std::env::set_var(REGISTERED_OTHER, "sk-other");
        }
        let providers = vec![
            key_provider(
                "acme",
                "Acme",
                Some(&format!("${REGISTERED}")),
                Some(ProviderType::OpenaiCompat),
            ),
            key_provider(
                "other",
                "Other",
                Some(&format!("${REGISTERED_OTHER}")),
                Some(ProviderType::OpenaiCompat),
            ),
        ];

        let by_catalog = connections_with(Some("acme"), None);
        let detected = scan_in(&providers, &by_catalog);
        assert_eq!(detected.len(), 1, "the catalog id is already registered");
        assert_eq!(detected[0].catalog_id, "other");

        // The variable alone is enough: a connection may name any catalog id
        // and still be reading the same key.
        let by_env = connections_with(None, Some(REGISTERED));
        let detected = scan_in(&providers, &by_env);
        assert_eq!(detected.len(), 1, "the variable is already registered");
        assert_eq!(detected[0].catalog_id, "other");

        // A pact written before the `catalog` field existed keeps its catalog
        // id in `kind`, and still covers that provider.
        let mut legacy = Connections::default();
        legacy.providers.insert(
            "legacy".into(),
            ProviderConfig::new("Acme (hand-written)", "acme", Some("sk-old".into()), None),
        );
        let detected = scan_in(&providers, &legacy);
        assert_eq!(detected.len(), 1, "the legacy pact covers acme");
        assert_eq!(detected[0].catalog_id, "other");

        unsafe {
            std::env::remove_var(REGISTERED);
            std::env::remove_var(REGISTERED_OTHER);
        }
    }

    #[test]
    fn a_hosted_entry_riding_a_local_transport_keeps_its_endpoint() {
        // SAFETY: the test owns this variable for its whole body.
        unsafe { std::env::set_var(ENDPOINT_KEY, "sk-live") };
        // Ollama Cloud is a hosted service on the `ollama` transport, whose
        // default endpoint is a local server: the catalog's URL has to come
        // with the connection, exactly as the add-provider form prefills it.
        let cloud = endpoint(
            key_provider(
                "ollama-cloud",
                "Ollama Cloud",
                Some(&format!("${ENDPOINT_KEY}")),
                Some(ProviderType::Ollama),
            ),
            "https://ollama.com",
        );

        let detected = scan_in(std::slice::from_ref(&cloud), &Connections::default());
        assert_eq!(detected[0].kind, "ollama");
        assert_eq!(detected[0].base_url.as_deref(), Some("https://ollama.com"));
        assert_eq!(
            detected[0].provider_config().base_url.as_deref(),
            Some("https://ollama.com"),
            "the connection carries the endpoint"
        );

        // A templated endpoint is not a URL, so it is left for the catalog to
        // resolve at connect time — the form's rule too.
        let templated = endpoint(cloud, "$SHUVARIE_TEST_SCAN_ENDPOINT");
        let detected = scan_in(&[templated], &Connections::default());
        assert_eq!(detected[0].base_url, None);

        unsafe { std::env::remove_var(ENDPOINT_KEY) };
    }

    #[test]
    fn scan_reports_each_key_once() {
        // SAFETY: the test owns this variable for its whole body.
        unsafe { std::env::set_var(DUPLICATE, "sk-live") };
        // Two registries repeating one entry, and a second entry naming the
        // same variable: a scan must not offer the same key twice.
        let providers = vec![
            key_provider(
                "acme",
                "Acme",
                Some(&format!("${DUPLICATE}")),
                Some(ProviderType::OpenaiCompat),
            ),
            key_provider(
                "acme",
                "Acme",
                Some(&format!("${DUPLICATE}")),
                Some(ProviderType::OpenaiCompat),
            ),
            key_provider(
                "acme-alt",
                "Acme Alt",
                Some(&format!("${{{DUPLICATE}}}")),
                Some(ProviderType::OpenaiCompat),
            ),
        ];

        let detected = scan_in(&providers, &Connections::default());
        assert_eq!(detected.len(), 1, "{detected:?}");
        assert_eq!(detected[0].env, DUPLICATE, "the sigil forms agree");

        unsafe { std::env::remove_var(DUPLICATE) };
    }

    #[test]
    fn a_kind_less_entry_keeps_its_catalog_id_as_the_transport() {
        // SAFETY: the test owns this variable for its whole body.
        unsafe { std::env::set_var(KINDLESS, "sk-live") };
        let providers = vec![key_provider(
            "acme",
            "Acme",
            Some(&format!("${KINDLESS}")),
            None,
        )];

        let detected = scan_in(&providers, &Connections::default());
        assert_eq!(detected[0].kind, "acme", "the legacy catalog-id kind");

        unsafe { std::env::remove_var(KINDLESS) };
    }

    #[test]
    fn plan_and_apply_register_every_detection_once() {
        // Nothing here reads the environment: the names are just what a scan
        // would have reported.
        let detected = vec![
            DetectedProvider {
                catalog_id: "acme".into(),
                name: "Acme".into(),
                env: "ACME_API_KEY".into(),
                kind: "openai-compat".into(),
                base_url: None,
            },
            DetectedProvider {
                catalog_id: "beta".into(),
                name: "Beta".into(),
                env: "BETA_API_KEY".into(),
                kind: "anthropic".into(),
                base_url: Some("https://beta.example/v1".into()),
            },
        ];
        let rows = plan(&detected);
        assert_eq!(rows.len(), 2);
        assert_ne!(rows[0].0, rows[1].0, "each row gets its own connection id");
        let added = &rows[0].1;
        assert_eq!(added.name, "Acme");
        assert_eq!(added.kind, "openai-compat");
        assert_eq!(added.catalog.as_deref(), Some("acme"));
        assert_eq!(added.api_key_env.as_deref(), Some("ACME_API_KEY"));
        assert!(added.api_key.is_none(), "the secret stays in the shell");
        assert!(
            added.base_url.is_none(),
            "an entry with no endpoint of its own leaves the transport's default alone"
        );
        assert_eq!(
            rows[1].1.base_url.as_deref(),
            Some("https://beta.example/v1"),
            "a literal catalog endpoint rides along"
        );

        let mut connections = Connections::default();
        apply(&mut connections, &rows);
        assert_eq!(connections.providers.len(), 2);
        assert!(
            connections.providers.contains_key(&rows[1].0),
            "every row was inserted"
        );
        assert_eq!(
            connections
                .active
                .as_ref()
                .map(|a| (a.provider.as_str(), a.model.as_deref())),
            Some((rows[0].0.as_str(), None)),
            "the first row becomes active, without a model"
        );
    }

    #[test]
    fn apply_keeps_an_active_provider_alone() {
        let mut connections = Connections::default();
        connections.providers.insert(
            "mine".into(),
            ProviderConfig::new("Mine", "openai", Some("sk-x".into()), None),
        );
        connections.active = Some(Active {
            provider: "mine".into(),
            model: Some("gpt-4o".into()),
            variant: None,
        });

        let rows = plan(&[DetectedProvider {
            catalog_id: "acme".into(),
            name: "Acme".into(),
            env: "ACME_API_KEY".into(),
            kind: "openai-compat".into(),
            base_url: None,
        }]);
        apply(&mut connections, &rows);

        assert_eq!(
            connections.active.as_ref().map(|a| a.provider.as_str()),
            Some("mine"),
            "a configured machine keeps its active provider"
        );
        assert_eq!(
            connections.active.as_ref().and_then(|a| a.model.as_deref()),
            Some("gpt-4o")
        );
    }

    #[test]
    fn apply_of_nothing_changes_nothing() {
        let mut connections = Connections::default();
        apply(&mut connections, &[]);
        assert!(connections.providers.is_empty());
        assert!(connections.active.is_none());
    }
}
