//! Attachment ingest and request shaping.
//!
//! Two concerns live here:
//!
//! - **Ingest** ([`resolve_directives`]): a turn's attachment paths (the
//!   composer's `@path` items) become [`Prepared`] attachments — bytes
//!   sniffed by magic, images bounded to provider limits (dimension cap +
//!   re-encode), documents converted to GFM markdown by [`shuvarie_doc`].
//!   Runs on the blocking pool; all-or-nothing per turn.
//! - **Request shaping** ([`prepare_for_send`]): the prompt's and history's
//!   attachments are fitted to the streaming target — a capability failure
//!   on *new* images aborts the turn, old images on a text-only model
//!   degrade to noted text, and the image payload is budgeted newest-first
//!   so long sessions trim their oldest images rather than overflowing the
//!   provider's request size.
//!
//! Attachment paths are user intent (typed in the composer), not model
//! action, so they read any path like a user pasting content would; the
//! model never originates an attachment, so it stays outside the read
//! tools' sandbox by design.

use sha2::Digest;
use shuvarie_doc::SourceFormat;
use shuvarie_llm::{Attachment, AttachmentKind, ChatMsg};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// Post-processing image byte cap per attachment. Base64 inflates this by
/// 4/3, so ~3.5 MiB raw stays under Anthropic's 5 MB per-part limit; the
/// other multimodal providers accept this comfortably.
const MAX_IMAGE_BYTES: u64 = 3500 * 1024;
/// The long-edge dimension cap (Anthropic's recommended maximum; the other
/// providers tolerate it). Larger images are decoded and downscaled.
const MAX_IMAGE_SIDE: u32 = 1568;
/// Hard decode guard: images wider/taller than this are rejected before
/// decoding rather than consuming multi-gigabyte pixel buffers.
const MAX_DECODE_SIDE: u32 = 24_000;
/// How many images one user message may carry in a single request.
const MAX_PROMPT_IMAGES: usize = 8;
/// Total attachment bytes (downscaled images + converted text) one user
/// message may carry.
const MAX_PROMPT_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;
/// Total image bytes across the whole request (prompt + history): the
/// newest images win, the oldest are trimmed to notes. Roughly the strictest
/// provider payload cap (Anthropic 32 MiB, Gemini 20 MiB) with headroom for
/// the text around them.
const MAX_REQUEST_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// The raw file size cap before any parsing: attachment files bigger than
/// this are rejected outright.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

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

/// Resolve a turn's `@path` attachments on the blocking pool. All-or-nothing:
/// any failure reports a combined per-file error listing and aborts the turn
/// before anything persists.
pub async fn resolve_directives(
    workspace_root: &Path,
    paths: Vec<String>,
) -> Result<Vec<Prepared>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let workspace_root = workspace_root.to_path_buf();
    tokio::task::spawn_blocking(move || resolve_sync(&workspace_root, paths))
        .await
        .expect("attachment resolver cannot join-fail")
}

fn resolve_sync(workspace_root: &Path, paths: Vec<String>) -> Result<Vec<Prepared>, String> {
    let mut failures: Vec<String> = Vec::new();
    let mut prepared: Vec<Prepared> = Vec::new();
    for path in paths {
        match prepare_one(workspace_root, &path) {
            Ok(item) => prepared.push(item),
            Err(reason) => failures.push(format!("cannot attach {path}: {reason}")),
        }
    }
    if let Err(reason) = check_prompt_budget(&prepared) {
        failures.push(reason);
    }
    if failures.is_empty() {
        Ok(prepared)
    } else {
        Err(failures.join("\n"))
    }
}

/// Per-message limits across the prepared attachments.
fn check_prompt_budget(prepared: &[Prepared]) -> Result<(), String> {
    let images = prepared
        .iter()
        .filter(|item| item.meta.kind == AttachmentKind::Image)
        .count();
    if images > MAX_PROMPT_IMAGES {
        return Err(format!(
            "too many attachments: a message carries at most {MAX_PROMPT_IMAGES} images (got \
             {images})"
        ));
    }
    let total: u64 = prepared
        .iter()
        .map(|item| item.meta.size)
        .fold(0u64, u64::saturating_add);
    if total > MAX_PROMPT_ATTACHMENT_BYTES {
        return Err(format!(
            "attachments too large: a message carries at most {} of attachment bytes (got {})",
            shuvarie_llm::format_size(MAX_PROMPT_ATTACHMENT_BYTES),
            shuvarie_llm::format_size(total)
        ));
    }
    Ok(())
}

