//! Filesystem watch on the workspace's git `HEAD` file: the branch label
//! shown by the sidebar (and the collapsed footer) refreshes when some
//! other process checks out a branch, instead of re-reading at turn start.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher, event::EventKind};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// A live watch on the workspace's git `HEAD` file, handing out unit
/// wakeups that mean "the label may have changed". The receiver only ever
/// resolves when HEAD was touched: the struct owns a channel sender itself,
/// so the channel can never close under the running render loop.
pub struct BranchWatch {
    rx: UnboundedReceiver<()>,
    /// Keeps the filesystem watch subscribed; dropping it unsubscribes.
    _watcher: Option<RecommendedWatcher>,
    /// Keeps the wake channel open even when no watch fits (no repository
    /// at startup, or the watch failed), so `rx` can never drain empty.
    _keep_open: UnboundedSender<()>,
}

impl BranchWatch {
    /// Starts watching the git `HEAD` file behind `workspace`'s repository.
    /// Without a repository the watch stays idle: no wakeups are ever
    /// produced (a repository created after startup is picked up on the
    /// next application start).
    pub fn start(workspace: &Path) -> Self {
        let (tx, rx) = unbounded_channel();
        Self {
            _watcher: head_file(workspace).and_then(|head| watch_head(&head, tx.clone())),
            rx,
            _keep_open: tx,
        }
    }

    /// Waits for the next "HEAD touched" wakeup; never resolves to a
    /// closed channel.
    pub async fn next(&mut self) {
        if self.rx.recv().await.is_none() {
            // Unreachable while `Self` (owning a sender) lives, and a
            // `None` must not read as "quit" either — park forever.
            std::future::pending::<()>().await;
        }
    }
}

/// Subscribes `tx` to changes of the `HEAD` file at `head`. `None` when the
/// platform watcher or the subscription itself fails.
fn watch_head(head: &Path, tx: UnboundedSender<()>) -> Option<RecommendedWatcher> {
    let mut watcher = RecommendedWatcher::new(
        move |res: notify::Result<notify::Event>| {
            // The subscription covers the containing directory, so most git
            // activity (lockfiles, the index, gc …) streams through; only
            // events that actually rewrite `HEAD` wake the TUI. The app re-reads
            // the file anyway, so repeats collapse cheaply.
            if res.ok().is_some_and(|ev| head_rewritten(&ev)) {
                let _ = tx.send(());
            }
        },
        notify::Config::default().with_poll_interval(Duration::from_secs(5)),
    )
    .ok()?;
    // Watch the directory, not the file, because a checkout replaces `HEAD`
    // atomically (lockfile + rename): a watch pinned to the replaced file's
    // inode silently dies together with it. A directory watch survives the
    // swap and sees the renamed file land.
    let dir = head.parent()?;
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
}

/// Whether an event means the git `HEAD` file was *rewritten*.
///
/// Reads must not count. The inotify backend subscribes to `IN_OPEN` and
/// `IN_CLOSE_NOWRITE` on top of the change masks, so every read of `HEAD`
/// raises an event — and the TUI reads `HEAD` (through `gix`, which opens the
/// repository config on the way) whenever a wake says the label may have
/// changed. Waking on those closes a loop: the read wakes the watch, the wake
/// makes the app read again, thousands of times a second, for as long as the
/// TUI runs (measured: both the render loop and the watcher thread pinned a
/// core while the app sat at an idle prompt). Only rewrites wake us: a checkout
/// writes `HEAD` in place (a modify) or replaces it through the lockfile
/// rename (create, rename, remove).
fn head_rewritten(event: &notify::Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
        && event
            .paths
            .iter()
            .any(|p| p.file_name() == Some(OsStr::new("HEAD")))
}

