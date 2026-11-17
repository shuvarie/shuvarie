//! Whether there is an interpreter to run scripts with, and which one.
//!
//! Codemode is offered only when Python can actually run: the alternative is a
//! tool whose every call fails with the same import error. The interpreter is
//! embedded (`pyo3`), so "found" is not a `PATH` lookup -- it is a successful
//! `Py_InitializeEx` plus a version old enough to be worth supporting.
//!
//! Initialization happens once per process and cannot be undone, so the answer
//! is cached: `codemode { python "..." }` has to be in effect before the first
//! probe, and changing it later needs a restart. [`initialize`] is the only way
//! in, which is what lets the host call it early from a place it controls
//! (`crates/core`), rather than from whichever worker thread runs a script.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pyo3::Python;
use pyo3::types::PyAnyMethods;

/// The oldest interpreter the scripts are written for: `X | None` annotations in
/// the card's signatures and positional-only arguments in the runtime are 3.10
/// ideas, and 3.10 is old enough that no distribution worth supporting lacks it.
pub const MIN_VERSION: (u8, u8) = (3, 10);

/// A usable interpreter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interpreter {
    /// Its version, as `platform.python_version` would write it.
    pub version: String,
    /// The installation prefix its standard library came from.
    pub prefix: String,
}

impl Interpreter {
    /// One line for a settings screen: `Python 3.14.10 (/usr)`.
    pub fn describe(&self) -> String {
        match self.prefix.is_empty() {
            true => format!("Python {}", self.version),
            false => format!("Python {} ({})", self.version, self.prefix),
        }
    }
}

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Scripts can run.
    Available(Interpreter),
    /// They cannot, and this is the sentence to show the user.
    Unavailable(String),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available(_))
    }

    /// The interpreter, when there is one.
    pub fn interpreter(&self) -> Option<&Interpreter> {
        match self {
            Availability::Available(interpreter) => Some(interpreter),
            Availability::Unavailable(_) => None,
        }
    }

    /// Why codemode is off, when it is.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Availability::Available(_) => None,
            Availability::Unavailable(reason) => Some(reason),
        }
    }
}

/// The interpreter, started once and remembered.
///
/// `home` is `codemode { python "..." }`: the installation to run scripts with,
/// given either as its prefix or as the `python3` binary inside it. It is applied
/// by pointing `PYTHONHOME` at the prefix *before* initialization, which is the
/// only hook an embedded interpreter offers. The first call wins; later ones with
/// a different `home` get the interpreter that is already running, because there
/// is no way back.
pub fn initialize(home: Option<&Path>) -> &'static Availability {
    static PROBE: OnceLock<Availability> = OnceLock::new();
    PROBE.get_or_init(|| {
        if let Some(home) = home.and_then(home_dir) {
            apply_home(&home);
        }
        probe()
    })
}

/// The cached answer, for a caller that only wants to report it.
pub fn availability() -> &'static Availability {
    initialize(None)
}

/// Point the embedded interpreter at an installation.
///
/// `PYTHONHOME` is the only hook an embedded interpreter offers for choosing
/// where its standard library lives, and it has to be set before
/// initialization. `std::env::set_var` is unsafe because the environment is
/// process-wide, so this runs from [`initialize`]'s one-shot probe -- before the
/// interpreter exists, hence before any script (or any other Python-touching
/// thread) can. The host is expected to call [`initialize`] from its own startup
/// path rather than from a worker.
fn apply_home(home: &Path) {
    let mut vars = vec![("PYTHONHOME".to_string(), home.to_string_lossy().to_string())];
    // A configured prefix also usually carries third-party packages the user
    // expects the script to import (`yaml`, `tomllib` backports, ...).
    let site = home.join("lib").join("python");
    if site.is_dir() {
        vars.push(("PYTHONPATH".to_string(), site.to_string_lossy().to_string()));
    }
    unsafe {
        // SAFETY: one-shot, before the interpreter is started (see above).
        for (key, value) in vars {
            std::env::set_var(key, value);
        }
    }
}