/// Ingest one attachment path: read, sniff by magic, bound, and convert.
fn prepare_one(workspace_root: &Path, path: &str) -> Result<Prepared, String> {
    let path = normalize_path(workspace_root, path);
    let (bytes, ext) = read_file(&path)?;
    if let Some(media_type) = image_media_type(&bytes) {
        let (bytes, media_type) = fit_image(&bytes, media_type)?;
        return Ok(finish(AttachmentKind::Image, &path, bytes, media_type));
    }
    match shuvarie_doc::detect(&bytes, ext.as_deref()) {
        Some(format) => {
            let markdown = shuvarie_doc::to_markdown(format, &bytes)
                .map_err(|error| format!("cannot convert document: {error}"))?;
            Ok(finish(
                AttachmentKind::Document,
                &path,
                markdown.into(),
                document_media_type(format),
            ))
        }
        // Not a document sniffable from content or extension: plain UTF-8
        // text files are attached verbatim under `text/plain` — anything
        // else is a binary blob the attachments pipeline has no use for.
        None => match String::from_utf8(bytes) {
            Ok(text) => Ok(finish(
                AttachmentKind::Document,
                &path,
                text.into(),
                "text/plain",
            )),
            Err(_) => Err(
                "unsupported attachment type: not a known document format, image, or readable \
                 text file"
                    .to_string(),
            ),
        },
    }
}

/// Read the file, checking that it is an in-bounds regular file.
fn read_file(path: &Path) -> Result<(Vec<u8>, Option<String>), String> {
    let metadata = std::fs::metadata(path).map_err(|error| format!("cannot read: {error}"))?;
    if metadata.is_dir() {
        return Err("cannot read: is a directory".to_string());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file is {} — attachments are capped at {}",
            shuvarie_llm::format_size(metadata.len()),
            shuvarie_llm::format_size(MAX_FILE_BYTES)
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("cannot read: {error}"))?;
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_string);
    Ok((bytes, ext))
}

/// A relative directive resolves against the workspace root; an absolute one
/// is user intent (`@~/screenshot.png`). A leading `@` is the composer's
/// mention sigil and may slip through.
fn normalize_path(workspace_root: &Path, path: &str) -> PathBuf {
    let path = path.trim().strip_prefix('@').unwrap_or(path.trim());
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        workspace_root.join(path)
    }
}

fn finish(kind: AttachmentKind, path: &Path, bytes: Vec<u8>, media_type: &'static str) -> Prepared {
    Prepared {
        meta: Attachment {
            kind,
            name: file_name(path),
            media_type: media_type.to_string(),
            size: bytes.len() as u64,
            sha256: sha256_hex(&bytes),
        },
        bytes: Some(bytes),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| "attachment".to_string())
}

/// The media type a detected document format reports, for the attachment's
/// metadata (the persisted bytes are the converted markdown).
fn document_media_type(format: SourceFormat) -> &'static str {
    match format {
        SourceFormat::Csv => "text/csv",
        SourceFormat::Docx => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        SourceFormat::Epub => "application/epub+zip",
        SourceFormat::Odp => "application/vnd.oasis.opendocument.presentation",
        SourceFormat::Ods => "application/vnd.oasis.opendocument.spreadsheet",
        SourceFormat::Odt => "application/vnd.oasis.opendocument.text",
        SourceFormat::Pdf => "application/pdf",
        SourceFormat::Pptx => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        SourceFormat::Rtf => "application/rtf",
        SourceFormat::Xls | SourceFormat::Xlsb | SourceFormat::Xlsm => "application/vnd.ms-excel",
        SourceFormat::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        SourceFormat::LegacyDoc => "application/msword",
        SourceFormat::LegacyPpt => "application/vnd.ms-powerpoint",
    }
}

