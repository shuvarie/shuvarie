//! Spawning and stopping core tasks: one per workspace, which is one per TUI
//! tab.
//!
//! The process starts with a single core task in the directory it was launched
//! from. Every further tab spawns another through [`CoreSpawner`]: the same
//! `shuvarie_core::run` loop, but with everything the core used to read from
//! the process for itself — the workspace root, the permission rules, the
//! config chain, the store — resolved for that tab's directory instead. The
//! core reaches its root through its `Permissions`, so a second core needs no
//! process-wide state (`crates/core/src/permissions.rs`).
//!
//! Two pieces stay process-global on purpose:
//!
//! - the Selune catalog: `catalog::init` is idempotent per registry id, and the
//!   TUI reads the same catalog for its model pickers. Tabs are expected to
//!   agree on `registries`; a tab whose config disagrees changes lookups in
//!   every tab rather than only its own.
//! - `connections.kdl` and the `ui` prefs: the active provider, model, and theme
//!   are the user's, not a tab's. Each core loads `connections.kdl` itself, so
//!   the file remains the one authority.

use std::path::{Path, PathBuf};

use shuvarie_core::permissions::Permissions;
use shuvarie_core::{
    Command, Config, Connections, Event as CoreEvent, StartupSession, TrustFile, TrustGrants,
};
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::task::JoinHandle;

/// Depth of the command and event channels a core is driven through.
const COMMAND_BUFFER: usize = 64;
const EVENT_BUFFER: usize = 64;

/// One running core task: the channels that drive it and the task itself.
pub struct CoreHandle {
    /// Commands into this core.
    pub cmd_tx: Sender<Command>,
    /// Events out of it.
    pub event_rx: Receiver<CoreEvent>,
    /// The workspace the core resolves paths against. Canonicalized, so the
    /// permission rules (which canonicalize their own paths) and every resolved
    /// tool path agree on the same base.
    root: PathBuf,
    join: JoinHandle<()>,
}

impl CoreHandle {
    /// The workspace this core runs in: what its tab's sidebar shows and what
    /// `/newtab` inherits when it is given no directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Splits the handle into what the render loop drives separately: the
    /// command sender (shared with the App), the event receiver (turned into the
    /// loop's stream), and the task that stops it. [`Self::shutdown`] is the
    /// fused form of the same thing, for the single-core callers.
    pub fn into_parts(self) -> CoreParts {
        let CoreHandle {
            cmd_tx,
            event_rx,
            root,
            join,
        } = self;
        CoreParts {
            cmd_tx,
            event_rx,
            root,
            join,
        }
    }

    /// Stops this core: drops its command sender and waits for the task, which
    /// releases its session lock and shuts its LSP and MCP managers down.
    ///
    /// Every other clone of `cmd_tx` must be gone first — the App holds one —
    /// or the core's select loop never sees its channel close.
    pub async fn shutdown(self) -> Result<(), String> {
        let CoreParts { cmd_tx, join, .. } = self.into_parts();
        drop(cmd_tx);
        join.await.map_err(|e| format!("core task failed: {e}"))
    }
}

/// A core's pieces, separated (see [`CoreHandle::into_parts`]).
pub struct CoreParts {
    pub cmd_tx: Sender<Command>,
    pub event_rx: Receiver<CoreEvent>,
    /// The workspace the core was spawned for.
    pub root: PathBuf,
    pub join: JoinHandle<()>,
}

/// Builds core tasks for workspaces other than the one the process started in.
///
/// Everything a spawn needs that is a property of the *process* rather than of a
/// workspace is captured here at startup; everything that is a property of the
/// workspace is resolved per spawn from the tab's directory.
#[derive(Clone)]
pub struct CoreSpawner {
    /// The `--config` file the process was started with, if any. An explicit
    /// file is a user override, so every tab reads it rather than discovering
    /// the chain of its own directory.
    config_path: Option<PathBuf>,
    /// The grants a workspace with no recorded trust decision starts under. A
    /// tab never prompts — opening one must not yank the terminal into a dialog
    /// from a key press — so an unrecorded workspace inherits the startup
    /// workspace's decision.
    fallback_grants: TrustGrants,
    /// `--transient`: every core runs on its own in-memory database.
    transient: bool,
}

impl CoreSpawner {
    pub fn new(
        config_path: Option<PathBuf>,
        fallback_grants: TrustGrants,
        transient: bool,
    ) -> Self {
        Self {
            config_path,
            fallback_grants,
            transient,
        }
    }

    /// Starts a core task whose workspace is `root`.
    ///
    /// `startup` is the session to resume; a new tab passes
    /// [`StartupSession::None`] and starts on an empty transcript. The error is
    /// the user-facing reason the tab could not start (an unreadable directory,
    /// an unparseable config), which the TUI shows instead of opening the tab.
    pub async fn spawn(&self, root: &Path, startup: StartupSession) -> Result<CoreHandle, String> {
        let root = std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
        if !root.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }

        let grants = self.grants_for(&root)?;
        let config = self.config_for(&root, &grants)?;
        // The scene set is the tab's own: a workspace may drop scenes into its
        // `.shuvarie/scene.d`, and the core runs the turns it is given.
        let scenes = Config::load_scenes(&config, &root, &grants, self.config_path.as_deref())
            .map_err(|e| format!("scenes: {e}"))?;
        // Compiled against the tab's root, which is also where the core reads
        // every relative tool path and starts `run_shell` from.
        let permissions = Permissions::build(&config.permissions, &root)
            .map_err(|e| format!("permissions: {e}"))?;
        let store = if self.transient {
            shuvarie_db::Store::open_in_memory().await
        } else {
            shuvarie_db::Store::for_workspace(&root, config.db.global_store).await
        }
        .map_err(|e| format!("store: {e}"))?;
        let connections = Connections::load().map_err(|e| format!("connections: {e}"))?;

