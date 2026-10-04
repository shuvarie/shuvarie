//! Filesystem watch on the workspace's git `HEAD` file: the branch label
//! shown by the sidebar (and the collapsed footer) refreshes when some
//! other process checks out a branch, instead of re-reading at turn start.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
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
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // The subscription covers the containing directory, so most git
        // activity (lockfiles, the index, gc …) streams through; only
        // events that actually touch `HEAD` wake the TUI. The app re-reads
        // the file anyway, so repeats collapse cheaply.
        let touched = res.ok().is_some_and(|ev| {
            ev.paths
                .iter()
                .any(|p| p.file_name() == Some(OsStr::new("HEAD")))
        });
        if touched {
            let _ = tx.send(());
        }
    })
    .ok()?;
    // Watch the directory, not the file, because a checkout replaces `HEAD`
    // atomically (lockfile + rename): a watch pinned to the replaced file's
    // inode silently dies together with it. A directory watch survives the
    // swap and sees the renamed file land.
    let dir = head.parent()?;
    watcher.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(watcher)
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

    use super::{BranchWatch, head_file};
    use crate::tui::workspace::testing::seed_git_repo;

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