/// Sniff the four raster types every multimodal provider accepts by their
/// magic bytes. SVG and camera-exotic formats (heic, …) deliberately do not
/// map — no provider pipeline here renders them.
fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Fit an image to the provider limits. An already-fitting image passes
/// through untouched (the persisted sha256 then matches the file on disk);
/// oversized or over-dimensioned images are decoded, downscaled, and
/// re-encoded — JPEG for a JPEG source, otherwise PNG when it keeps the cap
/// (text crispness) and JPEG at progressively lower quality otherwise —
/// returning the final bytes plus their media type (both may change: a webp
/// source may re-encode to png).
fn fit_image(bytes: &[u8], media_type: &'static str) -> Result<(Vec<u8>, &'static str), String> {
    let decoded = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("cannot read image: {error}"))?
        .decode()
        .map_err(decode_error)?;
    let oversized = decoded.width().max(decoded.height()) > MAX_IMAGE_SIDE;
    let over_bytes = bytes.len() as u64 > MAX_IMAGE_BYTES;
    if !oversized && !over_bytes {
        return Ok((bytes.to_vec(), media_type));
    }
    let rendered = if oversized {
        decoded.resize(
            MAX_IMAGE_SIDE,
            MAX_IMAGE_SIDE,
            image::imageops::FilterType::CatmullRom,
        )
    } else {
        decoded
    };

    if media_type == "image/jpeg" {
        for quality in [88, 75, 55] {
            let encoded = encode_jpeg(&rendered, quality)?;
            if encoded.len() as u64 <= MAX_IMAGE_BYTES {
                return Ok((encoded, "image/jpeg"));
            }
        }
        return Err(too_large());
    }
    if let Ok(png) = encode_png(&rendered)
        && png.len() as u64 <= MAX_IMAGE_BYTES
    {
        return Ok((png, "image/png"));
    }
    for quality in [88, 75, 55] {
        let encoded = encode_jpeg(&rendered, quality)?;
        if encoded.len() as u64 <= MAX_IMAGE_BYTES {
            return Ok((encoded, "image/jpeg"));
        }
    }
    Err(too_large())
}

fn decode_error(error: image::ImageError) -> String {
    if matches!(error, image::ImageError::Limits(_)) {
        format!(
            "image is too large to decode (long edge caps at {MAX_DECODE_SIDE}); downscale it \
             first"
        )
    } else {
        format!("cannot decode image: {error}")
    }
}

fn encode_png(image: &image::DynamicImage) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|error| format!("cannot re-encode image: {error}"))?;
    Ok(out.into_inner())
}

fn encode_jpeg(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    image
        .write_with_encoder(encoder)
        .map_err(|error| format!("cannot re-encode image: {error}"))?;
    Ok(out.into_inner())
}

fn too_large() -> String {
    format!(
        "image still larger than {} after downscaling; resize or convert it first",
        shuvarie_llm::format_size(MAX_IMAGE_BYTES)
    )
}

/// Whether the streaming target accepts image parts: the catalog's
/// `attachment` flag wins when it knows the model; without a catalog entry
/// the transport decides (rig's OpenAI-family, Anthropic, Gemini, and Ollama
/// clients render image parts; Copilot, Cohere, and Voyage do not).
pub fn supports_images(
    provider_type: selune::ProviderType,
    catalog_model: Option<&selune::Model>,
) -> bool {
    use selune::ProviderType::*;
    if let Some(model) = catalog_model {
        return model.attachment;
    }
    matches!(
        provider_type,
        Openai
            | OpenaiCompat
            | Openrouter
            | Vercel
            | Anthropic
            | Google
            | Azure
            | Bedrock
            | GoogleVertex
            | Ollama
            | Chatgpt
            | Llamafile
    )
}

/// The request-side note a removed attachment leaves behind.
fn drop_note(attachment: &Attachment, reason: &str) -> String {
    format!("[{} {:?} — {reason}]", attachment.kind, attachment.name)
}

