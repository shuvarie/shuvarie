use selune::Provider;
use shuvarie_core::catalog::{SELUNE_REGISTRY, registry_catalog};
use shuvarie_core::{CustomRegistry, RegistriesConfig, RegistryEntry};

/// The display name for the built-in registry's group.
const SELUNE_NAME: &str = "Selune";

/// The state of an on-demand remote registry fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchState {
    Idle,
    Fetching,
    Failed(String),
}

impl FetchState {
    /// The failure message, when failed. (Rendered by the grouped list.)
    #[allow(dead_code)]
    pub fn error(&self) -> Option<&str> {
        match self {
            FetchState::Failed(error) => Some(error),
            _ => None,
        }
    }
}

/// The static description of one registry: its identity and what the
/// `[registries]` config allows it to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistrySeed {
    /// The registry id (`selune` for the built-in one).
    pub id: String,

    /// The display name for group headers.
    pub name: String,

    /// A disabled registry never fetches; its local snapshot still shows.
    pub disabled: bool,

    /// The registry starts online, fetching its remote source.
    pub remote_first: bool,

    /// Whether the registry has an online source at all.
    pub remote_configured: bool,
}

impl RegistrySeed {
    /// The built-in registry; its hosted online source is always there.
    pub fn selune(entry: RegistryEntry) -> Self {
        Self {
            id: SELUNE_REGISTRY.to_string(),
            name: SELUNE_NAME.to_string(),
            disabled: entry.disabled,
            remote_first: entry.remote_first,
            remote_configured: true,
        }
    }

    /// A custom registry; only a `url` counts as an online source.
    pub fn custom(registry: &CustomRegistry) -> Self {
        Self {
            id: registry.name.clone(),
            name: registry.name.clone(),
            disabled: registry.disabled,
            remote_first: registry.remote_first,
            remote_configured: registry.url.is_some(),
        }
    }
}

/// One registry's popup state: which source it shows, its fetch progress, and
/// the providers for the active source. Seeded from the core catalog when the
/// popup opens, then kept current by registry events — the popup never
/// re-reads the catalog mid-session.
#[derive(Clone, Debug)]
pub struct RegistryState {
    pub seed: RegistrySeed,

    /// The registry shows its online snapshot (once loaded) instead of its
    /// offline one.
    pub online: bool,

    pub state: FetchState,

    /// The providers for the active source: the online snapshot once loaded
    /// (empty until a fetch succeeds), else the offline one.
    providers: Vec<Provider>,
}

impl RegistryState {
    /// Seeds from a `[registries]` entry; the registry starts online when
    /// `remote-first` is set, marking a pending fetch when no snapshot is
    /// loaded yet.
    fn new(seed: RegistrySeed) -> Self {
        let online = !seed.disabled && seed.remote_first && seed.remote_configured;
        let providers = if online {
            registry_catalog(&seed.id).remote.unwrap_or_default()
        } else {
            registry_catalog(&seed.id).local
        };
        let state = if online && providers.is_empty() {
            FetchState::Fetching
        } else {
            FetchState::Idle
        };
        Self {
            seed,
            online,
            state,
            providers,
        }
    }

    /// The registry id (`selune` for the built-in one). (Group headers.)
    #[allow(dead_code)]
    pub fn id(&self) -> &str {
        &self.seed.id
    }

    /// The display name for group headers.
    #[allow(dead_code)]
    pub fn name(&self) -> &str {
        &self.seed.name
    }

    /// A disabled registry never fetches; its local snapshot still shows.
    #[allow(dead_code)]
    pub fn disabled(&self) -> bool {
        self.seed.disabled
    }

    /// The source kind label for the active snapshot.
    pub fn source_label(&self) -> &'static str {
        if self.online { "online" } else { "offline" }
    }

    /// The providers for the active source.
    pub fn providers(&self) -> &[Provider] {
        &self.providers
    }

    /// Whether this registry may go online at all.
    fn can_go_online(&self) -> bool {
        !self.seed.disabled && self.seed.remote_configured
    }

    /// Switches to the online source: the loaded remote snapshot when the
    /// catalog has one, otherwise an empty list until a fetch succeeds.
    fn go_online(&mut self) {
        self.online = true;
        let remote = registry_catalog(&self.seed.id).remote;
        self.providers = remote.clone().unwrap_or_default();
        self.state = if remote.is_some() {
            FetchState::Idle
        } else {
            FetchState::Fetching
        };
    }

    /// A fetch succeeded: the online snapshot replaces the list. Ignored for
    /// an offline registry (it never fetches).
    fn on_loaded(&mut self, providers: Vec<Provider>) -> bool {
        if !self.online {
            return false;
        }
        self.state = FetchState::Idle;
        self.providers = providers;
        true
    }

    /// A fetch failed; the list keeps whatever it showed.
    fn on_error(&mut self, error: String) {
        self.state = FetchState::Failed(error);
    }
}

