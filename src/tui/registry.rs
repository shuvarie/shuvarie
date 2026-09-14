use selune::Provider;
use shuvarie_core::RegistryEntry;

/// The state of an on-demand remote (hosted) registry fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchState {
    Idle,
    Fetching,
    Failed(String),
}

impl FetchState {
    pub fn error(&self) -> Option<&str> {
        match self {
            FetchState::Failed(error) => Some(error),
            _ => None,
        }
    }
}

/// Where a registry popup reads its catalog from: the offline embedded
/// registry by default, the hosted remote one after Ctrl+O (or when
/// `[registries] selune remote-first` is set). A disabled selune registry
/// offers no remote source at all.
pub struct RegistrySource {
    pub remote: bool,
    pub state: FetchState,
    pub disabled: bool,
}

impl RegistrySource {
    /// Seeds from a `[registries]` entry; the popup starts on the remote
    /// source when `remote-first` is set, marking a pending fetch when no
    /// snapshot is loaded yet.
    pub fn new(entry: RegistryEntry) -> Self {
        let remote = !entry.disabled && entry.remote_first;
        let state = if remote && !shuvarie_core::catalog::remote_registry_loaded() {
            FetchState::Fetching
        } else {
            FetchState::Idle
        };
        Self {
            remote,
            state,
            disabled: entry.disabled,
        }
    }

    /// Whether a `Command::FetchRegistry` should be sent for this source.
    pub fn needs_fetch(&self) -> bool {
        self.state == FetchState::Fetching
    }

    /// Whether Ctrl+O may switch sources.
    pub fn can_toggle(&self) -> bool {
        !self.disabled
    }

    /// Flips between the offline and remote sources. Returns `true` when the
    /// caller must send `Command::FetchRegistry` (switched to the remote
    /// source with no snapshot loaded yet). Pressing Ctrl+O again after a
    /// failed fetch returns offline; toggling back retries.
    pub fn toggle(&mut self) -> bool {
        if self.disabled {
            return false;
        }
        if self.remote {
            self.remote = false;
            self.state = FetchState::Idle;
            return false;
        }
        self.remote = true;
        if shuvarie_core::catalog::remote_registry_loaded() {
            self.state = FetchState::Idle;
            false
        } else {
            self.state = FetchState::Fetching;
            true
        }
    }

    pub fn on_loaded(&mut self) {
        self.state = FetchState::Idle;
    }

    pub fn on_error(&mut self, error: String) {
        self.state = FetchState::Failed(error);
    }

    /// The provider catalog for the active source: the remote snapshot when
    /// wanted (empty until a fetch succeeds), else the embedded one.
    pub fn snapshot(&self) -> Vec<Provider> {
        shuvarie_core::catalog::registry_providers(self.remote)
    }

    /// The human label for the active source.
    pub fn label(&self) -> &'static str {
        if self.remote { "online" } else { "offline" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(disabled: bool, remote_first: bool) -> RegistryEntry {
        RegistryEntry {
            disabled,
            remote_first,
        }
    }

    #[test]
    fn offline_by_default() {
        let source = RegistrySource::new(entry(false, false));
        assert!(!source.remote);
        assert!(source.can_toggle());
        assert!(!source.needs_fetch());
        assert_eq!(source.label(), "offline");
    }

    #[test]
    fn remote_first_needs_fetch_until_loaded() {
        let source = RegistrySource::new(entry(false, true));
        assert!(source.remote);
        assert_eq!(source.state, FetchState::Fetching);
        assert!(source.needs_fetch());
    }

    #[test]
    fn disabled_hides_remote() {
        let mut source = RegistrySource::new(entry(true, true));
        assert!(!source.remote);
        assert!(!source.can_toggle());
        assert!(!source.toggle(), "toggle is a no-op when disabled");
        assert!(!source.remote);
    }

    #[test]
    fn toggle_round_trip_requests_fetch_when_unloaded() {
        let mut source = RegistrySource::new(entry(false, false));
        assert!(source.toggle(), "first switch to remote needs a fetch");
        assert!(source.remote);
        assert_eq!(source.state, FetchState::Fetching);

        source.on_loaded();
        assert!(!source.needs_fetch());

        assert!(!source.toggle(), "switching back offline never fetches");
        assert!(!source.remote);
        assert_eq!(source.state, FetchState::Idle);
    }

    #[test]
    fn failed_fetch_stays_remote_until_toggled_off() {
        let mut source = RegistrySource::new(entry(false, false));
        assert!(source.toggle());
        source.on_error("boom".into());
        assert_eq!(source.state, FetchState::Failed("boom".into()));
        assert!(source.remote, "the failed source stays selected");
        assert!(!source.toggle(), "toggling away clears the failure");
        assert!(!source.remote);
        assert_eq!(source.state, FetchState::Idle);
    }
}
