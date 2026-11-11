//! Compose-side support for attachments: the pending-attachment strip's
//! preview ([`AttachmentProbe`]) and the attachment dialog's directory hints
//! ([`directory_candidates`]). Deliberately metadata-only filesystem
//! operations — never reads of file content, never conversion; send time does
//! the authoritative preparation through [`super::ingest`].

use shuvarie_llm::AttachmentKind;
use std::path::Path;

use super::{normalize_path, normalize_path_in};

/// How many directory hints one reply carries.
pub const MAX_DIRECTORY_HINTS: usize = 20;

/// The composer strip's preview of one pending attachment: what it would become
/// when sent, decided from path metadata alone (no reads or conversion — send
/// time does the authoritative preparation). `kind: None` means a neutral chip
/// (an unknown or plain-text file); `error` marks a hard fail the send would
/// also reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentProbe {
    pub kind: Option<AttachmentKind>,
    pub size: Option<u64>,
    pub error: Option<String>,
}

/// A clipboard image staged to a temp file, ready to enter the composer as a
/// pending attachment: the staged path plus the same shallow probe the attach
/// strip uses, computed from the bytes core has already sniffed and fitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardAttachment {
    pub path: String,
    pub probe: AttachmentProbe,
}

/// Whether `path` names an attachment at all: an existing regular file after
/// workspace/tilde resolution (symlinks followed). The paste router's accept
/// step — a pasted *path* that is a directory, is missing, or dangles is not an
/// attachment, so the payload stays ordinary text instead of becoming a chip
/// that immediately reports an error.
pub fn is_attachable_file(workspace_root: &Path, path: &str) -> bool {
    let resolved = normalize_path(workspace_root, path);
    std::fs::metadata(&resolved).is_ok_and(|metadata| metadata.is_file())
}

/// Preview one attachment path: exists/size/kind from filesystem metadata plus
/// an extension-based kind guess. Deliberately shallow.
pub fn probe_attachment(workspace_root: &Path, path: &str) -> AttachmentProbe {
    let resolved = normalize_path(workspace_root, path);
    let metadata = match std::fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(_) => {
            return AttachmentProbe {
                kind: None,
                size: None,
                error: Some("missing file".to_string()),
            };
        }
    };
    if metadata.is_dir() {
        return AttachmentProbe {
            kind: None,
            size: None,
            error: Some("is a directory".to_string()),
        };
    }
    AttachmentProbe {
        kind: ext_kind(&resolved),
        size: Some(metadata.len()),
        error: None,
    }
}

/// Subdirectories matching a partially typed directory path, for the attachment
/// dialog's directory prompt: the text splits at its last `/` into a directory
/// part (kept exactly as typed) and a case-insensitive name prefix, and every
/// matching subdirectory comes back *completed* in the same form the user is
/// typing — `doc` → `docs`, `~/sh` → `~/shots`, `/tmp/sh` → `/tmp/shots`. An
/// empty text lists the workspace's own subdirectories.
///
/// Directories only (the prompt names a search root, never a file), hidden
/// entries only when the prefix asks for them, and a candidate identical to what
/// is already typed is dropped — completing to itself would do nothing.
pub fn directory_candidates(workspace_root: &Path, typed: &str) -> Vec<String> {
    directory_candidates_in(workspace_root, dirs::home_dir().as_deref(), typed)
}

/// [`directory_candidates`] with an explicit home directory, so tests can
/// exercise tilde expansion without mutating the process environment.
pub fn directory_candidates_in(
    workspace_root: &Path,
    home: Option<&Path>,
    typed: &str,
) -> Vec<String> {
    let typed = typed.trim();
    let (dir_raw, prefix) = match typed.strip_suffix('/') {
        // A trailing slash completes INTO the directory: list its contents.
        Some(dir) => (dir, ""),
        None => match typed.rsplit_once('/') {
            // A bare `/x` splits into an empty directory part, which is the
            // filesystem root rather than the workspace.
            Some(("", prefix)) if typed.starts_with('/') => ("/", prefix),
            Some((dir, prefix)) => (dir, prefix),
            None => ("", typed),
        },
    };
    // A leading `~` with nothing after it is the home directory itself.
    let base = normalize_path_in(workspace_root, home, dir_raw);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let prefix = prefix.to_ascii_lowercase();
    let mut out: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if prefix.is_empty() && name.starts_with('.') {
                return None;
            }
            if !name.to_ascii_lowercase().starts_with(&prefix) {
                return None;
            }
            let completed = if dir_raw.is_empty() {
                name
            } else {
                format!("{}/{}", dir_raw.trim_end_matches('/'), name)
            };
            (completed != typed).then_some(completed)
        })
        .collect();
    out.sort_by_key(|candidate| candidate.to_lowercase());
    out.truncate(MAX_DIRECTORY_HINTS);
    out
}