/// The registries a popup reads from: one state per configured registry,
/// selune first, in config order. Replaces the single-source view — every
/// registry has the same offline/online duality (a custom registry's `path`
/// file plays the embedded catalog's role), and Ctrl+O initiates the online
/// registries instead of toggling one source.
pub struct RegistryManager {
    registries: Vec<RegistryState>,
}

impl RegistryManager {
    /// Seeds from the `[registries]` config section: the built-in registry
    /// first, then the custom ones in config order.
    pub fn from_config(config: &RegistriesConfig) -> Self {
        let mut seeds = vec![RegistrySeed::selune(config.selune())];
        seeds.extend(config.custom.iter().map(RegistrySeed::custom));
        Self::new(seeds)
    }

    /// Seeds from explicit descriptors.
    pub fn new(seeds: impl IntoIterator<Item = RegistrySeed>) -> Self {
        Self {
            registries: seeds.into_iter().map(RegistryState::new).collect(),
        }
    }

    /// The per-registry states, in list order. (The grouped list iterates
    /// these for its header rows.)
    #[allow(dead_code)]
    pub fn registries(&self) -> &[RegistryState] {
        &self.registries
    }

    /// One registry's state, when tracked.
    pub fn state(&self, id: &str) -> Option<&RegistryState> {
        self.registries
            .iter()
            .find(|registry| registry.seed.id == id)
    }

    /// The union catalog the popup lists: per registry in order, the online
    /// snapshot once loaded (empty until a fetch succeeds), else the offline
    /// one.
    pub fn snapshot(&self) -> Vec<Provider> {
        self.registries
            .iter()
            .flat_map(|registry| registry.providers().to_vec())
            .collect()
    }

    /// The ids currently fetching — one `Command::FetchRegistry` per id is due.
    pub fn ids_needing_fetch(&self) -> Vec<String> {
        self.registries
            .iter()
            .filter(|registry| registry.state == FetchState::Fetching)
            .map(|registry| registry.seed.id.clone())
            .collect()
    }

    /// Whether Ctrl+O has any effect: some registry can go online.
    pub fn can_toggle(&self) -> bool {
        self.registries.iter().any(RegistryState::can_go_online)
    }

    /// Whether any tracked registry is fetching.
    pub fn any_fetching(&self) -> bool {
        self.registries
            .iter()
            .any(|registry| registry.state == FetchState::Fetching)
    }

    /// Brings every remote-capable registry online and retries failed
    /// fetches; already-loaded snapshots stay put. Returns the ids that need
    /// a fetch.
    pub fn initiate_online(&mut self) -> Vec<String> {
        for registry in &mut self.registries {
            if registry.can_go_online() {
                registry.go_online();
            }
        }
        self.ids_needing_fetch()
    }

    /// A fetch succeeded for the registry with the given id; `true` when a
    /// tracked online registry resolved and the popup should re-read the
    /// union snapshot. Untracked ids (other registries' fetches) are ignored.
    pub fn on_loaded(&mut self, id: &str, providers: Vec<Provider>) -> bool {
        match self.registries.iter_mut().find(|r| r.seed.id == id) {
            Some(registry) => registry.on_loaded(providers),
            None => false,
        }
    }

