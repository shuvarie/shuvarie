#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{Mutex, MutexGuard};

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_cwd() -> MutexGuard<'static, ()> {
        CWD_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }
}
