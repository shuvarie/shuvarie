//! Test-only helpers shared by unit tests across the crate.

use std::sync::{Mutex, MutexGuard};

use shuvarie_llm::ToolContext;

use crate::permissions::{Access, DenyCut, PermissionGate, Permissions};

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

/// Builtin-default permissions compiled against the current cwd. Ask
/// verdicts fail fast: the gate's receiver is dropped, so a paused request
/// resolves to a denial instead of hanging the test.
pub(crate) fn access() -> Access {
    access_for_config(&shuvarie_config::PermissionsConfig::builtin())
}

/// [`Access`] compiled from an explicit permissions config (ask verdicts
/// fail fast as in [`access`]).
pub(crate) fn access_for_config(config: &shuvarie_config::PermissionsConfig) -> Access {
    let permissions = std::sync::Arc::new(
        Permissions::build(
            config,
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        )
        .unwrap(),
    );
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    Access::new(permissions, PermissionGate::new(tx), DenyCut::default())
}

/// [`Access`] whose ask gate is driven by the returned receiver, so tests can
/// answer prompts.
pub(crate) fn access_with_answering_gate(
    config: &shuvarie_config::PermissionsConfig,
) -> (
    Access,
    tokio::sync::mpsc::Receiver<crate::permissions::PermissionRequest>,
) {
    let permissions = std::sync::Arc::new(
        Permissions::build(
            config,
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        )
        .unwrap(),
    );
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    (
        Access::new(permissions, PermissionGate::new(tx), DenyCut::default()),
        rx,
    )
}