/// Fit a whole request (history + prompt attachments) to the streaming
/// target, mutating in place:
///
/// 1. `Err` when the prompt carries images the model cannot receive — the
///    caller aborts the turn (history images instead degrade per 2).
/// 2. Removes history images for a non-image target, leaving a content note
///    behind so the model still sees that the user showed it something.
/// 3. Budgets image bytes across the request newest-first (the prompt is
///    newest and never trimmed), removing history images beyond
///    [`MAX_REQUEST_IMAGE_BYTES`].
///
/// Returns the accommodations it made so the caller can surface them to the
/// user (they are otherwise silent in the request).
pub fn prepare_for_send(
    history: &mut [ChatMsg],
    prompt_attachments: &[Attachment],
    model: &str,
    supports_images: bool,
) -> Result<FitReport, String> {
    let mut report = FitReport::default();
    let prompt_images: Vec<&Attachment> = prompt_attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image)
        .collect();
    let prompt_bytes: u64 = prompt_images
        .iter()
        .map(|a| a.size)
        .fold(0u64, u64::saturating_add);
    if !supports_images {
        if !prompt_images.is_empty() {
            return Err(format!(
                "model {model:?} does not support image attachments — pick a multimodal model \
                 (or remove the images)"
            ));
        }
        for msg in history.iter_mut() {
            if msg.attachments.is_empty() {
                continue;
            }
            report.degraded_history += strip_images(msg, "current model has no image support");
        }
        return Ok(report);
    }
    // Newest-first budget walk: images beyond the cap drop from the oldest
    // messages, prompt images never trim.
    let mut remaining = MAX_REQUEST_IMAGE_BYTES.saturating_sub(prompt_bytes);
    for msg in history.iter_mut().rev() {
        let has_image = msg
            .attachments
            .iter()
            .any(|a| a.kind == AttachmentKind::Image);
        if !has_image {
            continue;
        }
        let mut notes: Vec<String> = Vec::new();
        msg.attachments.retain(|a| {
            if a.kind != AttachmentKind::Image {
                return true;
            }
            if a.size <= remaining {
                remaining -= a.size;
                true
            } else {
                notes.push(drop_note(a, "trimmed to fit the image budget"));
                false
            }
        });
        if !notes.is_empty() {
            report.trimmed_history += notes.len();
            let mut content = std::mem::take(&mut msg.content);
            for note in notes {
                if !content.is_empty() {
                    content.push('\n');
                }
                content.push_str(&note);
            }
            msg.content = content;
        }
    }
    Ok(report)
}

/// Remove a message's images, leaving one note per image behind so the
/// model still knows what it loses access to. Returns the removed count.
fn strip_images(msg: &mut ChatMsg, reason: &str) -> usize {
    let names: Vec<String> = msg
        .attachments
        .iter()
        .filter(|a| a.kind == AttachmentKind::Image)
        .map(|a| drop_note(a, reason))
        .collect();
    let removed = names.len();
    if removed == 0 {
        return 0;
    }
    msg.attachments.retain(|a| a.kind != AttachmentKind::Image);
    let mut content = std::mem::take(&mut msg.content);
    for note in names {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&note);
    }
    msg.content = content;
    removed
}

/// A stored history attachment as a re-sendable prepared attachment:
/// metadata kept, bytes looked up from the blob store when they survive
/// (missing content degrades to a note in the request).
pub async fn resolve_stored_prepared(
    store: &mut shuvarie_db::Store,
    metas: Vec<Attachment>,
) -> Vec<Prepared> {
    let mut prepared = Vec::with_capacity(metas.len());
    for meta in metas {
        let bytes = store.attachment_blob(&meta.sha256).await.ok().flatten();
        prepared.push(Prepared { meta, bytes });
    }
    prepared
}

/// Preload every attachment's persisted content for one request: the
/// history's and the prompt's. Content-addressed blobs load per sha256; a
/// missing or failed blob simply leaves the key out (the renderer degrades
/// that attachment to a note).
pub async fn collect_blobs(
    store: &mut shuvarie_db::Store,
    history: &[ChatMsg],
    prompt_attachments: &[Attachment],
) -> shuvarie_llm::Blobs {
    let mut shas: Vec<String> = history
        .iter()
        .flat_map(|msg| msg.attachments.iter().map(|a| a.sha256.clone()))
        .chain(prompt_attachments.iter().map(|a| a.sha256.clone()))
        .collect();
    shas.sort();
    shas.dedup();
    let mut blobs = shuvarie_llm::Blobs::new();
    for sha in shas {
        if let Ok(Some(content)) = store.attachment_blob(&sha).await {
            blobs.insert(sha, content);
        }
    }
    blobs
}