/// The git `HEAD` file whose rewrites change the branch label, resolved
/// through `gix` so linked worktrees (`.git` as a file) resolve to their
/// real git directory. `None` when `workspace` sits outside a repository.
fn head_file(workspace: &Path) -> Option<PathBuf> {
    let repo = gix::discover(workspace).ok()?;
    let head = repo.git_dir().join("HEAD");
    head.exists().then_some(head)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, EventKind, ModifyKind, RemoveKind,
        RenameMode,
    };

    use super::{BranchWatch, head_file, head_rewritten};
    use crate::tui::workspace::testing::seed_git_repo;

    fn event(kind: EventKind, path: &str) -> notify::Event {
        notify::Event::new(kind).add_path(std::path::PathBuf::from(path))
    }

    #[test]
    fn branch_watch_wakes_on_rewrites_of_head() {
        let head = "/repo/.git/HEAD";

        // A checkout rewriting `HEAD` in place, or swapping it in through the
        // lockfile rename, is what the label refresh is for.
        assert!(head_rewritten(&event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            head
        )));
        assert!(head_rewritten(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            head
        )));
        assert!(head_rewritten(&event(
            EventKind::Create(CreateKind::File),
            head
        )));
        assert!(head_rewritten(&event(
            EventKind::Remove(RemoveKind::File),
            head
        )));
    }

    #[test]
    fn branch_watch_ignores_reads_of_head() {
        // The app's own re-read of `HEAD` — the first thing a wake makes it do —
        // raises these. Waking on them re-arms the watch from the read itself,
        // which is the loop that burned ~120% CPU at an idle prompt.
        assert!(!head_rewritten(&event(
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            "/repo/.git/HEAD"
        )));
        assert!(!head_rewritten(&event(
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            "/repo/.git/HEAD"
        )));
        assert!(!head_rewritten(&event(
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
            "/repo/.git/HEAD"
        )));
    }

    #[test]
    fn branch_watch_ignores_git_churn_that_is_not_head() {
        // The watch covers the whole git directory, so the index, the lockfiles
        // and gc traffic all stream through it; none of it moves the branch
        // label.
        assert!(!head_rewritten(&event(
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            "/repo/.git/index"
        )));
        assert!(!head_rewritten(&event(
            EventKind::Create(CreateKind::File),
            "/repo/.git/HEAD.lock"
        )));
        assert!(!head_rewritten(&event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            "/repo/.git/HEAD.lock"
        )));
        // The overflow rescan notify emits carries no paths at all.
        assert!(!head_rewritten(&notify::Event::new(EventKind::Other)));
    }

    #[test]
    fn branch_watch_matches_head_by_name_not_by_path() {
        // The match is on the file name, not on the watched path: the backends
        // that report a subtree (macOS FSEvents) hand over canonicalized paths
        // that no longer equal the one `watch` was given. A ref log named
        // `HEAD` therefore wakes the watch as well — a label refresh, not the
        // read loop, since the app never reads the log.
        assert!(head_rewritten(&event(
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            "/repo/.git/logs/HEAD"
        )));
    }

    #[test]
    fn head_file_resolves_at_the_workspace_git_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        seed_git_repo(dir.path(), "main");

        let head = head_file(dir.path()).expect("a seeded worktree resolves");
        assert_eq!(head.file_name(), Some(std::ffi::OsStr::new("HEAD")));
        assert!(head.starts_with(dir.path()));
        assert!(head.ancestors().any(|a| a.ends_with(".git")));
    }

    #[test]
    fn head_file_is_none_outside_a_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(head_file(dir.path()), None);
    }

    #[tokio::test]
    async fn head_writes_wake_the_watch() {
        let dir = tempfile::tempdir().expect("tempdir");
        seed_git_repo(dir.path(), "one");
        head_file(dir.path()).expect("the seeded worktree resolves");

        let mut watch = BranchWatch::start(dir.path());
        // Let the platform watch register before the write; FSEvents (macOS)
        // starts its stream asynchronously.
        tokio::time::sleep(Duration::from_millis(250)).await;
        std::fs::write(
            head_file(dir.path()).expect("again"),
            "ref: refs/heads/two\n",
        )
        .expect("switch branch");
        // A generous timeout: file events are fast but not instantaneous.
        tokio::time::timeout(Duration::from_secs(10), watch.next())
            .await
            .expect("a HEAD rewrite wakes the watch");
    }
}
