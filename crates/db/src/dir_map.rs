//! The data dir's directory-session map (`session-dir.kdl`): which sessions
//! were created in which workspace directory while the shared global store is
//! active. The file is the authority for what counts as an orphan — a stored
//! main session absent from it is purged on the next launch.
//!
//! ```kdl
//! maps {
//!   dir "/path/to/a/project" {
//!     session "<session_id>"
//!   }
//! }
//! ```
//!
//! The map is process-shared ([`SessionDirMap`] is cheap to clone; all
//! clones see the same state) and every write is a locked re-read-modify-write
//! of the file, so concurrent shuvarie instances only lose each other's
//! updates within a tiny read→rename window.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kdl::{KdlDocument, KdlNode};

use crate::error::{DbError, Result};

/// The directory-session map's file name inside the data dir.
pub const SESSION_DIR_MAP_FILE: &str = "session-dir.kdl";

type Entries = BTreeMap<String, Vec<uuid::Uuid>>;

/// The `session-dir.kdl` claims, shared by every clone of a store. Keys are
/// workspace directory paths (canonicalized when possible), values are the
/// session ids claimed for them in claim order.
#[derive(Clone)]
pub struct SessionDirMap {
    path: PathBuf,
    entries: Arc<Mutex<Entries>>,
}

impl SessionDirMap {
    /// Reads the map at `path`; a missing file yields an empty map.
    pub fn load(path: &Path) -> Result<Self> {
        let entries = match std::fs::read_to_string(path) {
            Ok(contents) => parse_map(&contents, path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Entries::new(),
            Err(e) => {
                return Err(DbError::Map(format!("read {}: {e}", path.display())));
            }
        };
        Ok(Self {
            path: path.to_path_buf(),
            entries: Arc::new(Mutex::new(entries)),
        })
    }

    /// The map file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sessions claimed by `dir` (the key is canonicalized the same way
    /// every write canonicalizes it).
    pub fn sessions_for(&self, dir: &Path) -> Vec<uuid::Uuid> {
        let key = canonical_dir(dir);
        self.entries
            .lock()
            .expect("session-dir map lock")
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    /// Every claimed session id across all directories.
    pub fn claimed_ids(&self) -> HashSet<uuid::Uuid> {
        self.entries
            .lock()
            .expect("session-dir map lock")
            .values()
            .flatten()
            .copied()
            .collect()
    }

    /// Records `session` as created in `dir` (idempotent), merging with any
    /// claims written since this map was loaded.
    pub fn claim(&self, dir: &Path, session: uuid::Uuid) -> Result<()> {
        let key = canonical_dir(dir);
        let mut guard = self.locked_entries();
        let list = guard.entry(key).or_default();
        if !list.contains(&session) {
            list.push(session);
        }
        write_entries(&self.path, &guard)
    }

    /// Drops every claim of `session` (idempotent), merging with any claims
    /// written since this map was loaded. The file is only rewritten when a
    /// claim actually went away.
    pub fn unclaim(&self, session: uuid::Uuid) -> Result<()> {
        let mut guard = self.locked_entries();
        let mut changed = false;
        for list in guard.values_mut() {
            let before = list.len();
            list.retain(|id| *id != session);
            changed |= list.len() != before;
        }
        guard.retain(|_, list| !list.is_empty());
        if !changed {
            return Ok(());
        }
        write_entries(&self.path, &guard)
    }

    /// Prunes every claim whose session id is not among `existing` (the
    /// reverse-hygiene half of the orphan purge). The file is only rewritten
    /// when a claim actually went away.
    pub fn prune_missing(&self, existing: &HashSet<uuid::Uuid>) -> Result<()> {
        let mut guard = self.locked_entries();
        let mut changed = false;
        for list in guard.values_mut() {
            let before = list.len();
            list.retain(|id| existing.contains(id));
            changed |= list.len() != before;
        }
        guard.retain(|_, list| !list.is_empty());
        if !changed {
            return Ok(());
        }
        write_entries(&self.path, &guard)
    }

    /// The shared cache with the file's current content re-read into it, held
    /// across the whole read-modify-write: same-process callers serialize on
    /// it and every write lands exactly what its call merged in. An unreadable
    /// or malformed file keeps the cached state instead, so the write still
    /// merges the in-memory claims into whatever is on disk.
    fn locked_entries(&self) -> std::sync::MutexGuard<'_, Entries> {
        let mut guard = self.entries.lock().expect("session-dir map lock");
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => {
                if let Ok(entries) = parse_map(&contents, &self.path) {
                    *guard = entries;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                *guard = Entries::new();
            }
            Err(_) => {}
        }
        guard
    }
}

/// Atomically replaces the map file: the new content lands via a temp file +
/// rename so concurrent readers never see a torn file. [`SessionDirMap`]'s
/// mutators hold the shared cache lock across this, so the in-memory state
/// they leave behind is exactly what was written.
fn write_entries(path: &Path, entries: &Entries) -> Result<()> {
    let temp = path.with_extension("kdl.tmp");
    std::fs::write(&temp, to_kdl(entries))
        .map_err(|e| DbError::Map(format!("write {}: {e}", temp.display())))?;
    std::fs::rename(&temp, path)
        .map_err(|e| DbError::Map(format!("rename {}: {e}", path.display())))
}

/// The workspace dir as a map key: canonicalized so the same directory
/// reached through different paths shares one entry; an uncanonicalizable
/// path (e.g. a deleted dir) is kept verbatim.
fn canonical_dir(dir: &Path) -> String {
    match std::fs::canonicalize(dir) {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => dir.to_string_lossy().into_owned(),
    }
}

fn parse_map(contents: &str, path: &Path) -> Result<Entries> {
    let doc = KdlDocument::parse(contents)
        .map_err(|e| DbError::Map(format!("parse {}: {e}", path.display())))?;
    let mut entries = Entries::new();
    for maps in doc.nodes() {
        if maps.name().value() != "maps" {
            return Err(unexpected_node(path, maps, "maps"));
        }
        if !maps.entries().is_empty() {
            return Err(DbError::Map(format!(
                "parse {}: `maps` takes no arguments",
                path.display()
            )));
        }
        for dir_node in maps.children().map(KdlDocument::nodes).unwrap_or(&[]) {
            if dir_node.name().value() != "dir" {
                return Err(unexpected_node(path, dir_node, "dir"));
            }
            let dir = scalar_string(path, dir_node)?;
            let key = canonical_dir(Path::new(&dir));
            if entries.contains_key(&key) {
                return Err(DbError::Map(format!(
                    "parse {}: duplicate `dir` {dir}",
                    path.display()
                )));
            }
            let mut list = Vec::new();
            for session_node in dir_node.children().map(KdlDocument::nodes).unwrap_or(&[]) {
                if session_node.name().value() != "session" {
                    return Err(unexpected_node(path, session_node, "session"));
                }
                let raw = scalar_string(path, session_node)?;
                let Ok(id) = uuid::Uuid::parse_str(&raw) else {
                    return Err(DbError::Map(format!(
                        "parse {}: `session` must be a UUID, found {raw}",
                        path.display()
                    )));
                };
                if list.contains(&id) {
                    return Err(DbError::Map(format!(
                        "parse {}: duplicate `session` {raw}",
                        path.display()
                    )));
                }
                list.push(id);
            }
            entries.insert(key, list);
        }
    }
    Ok(entries)
}

fn unexpected_node(path: &Path, node: &KdlNode, expected: &str) -> DbError {
    DbError::Map(format!(
        "parse {}: unexpected node `{}` (expected `{expected}`)",
        path.display(),
        node.name().value()
    ))
}

fn scalar_string(path: &Path, node: &KdlNode) -> Result<String> {
    let mut positionals = node.entries().iter().filter(|entry| entry.name().is_none());
    let Some(entry) = positionals.next() else {
        return Err(DbError::Map(format!(
            "parse {}: `{}` takes a string argument",
            path.display(),
            node.name().value()
        )));
    };
    if positionals.next().is_some() {
        return Err(DbError::Map(format!(
            "parse {}: `{}` takes a single argument",
            path.display(),
            node.name().value()
        )));
    }
    match entry.value() {
        kdl::KdlValue::String(value) => Ok(value.clone()),
        _ => Err(DbError::Map(format!(
            "parse {}: `{}` must be a string",
            path.display(),
            node.name().value()
        ))),
    }
}

/// Emits the map in the two-level `maps`/`dir`/`session` shape; an empty map
/// serializes to an empty file.
fn to_kdl(entries: &Entries) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::from("maps {\n");
    for (dir, sessions) in entries {
        out.push_str("  dir ");
        out.push_str(&kdl_string(dir));
        out.push_str(" {\n");
        for session in sessions {
            out.push_str("    session ");
            out.push_str(&kdl_string(&session.to_string()));
            out.push('\n');
        }
        out.push_str("  }\n");
    }
    out.push_str("}\n");
    out
}

/// A KDL string literal with the escapes a filesystem path can carry.
fn kdl_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