    /// A fetch failed for the registry with the given id; untracked ids are
    /// ignored.
    pub fn on_error(&mut self, id: &str, error: String) {
        if let Some(registry) = self.registries.iter_mut().find(|r| r.seed.id == id) {
            registry.on_error(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(id: &str, remote_first: bool) -> RegistrySeed {
        RegistrySeed {
            id: id.to_string(),
            name: id.to_string(),
            disabled: false,
            remote_first,
            remote_configured: true,
        }
    }

    fn path_only_seed(id: &str) -> RegistrySeed {
        RegistrySeed {
            remote_configured: false,
            ..seed(id, false)
        }
    }

    fn disabled_seed(id: &str) -> RegistrySeed {
        RegistrySeed {
            disabled: true,
            ..seed(id, true)
        }
    }

    #[test]
    fn offline_by_default() {
        let manager = RegistryManager::new([seed("selune", false)]);
        let registry = manager.state("selune").unwrap();
        assert!(!registry.online);
        assert_eq!(registry.source_label(), "offline");
        assert_eq!(registry.state, FetchState::Idle);
        assert!(manager.ids_needing_fetch().is_empty());
    }

    #[test]
    fn remote_first_starts_online_and_needs_fetch() {
        let manager = RegistryManager::new([seed("selune", true)]);
        let registry = manager.state("selune").unwrap();
        assert!(registry.online);
        assert_eq!(registry.state, FetchState::Fetching);
        assert_eq!(manager.ids_needing_fetch(), vec!["selune".to_string()]);
    }

    #[test]
    fn from_config_seeds_selune_then_customs_in_order() {
        let config = RegistriesConfig {
            custom: vec![CustomRegistry {
                name: "acme".to_string(),
                url: Some("https://example.com/providers.json".to_string()),
                ..CustomRegistry::default()
            }],
            ..RegistriesConfig::default()
        };
        let manager = RegistryManager::from_config(&config);
        let ids: Vec<&str> = manager.registries().iter().map(|r| r.id()).collect();
        assert_eq!(ids, vec!["selune", "acme"]);
        assert_eq!(manager.state("selune").unwrap().name(), "Selune");
        assert_eq!(manager.state("acme").unwrap().name(), "acme");
    }

    #[test]
    fn initiate_online_bring_every_capable_registry_online() {
        let mut manager = RegistryManager::new([
            seed("selune", false),
            seed("acme", false),
            path_only_seed("local-only"),
            disabled_seed("off"),
        ]);
        assert_eq!(
            manager.initiate_online(),
            vec!["selune".to_string(), "acme".to_string()],
            "path-only and disabled registries never fetch"
        );
        assert!(manager.state("selune").unwrap().online);
        assert!(manager.state("acme").unwrap().online);
        assert!(!manager.state("local-only").unwrap().online);
        assert!(!manager.state("off").unwrap().online);
    }

    #[test]
    fn initiate_online_is_idempotent_when_loaded() {
        let mut manager = RegistryManager::new([seed("selune", true)]);
        assert_eq!(manager.initiate_online(), vec!["selune".to_string()]);
        // The loaded event resolves the fetch…
        assert!(manager.on_loaded("selune", vec![]));
        assert_eq!(manager.state("selune").unwrap().state, FetchState::Idle);
        // …but without a catalog snapshot the next initiation retries.
        assert_eq!(manager.initiate_online(), vec!["selune".to_string()]);
    }

    #[test]
    fn disabled_registry_is_a_no_op() {
        let mut manager = RegistryManager::new([disabled_seed("selune")]);
        assert!(!manager.can_toggle());
        assert!(manager.initiate_online().is_empty());
        let registry = manager.state("selune").unwrap();
        assert!(!registry.online);
        assert_eq!(registry.state, FetchState::Idle);
    }

    #[test]
    fn path_only_registry_has_no_online_source() {
        let mut manager = RegistryManager::new([path_only_seed("local-only")]);
        assert!(!manager.can_toggle());
        assert!(manager.initiate_online().is_empty());
        assert!(manager.state("local-only").unwrap().providers().is_empty());
    }

    #[test]
    fn loaded_and_error_events_route_by_id() {
        let mut manager = RegistryManager::new([seed("selune", false), seed("acme", false)]);
        manager.initiate_online();

        assert!(
            !manager.on_loaded("local-only", vec![]),
            "untracked registries resolve nothing"
        );
        manager.on_error("local-only", "boom".to_string());

        manager.on_error("acme", "boom".to_string());
        assert_eq!(
            manager.state("acme").unwrap().state,
            FetchState::Failed("boom".to_string())
        );
        assert_eq!(
            manager.state("selune").unwrap().state,
            FetchState::Fetching,
            "another registry's failure does not mark this one"
        );

        assert!(manager.on_loaded("acme", vec![]));
        assert_eq!(manager.state("acme").unwrap().state, FetchState::Idle);
    }

    #[test]
    fn loaded_event_is_ignored_while_offline() {
        let mut manager = RegistryManager::new([seed("selune", false)]);
        assert!(!manager.on_loaded("selune", vec![]));
        assert_eq!(manager.state("selune").unwrap().state, FetchState::Idle);
    }

    #[test]
    fn snapshot_unions_per_registry_snapshots_in_order() {
        // The built-in registry contributes its embedded offline snapshot; an
        // unknown custom registry contributes nothing (its `path` never
        // loaded, its `url` never fetched).
        let manager = RegistryManager::new([seed("selune", false), seed("acme", false)]);
        assert_eq!(manager.snapshot(), registry_catalog("selune").local);
        assert!(manager.state("acme").unwrap().providers().is_empty());
    }
}