/// `<prefix>/bin/python3` and `<prefix>` both name `<prefix>`.
fn home_dir(path: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let is_bin = |path: &Path| path.file_name().is_some_and(|name| name == "bin");
    // The name has to decide whether a path is a binary, because the configured
    // one need not exist yet (an installation that is about to be created, a
    // prefix inside a container image).
    // A path is a binary when it is one, or when it sits where a binary sits and is
    // named like one -- which is what lets a not-yet-created installation be
    // configured by the path it will have.
    let is_binary = || {
        path.is_file()
            || (path.parent().is_some_and(is_bin)
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("python")))
    };
    if !is_binary() && !path.is_file() {
        return Some(match is_bin(&path) {
            true => path.parent()?.to_path_buf(),
            false => path,
        });
    }
    let parent = path.parent()?;
    Some(match is_bin(parent) {
        true => parent.parent()?.to_path_buf(),
        false => parent.to_path_buf(),
    })
}

/// Start the interpreter and ask it what it is.
fn probe() -> Availability {
    Python::initialize();
    Python::attach(|py| {
        let version = py.version_info();
        let found = (version.major, version.minor);
        if found < MIN_VERSION {
            return Availability::Unavailable(format!(
                "Python {}.{} is too old: codemode scripts need {}.{} or newer",
                version.major, version.minor, MIN_VERSION.0, MIN_VERSION.1
            ));
        }
        let prefix = py
            .import("sys")
            .and_then(|sys| sys.getattr("base_prefix"))
            .and_then(|prefix| prefix.extract::<String>())
            .unwrap_or_default();
        Availability::Available(Interpreter {
            version: Python::version_str().to_string(),
            prefix,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_and_the_binary_inside_it_name_the_same_home() {
        // Neither path has to exist for the shape to be read.
        assert_eq!(
            home_dir(Path::new("/opt/python/bin/python3")),
            Some(PathBuf::from("/opt/python"))
        );
        assert_eq!(
            home_dir(Path::new("/opt/python")),
            Some(PathBuf::from("/opt/python"))
        );
        assert_eq!(
            home_dir(Path::new("/opt/python/bin")),
            Some(PathBuf::from("/opt/python"))
        );
    }

    #[test]
    fn the_probe_answers_exactly_once_and_consistently() {
        let first = initialize(None);
        assert_eq!(first, availability());
        // The first call wins: a later one cannot re-point a running interpreter.
        assert_eq!(initialize(None), first);
    }

    #[test]
    fn an_interpreter_describes_itself_in_one_line() {
        let described = Interpreter {
            version: "3.14.10".to_string(),
            prefix: "/usr".to_string(),
        };
        assert_eq!(described.describe(), "Python 3.14.10 (/usr)");
        let bare = Interpreter {
            version: "3.14.10".to_string(),
            prefix: String::new(),
        };
        assert_eq!(bare.describe(), "Python 3.14.10");
    }

    /// `python "<path>"` is spelled as a prefix *or* as the binary inside it, and
    /// the two have to resolve to the same installation -- which, with an embedded
    /// interpreter, is the directory `PYTHONHOME` is set to. Read off this
    /// machine's own installation so the test is about the rule rather than about a
    /// path that happens to exist.
    #[test]
    fn a_configured_installation_resolves_to_its_prefix() {
        let Some(interpreter) = availability().interpreter() else {
            // No interpreter here; the path rule is all that can be checked.
            assert_eq!(home_dir(Path::new("/np")), Some(PathBuf::from("/np")));
            return;
        };
        let prefix = Path::new(&interpreter.prefix);
        let binary = prefix.join("bin").join("python3");
        if !binary.exists() {
            // A prefix without a `bin/python3` is unusual but not a rule to test.
            return;
        }
        assert_eq!(home_dir(&binary), Some(prefix.to_path_buf()), "{binary:?}");
        assert_eq!(home_dir(prefix), Some(prefix.to_path_buf()), "{prefix:?}");
    }
}
