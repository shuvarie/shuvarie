//! Test-only helpers shared by unit tests across the crate.

use std::sync::{Mutex, MutexGuard};

use shuvarie_llm::ToolContext;

static CWD_LOCK: Mutex<()> = Mutex::new(());

/// Wrapper around the cwd lock's `MutexGuard`, held for a whole test body.
///
/// The newtype keeps `clippy::await_holding_lock` quiet: the lock is
/// intentionally held across `await` points to serialize tests that touch
/// the process cwd, and `#[tokio::test]` runs on a single-threaded
/// runtime, so a std guard cannot stall other tasks on the executor.
pub(crate) struct CwdGuard(#[allow(dead_code)] MutexGuard<'static, ()>);

pub(crate) fn lock_cwd() -> CwdGuard {
    CwdGuard(CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner()))
}

/// A scratch working directory with the cwd lock held: tests that touch
/// the process cwd (tools resolve paths against it) must serialize.
pub(crate) fn tempdir() -> (tempfile::TempDir, CwdGuard) {
    let guard = lock_cwd();
    let dir = tempfile::TempDir::new().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    (dir, guard)
}

pub(crate) fn new_ctx() -> ToolContext {
    ToolContext::new()
}