/// What [`prepare_for_send`] had to accommodate in the request, for the
/// TUI's status row (the accommodations are silent in the request itself).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FitReport {
    /// History images removed to fit [`MAX_REQUEST_IMAGE_BYTES`].
    pub trimmed_history: usize,
    /// History images degraded to notes on a non-image target.
    pub degraded_history: usize,
}

impl FitReport {
    /// Whether anything moved (the notice only shows for real changes).
    pub fn any(&self) -> bool {
        self.trimmed_history > 0 || self.degraded_history > 0
    }

    /// The status-row text for the report, `None` when nothing changed.
    pub fn notice_text(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.trimmed_history > 0 {
            parts.push(format!(
                "{} history image{} trimmed to fit the image budget",
                self.trimmed_history,
                if self.trimmed_history == 1 { "" } else { "s" }
            ));
        }
        if self.degraded_history > 0 {
            parts.push(format!(
                "{} history image{} degraded to notes (current model has no image support)",
                self.degraded_history,
                if self.degraded_history == 1 { "" } else { "s" }
            ));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

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
        PathBuf::from(dir_raw)
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
    use std::io::Cursor as _Cursor;

    /// Re-encode one image to bytes through the given format.
    fn encode(img: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = _Cursor::new(Vec::new());
        img.write_to(&mut out, format).expect("encode");
        out.into_inner()
    }

    fn tiny_png() -> Vec<u8> {
        encode(
            &image::DynamicImage::new_rgb8(2, 2),
            image::ImageFormat::Png,
        )
    }

    fn square_png(side: u32) -> Vec<u8> {
        encode(
            &image::DynamicImage::new_rgb8(side, side),
            image::ImageFormat::Png,
        )
    }

    fn prep_sync(dir: &std::path::Path, paths: &[&str]) -> Result<Vec<Prepared>, String> {
        resolve_sync(dir, paths.iter().map(|p| p.to_string()).collect())
    }

    fn image_meta(name: &str, size: u64) -> Attachment {
        Attachment {
            kind: AttachmentKind::Image,
            name: name.to_string(),
            media_type: "image/png".to_string(),
            size,
            sha256: name.to_string(),
        }
    }

    fn stub(kind: AttachmentKind, size: u64) -> Prepared {
        Prepared {
            meta: Attachment {
                kind,
                name: "x".to_string(),
                media_type: "image/png".to_string(),
                size,
                sha256: "stub".to_string(),
            },
            bytes: None,
        }
    }

    #[test]
    fn a_fitting_image_passes_through_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let raw = tiny_png();
        std::fs::write(dir.path().join("shot.png"), &raw).unwrap();
        let prepared = prep_sync(dir.path(), &["shot.png"]).unwrap();
        assert_eq!(prepared.len(), 1);
        let meta = &prepared[0].meta;
        assert_eq!(meta.kind, AttachmentKind::Image);
        assert_eq!(meta.name, "shot.png");
        assert_eq!(meta.media_type, "image/png");
        assert_eq!(
            prepared[0].bytes.as_deref(),
            Some(raw.as_slice()),
            "an already-fitting image keeps its exact bytes"
        );
        assert_eq!(meta.sha256, sha256_hex(&raw));
    }

    #[test]
    fn over_dimensioned_image_downscales_to_the_long_edge_cap() {
        let dir = tempfile::tempdir().unwrap();
        let raw = square_png(2000);
        std::fs::write(dir.path().join("big.png"), &raw).unwrap();
        let prepared = prep_sync(dir.path(), &["big.png"]).unwrap();
        let meta = &prepared[0].meta;
        assert_eq!(meta.media_type, "image/png");
        let out = decode_render(prepared[0].bytes.as_ref().unwrap());
        assert_eq!(out.width().max(out.height()), MAX_IMAGE_SIDE);
        assert_eq!(
            meta.sha256,
            sha256_hex(prepared[0].bytes.as_ref().unwrap()),
            "the sha addresses the persisted content"
        );
    }

    fn decode_render(bytes: &[u8]) -> image::DynamicImage {
        image::load_from_memory(bytes).expect("decode the processed image")
    }

    #[test]
    fn text_and_document_files_ingest_as_document_attachments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("table.csv"), "a,b\n1,2\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "plain text\n").unwrap();
        let prepared = prep_sync(dir.path(), &["table.csv", "notes.txt"]).unwrap();
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0].meta.kind, AttachmentKind::Document);
        assert_eq!(prepared[0].meta.media_type, "text/csv");
        let md = String::from_utf8(prepared[0].bytes.clone().unwrap()).unwrap();
        assert!(md.contains("1 | 2"), "converted table: {md}");
        assert_eq!(prepared[1].meta.media_type, "text/plain");
        assert_eq!(
            prepared[1].bytes.as_deref(),
            Some("plain text\n".as_bytes())
        );
    }

    #[test]
    fn failures_join_per_file_and_abort_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("blob.bin"), [0x00u8, 0xff, 0xfe, 0x02]).unwrap();
        let error = prep_sync(dir.path(), &["blob.bin", "missing.png"]).unwrap_err();
        assert!(error.contains("blob.bin"), "{error}");
        assert!(error.contains("missing.png"), "{error}");
        assert!(error.contains("unsupported attachment type"), "{error}");
    }

    #[test]
    fn directories_and_oversized_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("inside")).unwrap();
        let error = prep_sync(dir.path(), &["inside"]).unwrap_err();
        assert!(error.contains("is a directory"), "{error}");
        let huge = tempfile::tempdir().unwrap();
        let file = huge.path().join("huge.png");
        let mut out = std::fs::File::create(&file).unwrap();
        out.write_all(b"\x89PNG\r\n\x1a\n").unwrap();
        out.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(out);
        let error = prep_sync(huge.path(), &["huge.png"]).unwrap_err();
        assert!(error.contains("attachments are capped"), "{error}");
    }

    use std::io::Write as _Write;

    #[test]
    fn sigil_relative_and_absolute_paths_resolve() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "text").unwrap();
        let prepared = prep_sync(dir.path(), &["note.txt", "@note.txt", "./note.txt"]).unwrap();
        assert_eq!(prepared.len(), 3);
        assert!(
            prepared
                .iter()
                .all(|p| p.meta.sha256 == prepared[0].meta.sha256)
        );
    }

    #[test]
    fn per_message_limits_reject_many_and_heavy_prompts() {
        let many: Vec<Prepared> = (0..9).map(|_| stub(AttachmentKind::Image, 1)).collect();
        let error = check_prompt_budget(&many).unwrap_err();
        assert!(error.contains("at most 8 images"), "{error}");
        let heavy = vec![stub(AttachmentKind::Image, 17 * 1024 * 1024)];
        let error = check_prompt_budget(&heavy).unwrap_err();
        assert!(error.contains("attachments too large"), "{error}");
        assert!(check_prompt_budget(&[stub(AttachmentKind::Image, 1)]).is_ok());
    }

    #[test]
    fn the_catalog_attachment_flag_decides_transport_defaults_last() {
        use selune::ProviderType;
        let mut flag_true = plain_model("m");
        flag_true.attachment = true;
        assert!(supports_images(ProviderType::Copilot, Some(&flag_true)));
        assert!(!supports_images(
            ProviderType::Openai,
            Some(&plain_model("m"))
        ));
        assert!(supports_images(ProviderType::Openai, None));
        assert!(supports_images(ProviderType::Anthropic, None));
        assert!(supports_images(ProviderType::Ollama, None));
        assert!(!supports_images(ProviderType::Copilot, None));
        assert!(!supports_images(ProviderType::Cohere, None));
        assert!(!supports_images(ProviderType::Voyageai, None));
    }

    fn plain_model(id: &str) -> selune::Model {
        let (org, model) = id.split_once('/').unwrap_or(("test-org", id));
        selune::Model {
            id: id.to_string(),
            model_code: selune::ModelCode::new(org, model, None).expect("model code"),
            name: id.to_string(),
            reasoning: false,
            reasoning_options: Vec::new(),
            attachment: false,
            limit: selune::ModelLimit::default(),
            cost: selune::ModelCost::default(),
            options: None,
        }
    }

    fn user_msg(content: &str, attachments: Vec<Attachment>) -> ChatMsg {
        let mut msg = ChatMsg::user(content);
        msg.attachments = attachments;
        msg
    }

    #[test]
    fn prompt_images_on_a_text_only_model_abort_the_turn() {
        let mut history: Vec<ChatMsg> = Vec::new();
        let prompt = [image_meta("shot.png", 12)];
        let error = prepare_for_send(&mut history, &prompt, "old-text-model", false).unwrap_err();
        assert!(
            error.contains("does not support image attachments"),
            "{error}"
        );
    }

    #[test]
    fn history_images_on_a_text_model_degrade_to_notes() {
        let mut history = vec![user_msg("look", vec![image_meta("shot.png", 12)])];
        prepare_for_send(&mut history, &[], "old-text-model", false).unwrap();
        assert!(history[0].attachments.is_empty(), "images stripped");
        assert!(
            history[0]
                .content
                .contains("[image \"shot.png\" \u{2014} current model has no image support]"),
            "{}",
            history[0].content
        );
    }

    #[test]
    fn the_request_image_budget_trims_oldest_first_and_never_the_prompt() {
        let meg = 1024 * 1024;
        let mut history = vec![
            user_msg("a", vec![image_meta("old.png", 8 * meg as u64)]),
            user_msg("b", vec![image_meta("mid.png", 8 * meg as u64)]),
            user_msg("c", vec![image_meta("new.png", 8 * meg as u64)]),
        ];
        let prompt = [image_meta("prompt.png", 4 * meg as u64)];
        prepare_for_send(&mut history, &prompt, "vision", true).unwrap();
        assert_eq!(history[2].attachments.len(), 1, "newest kept");
        assert!(history[1].attachments.is_empty(), "over-budget dropped");
        assert!(history[0].attachments.is_empty(), "over-budget dropped");
        assert!(
            history[1]
                .content
                .contains("[image \"mid.png\" \u{2014} trimmed to fit the image budget]"),
            "{}",
            history[1].content
        );
        assert_eq!(prompt[0].size, 4 * meg as u64, "prompt images never trim");
    }

    #[test]
    fn documents_ride_regardless_of_image_capability() {
        let document = Attachment {
            kind: AttachmentKind::Document,
            name: "spec.pdf".into(),
            media_type: "application/pdf".into(),
            size: 700,
            sha256: "cd".into(),
        };
        let mut history = Vec::new();
        prepare_for_send(
            &mut history,
            std::slice::from_ref(&document),
            "old-text-model",
            false,
        )
        .expect("a converted document is text and needs no image support");
    }

    #[tokio::test]
    async fn collect_blobs_round_trips_the_persisted_content() {
        let mut store = shuvarie_db::Store::open_in_memory().await.unwrap();
        let id = store.create_session("t", None, None, None).await.unwrap();
        let message = store
            .append_message(id, None, shuvarie_llm::Role::User, "p")
            .await
            .unwrap();
        let content = b"png bytes".to_vec();
        let meta = Attachment {
            sha256: sha256_hex(&content),
            ..image_meta("shot.png", content.len() as u64)
        };
        store
            .attach_message_content(message.id, id, &[(meta.clone(), Some(content.clone()))])
            .await
            .unwrap();
        let blobs = collect_blobs(&mut store, &[], &[meta]).await;
        let expected_key = sha256_hex(b"png bytes");
        assert_eq!(
            blobs.get(&expected_key).map(Vec::as_slice),
            Some(b"png bytes".as_slice()),
            "the persisted content loads by sha"
        );
        // A sha with no blob simply stays out of the map.
        let blobs = collect_blobs(
            &mut store,
            &[],
            &[
                image_meta("lost.png", 1), /* sha256 = "lost.png" — not a store key */
            ],
        )
        .await;
        assert!(!blobs.contains_key("lost.png"));
    }

    // ---- compose-strip probes

    #[test]
    fn probe_reports_exists_size_and_extension_kind() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("shot.png"), tiny_png()).unwrap();
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

    #[test]
    fn fit_report_notice_covers_both_accommodations() {
        let mut report = FitReport::default();
        assert!(report.notice_text().is_none());
        report.trimmed_history = 1;
        assert_eq!(
            report.notice_text().as_deref(),
            Some("1 history image trimmed to fit the image budget")
        );
        report.degraded_history = 2;
        assert_eq!(
            report.notice_text().as_deref(),
            Some(
                "1 history image trimmed to fit the image budget, 2 history images degraded to \
                 notes (current model has no image support)"
            )
        );
    }
}
