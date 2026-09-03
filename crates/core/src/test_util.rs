#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{Mutex, MutexGuard};

    use shuvarie_llm::ToolContext;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_cwd() -> MutexGuard<'static, ()> {
        CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// A scratch working directory with the cwd lock held: tests that touch
    /// the process cwd (tools resolve paths against it) must serialize.
    pub(crate) fn tempdir() -> (tempfile::TempDir, MutexGuard<'static, ()>) {
        let guard = lock_cwd();
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    pub(crate) fn new_ctx() -> ToolContext {
        ToolContext::new()
    }
}
