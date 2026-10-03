//! Attachment ingest and request shaping.
//!
//! Three concerns live here, one module each:
//!
//! - **Compose** ([`compose`]): the composer's preview surface — a metadata-
//!   only probe of `@path` directives ([`DirectiveProbe`]) and the workspace
//!   completion listing behind the `@` mention popup ([`path_candidates`]).
//!   Deliberately shallow: no reads of file content.
//! - **Ingest** ([`ingest`]): a turn's attachment paths become [`Prepared`]
//!   attachments — bytes sniffed by magic, images bounded to provider limits
//!   (dimension cap + re-encode), documents converted to GFM markdown by
//!   [`shuvarie_doc`], legacy Office formats through the configured external
//!   converter. Runs on the blocking pool; all-or-nothing per turn.
//! - **Request shaping** ([`shape`]): the prompt's and history's attachments
//!   are fitted to the streaming target — a capability failure on *new*
//!   images aborts the turn, old images on a text-only model degrade to noted
//!   text, and the image payload is budgeted newest-first so long sessions
//!   trim their oldest images rather than overflowing the provider's request
//!   size. Persisted content preloads for the request too ([`collect_blobs`]).
//!
//! Attachment paths are user intent (typed in the composer), not model
//! action, so they read any path like a user pasting content would; the
//! model never originates an attachment, so it stays outside the read
//! tools' sandbox by design.

mod compose;
mod ingest;
mod shape;

use sha2::Digest;
use shuvarie_llm::Attachment;
use std::path::{Path, PathBuf};

pub use compose::{DirectiveProbe, PathCandidate, path_candidates, run_directive_probe};
pub use ingest::resolve_directives;
pub use shape::{
    FitReport, collect_blobs, prepare_for_send, resolve_stored_prepared, supports_images,
};

/// The long-edge dimension cap (Anthropic's recommended maximum) an unset
/// config falls back to; larger images are decoded and downscaled.
pub(crate) const DEFAULT_IMAGE_EDGE: u32 = 1568;
/// How many images one user message may carry by default.
pub(crate) const DEFAULT_MAX_IMAGES: usize = 8;
/// Default total image bytes across the whole request (prompt + history):
/// the newest images win, the oldest are trimmed to notes. Roughly the
/// strictest provider payload cap (Anthropic 32 MiB, Gemini 20 MiB) with
/// headroom for the text around them.
pub(crate) const DEFAULT_REQUEST_IMAGE_BYTES: u64 = 16 * 1024 * 1024;

/// The configured attachment limits (`attachments { … }`), resolved for the
/// ingest and request pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentSettings {
    /// How many images one user message may carry.
    pub max_images: usize,
    /// Total image bytes (post-processing) one request may carry across the
    /// prompt and history; the oldest history images trim first.
    pub image_budget: u64,
    /// The images' long-edge pixel cap; larger images are downscaled.
    pub image_edge: u32,
    /// The legacy-format external converter program (`.doc`/`.ppt`), if any.
    pub office_converter: Option<String>,
}

impl Default for AttachmentSettings {
    fn default() -> Self {
        Self {
            max_images: DEFAULT_MAX_IMAGES,
            image_budget: DEFAULT_REQUEST_IMAGE_BYTES,
            image_edge: DEFAULT_IMAGE_EDGE,
            office_converter: None,
        }
    }
}

impl From<&shuvarie_config::AttachmentsConfig> for AttachmentSettings {
    fn from(config: &shuvarie_config::AttachmentsConfig) -> Self {
        Self {
            max_images: config.max_images,
            image_budget: config.image_budget as u64 * 1024 * 1024,
            image_edge: config.image_edge,
            office_converter: config.office_converter.clone(),
        }
    }
}

/// One fully prepared attachment: the metadata that will persist on the
/// message plus the content bytes the LLM request will use. `bytes` is the
/// *persisted content*: for images the (possibly re-encoded) media, for
/// documents the converted markdown. `None` (a metadata-only carrier — a
/// re-sent history attachment whose blob was pruned) attaches the metadata
/// row without touching the blob store, and the request renders it as a
/// note instead of a multimodal part.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub meta: Attachment,
    pub bytes: Option<Vec<u8>>,
}

impl Prepared {
    /// The metadata-only form used when re-sending a stored history
    /// attachment whose blob content could not be loaded.
    pub fn metadata_only(meta: Attachment) -> Self {
        Self { meta, bytes: None }
    }
}

/// A relative directive resolves against the workspace root; an absolute one
/// is user intent (`@~/screenshot.png`). A leading `@` is the composer's
/// mention sigil and may slip through.
pub(super) fn normalize_path(workspace_root: &Path, path: &str) -> PathBuf {
    let path = path.trim().strip_prefix('@').unwrap_or(path.trim());
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

/// The hex sha256 of `bytes` — the content address every layer keys on.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