/// Preview-only kind guess by extension (magic bytes are the send-time
/// truth): images are the four accepted raster types, documents anything
/// [`shuvarie_doc`] recognizes by extension.
fn ext_kind(path: &Path) -> Option<AttachmentKind> {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)?;
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return Some(AttachmentKind::Image);
    }
    shuvarie_doc::detect(&[], Some(&ext)).map(|_| AttachmentKind::Document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attachments::normalize_path_in;

    #[test]
    fn probe_reports_exists_size_and_extension_kind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), b"png bytes").unwrap();
        std::fs::write(dir.path().join("plan.pdf"), b"%PDF-1.4 body").unwrap();
        std::fs::write(dir.path().join("notes"), b"").unwrap();
        let root = dir.path();
        let image = probe_attachment(root, "shot.png");
        assert_eq!(image.kind, Some(AttachmentKind::Image));
        assert!(image.error.is_none());
        assert!(image.size.unwrap() > 0);
        let doc = probe_attachment(root, "plan.pdf");
        assert_eq!(doc.kind, Some(AttachmentKind::Document));
        let plain = probe_attachment(root, "notes");
        assert_eq!(plain.kind, None, "extension-less stays a neutral chip");
    }

    #[test]
    fn probe_reports_missing_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let missing = probe_attachment(dir.path(), "nope.png");
        assert_eq!(missing.error.as_deref(), Some("missing file"));
        let directory = probe_attachment(dir.path(), "sub");
        assert_eq!(directory.error.as_deref(), Some("is a directory"));
        // Absolute probes resolve without the workspace root.
        let absolute = probe_attachment(
            dir.path(),
            dir.path().join("shot.png").to_string_lossy().as_ref(),
        );
        assert!(absolute.error.is_none());
    }

    #[test]
    fn probe_sniffs_image_extensions_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["JPG", "Png", "WEBP", "gif"] {
            let path = dir.path().join(format!("f.{ext}"));
            std::fs::write(&path, b"x").unwrap();
            let probe = probe_attachment(dir.path(), &format!("f.{ext}"));
            assert_eq!(probe.kind, Some(AttachmentKind::Image), "ext {ext}");
        }
    }

    #[test]
    fn probe_resolves_relative_paths_and_expands_tilde() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/plan.pdf"), b"%PDF-1.4").unwrap();
        let probe = probe_attachment(dir.path(), "docs/plan.pdf");
        assert!(probe.error.is_none(), "{probe:?}");
        assert_eq!(
            normalize_path_in(dir.path(), Some(dir.path()), "~/docs/plan.pdf"),
            dir.path().join("docs/plan.pdf")
        );
    }

    #[test]
    fn attachable_files_are_regular_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("shot.png"), b"x").unwrap();
        std::fs::create_dir(root.join("shots")).unwrap();
        assert!(is_attachable_file(root, "shot.png"), "a real file");
        assert!(
            is_attachable_file(root, root.join("shot.png").to_string_lossy().as_ref()),
            "an absolute path resolves without the workspace root"
        );
        assert!(
            !is_attachable_file(root, "shots"),
            "a directory is a path, not a file"
        );
        assert!(!is_attachable_file(root, "nope.png"), "a missing path");
    }

    #[cfg(unix)]
    #[test]
    fn attachable_files_follow_symlinks_but_reject_dangling_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("shot.png"), b"x").unwrap();
        std::os::unix::fs::symlink(root.join("shot.png"), root.join("link.png")).unwrap();
        std::os::unix::fs::symlink(root.join("gone.png"), root.join("dangling.png")).unwrap();
        assert!(
            is_attachable_file(root, "link.png"),
            "a symlink to a file resolves"
        );
        assert!(
            !is_attachable_file(root, "dangling.png"),
            "a broken symlink is not a file"
        );
    }

    fn names(candidates: Vec<String>) -> Vec<String> {
        candidates
    }

    #[test]
    fn directory_hints_complete_the_typed_form() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("docs/deep")).unwrap();
        std::fs::create_dir(root.join("docs-old")).unwrap();
        std::fs::create_dir(root.join("src")).unwrap();
        std::fs::write(root.join("docs.md"), "").unwrap();
        assert_eq!(
            names(directory_candidates_in(root, None, "doc")),
            vec!["docs", "docs-old"],
            "directories only, completed in the typed (relative) form"
        );
        // A trailing slash lists the directory's contents, keeping the prefix.
        assert_eq!(
            names(directory_candidates_in(root, None, "docs/")),
            vec!["docs/deep"]
        );
        // Empty text lists the workspace root's own directories.
        assert_eq!(
            names(directory_candidates_in(root, None, "")),
            vec!["docs", "docs-old", "src"]
        );
        // An exact match is not offered as its own completion.
        assert_eq!(
            names(directory_candidates_in(root, None, "src")),
            Vec::<String>::new()
        );
        // An unreadable directory is an empty list rather than an error.
        assert!(directory_candidates_in(root, None, "nope/").is_empty());
    }

    #[test]
    fn directory_hints_expand_tilde_and_absolute_forms() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join("shots/2026")).unwrap();
        std::fs::create_dir(home.join("scratch")).unwrap();
        assert_eq!(
            names(directory_candidates_in(home, Some(home), "~/sh")),
            vec!["~/shots"]
        );
        assert_eq!(
            names(directory_candidates_in(home, Some(home), "~/")),
            vec!["~/scratch", "~/shots"]
        );
        // An absolute form completes absolutely — including a bare `/x`.
        let absolute = home.to_string_lossy().into_owned();
        assert_eq!(
            names(directory_candidates_in(
                home,
                Some(home),
                &format!("{absolute}/sh")
            )),
            vec![format!("{absolute}/shots")]
        );
        assert_eq!(
            names(directory_candidates_in(home, Some(home), "/tm")),
            vec!["/tmp"]
                .into_iter()
                .filter(|candidate| std::path::Path::new(candidate).is_dir())
                .map(str::to_string)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn directory_hints_hide_dot_entries_until_the_prefix_asks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join(".config")).unwrap();
        std::fs::create_dir(root.join("cfg")).unwrap();
        assert_eq!(names(directory_candidates_in(root, None, "")), vec!["cfg"]);
        assert_eq!(
            names(directory_candidates_in(root, None, ".con")),
            vec![".config"]
        );
    }
}
