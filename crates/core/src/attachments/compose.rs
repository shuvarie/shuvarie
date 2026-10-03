//! Compose-side support for attachments: the pending-attachment strip's
//! preview ([`DirectiveProbe`]) and the `@` mention completion listing
//! ([`PathCandidate`]). Both are deliberately metadata-only filesystem
//! operations — never reads of file content, never conversion; send time
//! does the authoritative preparation through [`super::ingest`].

use shuvarie_llm::AttachmentKind;
use std::path::Path;

use super::normalize_path;

/// The composer strip's preview of one `@path` directive: what it would
/// become when sent, decided from path metadata alone (no reads or
/// conversion — send time does the authoritative preparation). `kind: None`
/// means a neutral chip (an unknown or plain-text file); `error` marks a
/// hard fail the send would also reject.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveProbe {
    pub kind: Option<AttachmentKind>,
    pub size: Option<u64>,
    pub error: Option<String>,
}

/// Preview one attachment directive path: exists/size/kind from filesystem
/// metadata plus an extension-based kind guess. Deliberately shallow.
pub fn run_directive_probe(workspace_root: &Path, path: &str) -> DirectiveProbe {
    let resolved = normalize_path(workspace_root, path);
    let metadata = match std::fs::metadata(&resolved) {
        Ok(metadata) => metadata,
        Err(_) => {
            return DirectiveProbe {
                kind: None,
                size: None,
                error: Some("missing file".to_string()),
            };
        }
    };
    if metadata.is_dir() {
        return DirectiveProbe {
            kind: None,
            size: None,
            error: Some("is a directory".to_string()),
        };
    }
    DirectiveProbe {
        kind: ext_kind(&resolved),
        size: Some(metadata.len()),
        error: None,
    }
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

/// One completion candidate for the composer's `@` mention: a directory
/// entry (`name` carries the trailing `/` for directories).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCandidate {
    pub name: String,
    pub is_dir: bool,
}

/// How many candidates one completion reply carries.
const MAX_COMPLETIONS: usize = 40;

/// List the workspace entries matching a partial `@` mention query —
/// `""`/`"."` list the root, anything else splits at the last `/` into a
/// directory part (kept) and a case-insensitive name prefix. Directories
/// sort first, both groups alphabetically; hidden entries appear only when
/// the prefix starts with a dot.
pub fn path_candidates(workspace_root: &Path, query: &str) -> Vec<PathCandidate> {
    let query = query.trim().strip_prefix('@').unwrap_or(query.trim());
    let (dir_raw, prefix) = match query.strip_suffix('/') {
        // A trailing slash completes INTO the directory: list its contents.
        Some(dir) => (dir, ""),
        None => match query.rsplit_once('/') {
            Some((dir, prefix)) => (dir, prefix),
            None => ("", query),
        },
    };
    let dir = if Path::new(dir_raw).is_absolute() {
        std::path::PathBuf::from(dir_raw)
    } else {
        workspace_root.join(dir_raw)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let prefix = prefix.to_ascii_lowercase();
    let mut out: Vec<PathCandidate> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if is_dir {
                // The candidate completes into a `dir/` fragment: a trailing
                // slash keeps the mention inside the directory.
                name.push('/');
            }
            if prefix.is_empty() && name.starts_with('.') {
                return None;
            }
            if !name.to_ascii_lowercase().starts_with(&prefix) {
                return None;
            }
            Some(PathCandidate { name, is_dir })
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out.truncate(MAX_COMPLETIONS);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_exists_size_and_extension_kind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), b"png bytes").unwrap();
        std::fs::write(dir.path().join("plan.pdf"), b"%PDF-1.4 body").unwrap();
        std::fs::write(dir.path().join("notes"), b"").unwrap();
        let root = dir.path();
        let image = run_directive_probe(root, "shot.png");
        assert_eq!(image.kind, Some(AttachmentKind::Image));
        assert!(image.error.is_none());
        assert!(image.size.unwrap() > 0);
        let doc = run_directive_probe(root, "plan.pdf");
        assert_eq!(doc.kind, Some(AttachmentKind::Document));
        let plain = run_directive_probe(root, "notes");
        assert_eq!(plain.kind, None, "extension-less stays a neutral chip");
    }

    #[test]
    fn probe_reports_missing_and_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("shot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let missing = run_directive_probe(dir.path(), "nope.png");
        assert_eq!(missing.error.as_deref(), Some("missing file"));
        let directory = run_directive_probe(dir.path(), "sub");
        assert_eq!(directory.error.as_deref(), Some("is a directory"));
        // Absolute probes resolve without the workspace root.
        let absolute = run_directive_probe(
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
            let probe = run_directive_probe(dir.path(), &format!("f.{ext}"));
            assert_eq!(probe.kind, Some(AttachmentKind::Image), "ext {ext}");
        }
    }

    #[test]
    fn completions_list_dirs_first_then_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tools")).unwrap();
        std::fs::create_dir(dir.path().join("tests")).unwrap();
        std::fs::write(dir.path().join("setup.rs"), "").unwrap();
        std::fs::write(dir.path().join("main.rs"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        let root = dir.path();
        assert_eq!(
            path_candidates(root, ""),
            vec!["src/", "tests/", "main.rs", "setup.rs"]
                .into_iter()
                .map(|name| PathCandidate {
                    name: name.to_string(),
                    is_dir: name.ends_with('/'),
                })
                .collect::<Vec<_>>(),
            "directories sort first, then files, both alphabetically"
        );
    }

    #[test]
    fn completions_prefix_match_is_case_insensitive_and_hidden_aware() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("code.md"), "").unwrap();
        std::fs::write(dir.path().join(".cargo"), "").unwrap();
        let root = dir.path();
        // Case-insensitive prefix — but the prefix itself must match, so a
        // dotfile only shows when the prefix starts with a dot. Hidden
        // entries are otherwise invisible from any prefix.
        let dot = path_candidates(root, ".car");
        assert_eq!(
            dot.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec![".cargo"],
        );
        let visible = path_candidates(root, "c");
        assert_eq!(
            visible.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Cargo.toml", "code.md"],
            "prefix match is case-insensitive; dotfiles stay hidden"
        );
    }

    #[test]
    fn completions_resolve_directory_parts_under_the_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/tools")).unwrap();
        std::fs::write(dir.path().join("src/tools/tool.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/tools/walk.rs"), "").unwrap();
        std::fs::write(dir.path().join("src/other.rs"), "").unwrap();
        let root = dir.path();
        let found = path_candidates(root, "src/tools/tool");
        assert_eq!(
            found.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["tool.rs"]
        );
        // A trailing slash lists the directory's contents.
        assert_eq!(
            path_candidates(root, "src/tools/")
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tool.rs", "walk.rs"]
        );
        // The intermediate dir completes itself from the outer listing.
        assert_eq!(
            path_candidates(root, "src/to")
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["tools/"]
        );
        // An unreadable dir is an empty list.
        assert!(path_candidates(root, "does-not-exist/").is_empty());
    }
}
