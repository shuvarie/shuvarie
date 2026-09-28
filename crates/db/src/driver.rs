use std::path::Path;

use toasty_driver_turso::Turso;

/// Instantiate a default Turso driver
pub fn new_default_driver(path: impl AsRef<Path>) -> Turso {
    toasty_driver_turso::Turso::file(path)
        .experimental_index_method(true)
        .experimental_multiprocess_wal(true)
}

/// Instantiate a default in-memory Turso driver
pub fn new_default_in_memory_driver() -> Turso {
    toasty_driver_turso::Turso::in_memory().experimental_index_method(true)
}