        let (cmd_tx, cmd_rx) = channel::<Command>(COMMAND_BUFFER);
        let (event_tx, event_rx) = channel::<CoreEvent>(EVENT_BUFFER);
        let join = tokio::spawn(shuvarie_core::run(
            config,
            connections,
            store,
            startup,
            self.config_path.clone(),
            None,
            std::sync::Arc::new(permissions),
            grants,
            scenes,
            cmd_rx,
            event_tx,
        ));
        Ok(CoreHandle {
            cmd_tx,
            event_rx,
            root,
            join,
        })
    }

    /// The grants `root` runs under: its recorded trust decision, else the
    /// startup workspace's. Never prompts and never writes.
    fn grants_for(&self, root: &Path) -> Result<TrustGrants, String> {
        let file = TrustFile::load().map_err(|e| format!("trusts: {e}"))?;
        Ok(file
            .lookup_record(root)
            .map(|record| record.grants.clone())
            .unwrap_or_else(|| self.fallback_grants.clone()))
    }

    /// The config chain `root` runs under: the explicit `--config` file when one
    /// was given, else the chain discovered from the tab's own directory.
    fn config_for(&self, root: &Path, grants: &TrustGrants) -> Result<Config, String> {
        match &self.config_path {
            Some(path) => Config::load_explicit(path),
            None => Config::load_trusted(root, grants),
        }
        .map_err(|e| format!("config: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// How long a spawned core gets to report its startup before the test calls
    /// it stuck (the cores announce these immediately; the slack is for a loaded
    /// machine, not for real work).
    const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

    /// Seeds one custom command in `root`. The roster is loaded from
    /// `<root>/.shuvarie/commands`, so which commands a core reports is a direct
    /// probe of the root it resolved.
    fn seed_command(root: &Path, command: &str) {
        let dir = root
            .join(shuvarie_core::WORKSPACE_DIR_NAME)
            .join("commands")
            .join(command);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("command.md"), "Do the thing.\n").unwrap();
    }

    /// A spawner reading an explicit empty config, so a test never picks up the
    /// user's config chain, registries, or skills.
    fn spawner(dir: &Path) -> CoreSpawner {
        let config = dir.join("config.kdl");
        std::fs::write(&config, "").unwrap();
        CoreSpawner::new(Some(config), TrustGrants::all(), false)
    }

    /// The command names the core reports at startup.
    async fn commands_of(core: &mut CoreHandle) -> Vec<String> {
        loop {
            let event = tokio::time::timeout(STARTUP_TIMEOUT, core.event_rx.recv())
                .await
                .expect("the core reports its startup")
                .expect("the core is running");
            if let CoreEvent::CustomCommandsLoaded { commands, .. } = event {
                return commands.into_iter().map(|command| command.name).collect();
            }
        }
    }

    /// Two roots, two cores, one process: each resolves the workspace it was
    /// spawned for, which is what makes a tab a separate agent.
    #[tokio::test]
    async fn each_core_resolves_the_workspace_it_was_spawned_for() {
        let alpha = tempfile::TempDir::new().unwrap();
        let beta = tempfile::TempDir::new().unwrap();
        let config_dir = tempfile::TempDir::new().unwrap();
        seed_command(alpha.path(), "alpha-only");
        seed_command(beta.path(), "beta-only");
        let spawner = spawner(config_dir.path());

        let mut a = spawner
            .spawn(alpha.path(), StartupSession::None)
            .await
            .unwrap();
        let mut b = spawner
            .spawn(beta.path(), StartupSession::None)
            .await
            .unwrap();

        assert_eq!(commands_of(&mut a).await, ["alpha-only"]);
        assert_eq!(commands_of(&mut b).await, ["beta-only"]);

        // The root is canonicalized, so it is the base the permission rules and
        // the resolved tool paths share.
        assert_eq!(a.root(), alpha.path().canonicalize().unwrap());
        assert_eq!(b.root(), beta.path().canonicalize().unwrap());

        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    }

    /// A tab pointed at somewhere that is not a usable directory is rejected with
    /// the reason the user needs, instead of half-opening a session.
    #[tokio::test]
    async fn a_workspace_that_is_not_a_directory_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let spawner = spawner(dir.path());

        let missing = dir.path().join("no-such-project");
        let Err(error) = spawner.spawn(&missing, StartupSession::None).await else {
            panic!("a missing directory must not spawn a core");
        };
        assert!(error.contains("no-such-project"), "{error}");

        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let Err(error) = spawner.spawn(&file, StartupSession::None).await else {
            panic!("a file must not spawn a core");
        };
        assert!(error.contains("is not a directory"), "{error}");
    }

    /// A workspace with no recorded trust decision runs under the startup
    /// workspace's grants: opening a tab never prompts.
    #[tokio::test]
    async fn an_unrecorded_workspace_inherits_the_startup_grants() {
        let dir = tempfile::TempDir::new().unwrap();
        let workspace = tempfile::TempDir::new().unwrap();
        let grants = TrustGrants::none();
        let config = dir.path().join("config.kdl");
        std::fs::write(&config, "").unwrap();
        let spawner = CoreSpawner::new(Some(config), grants.clone(), false);

        assert_eq!(
            spawner.grants_for(workspace.path().canonicalize().unwrap().as_path()),
            Ok(grants)
        );
    }
}
