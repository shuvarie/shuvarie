//! Ingest: a turn's `@path` attachment paths become [`Prepared`] attachments.
//! Files are read whole, sniffed by magic bytes, bounded to provider limits,
//! and (documents) converted to markdown; the legacy Office formats convert
//! through the configured external converter. Runs on the blocking pool;
//! all-or-nothing per turn — any failure aborts the turn before anything
//! persists.

use shuvarie_doc::SourceFormat;
use shuvarie_llm::{Attachment, AttachmentKind};
use std::io::Cursor;
use std::path::Path;

use super::{AttachmentSettings, Prepared, normalize_path, sha256_hex};

/// The per-message attachment byte cap (documents plus downscaled images
/// together), independent of the per-request image budget.
const MAX_PROMPT_ATTACHMENT_BYTES: u64 = 16 * 1024 * 1024;
/// Post-processing image byte cap per attachment. Base64 inflates this by
/// 4/3, so ~3.5 MiB raw stays under Anthropic's 5 MB per-part limit; the
/// other multimodal providers accept this comfortably.
const MAX_IMAGE_BYTES: u64 = 3500 * 1024;
/// Hard decode guard: images wider/taller than this are rejected before
/// decoding rather than consuming multi-gigabyte pixel buffers.
const MAX_DECODE_SIDE: u32 = 24_000;
/// The raw file size cap before any parsing: attachment files bigger than
/// this are rejected outright.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Resolve a turn's `@path` attachments on the blocking pool. All-or-nothing:
/// any failure reports a combined per-file error listing and aborts the turn
/// before anything persists.
pub async fn resolve_directives(
    workspace_root: &Path,
    paths: Vec<String>,
    settings: AttachmentSettings,
) -> Result<Vec<Prepared>, String> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let workspace_root = workspace_root.to_path_buf();
    tokio::task::spawn_blocking(move || resolve_sync(&workspace_root, paths, settings))
        .await
        .expect("attachment resolver cannot join-fail")
}

fn resolve_sync(
    workspace_root: &Path,
    paths: Vec<String>,
    settings: AttachmentSettings,
) -> Result<Vec<Prepared>, String> {
    let mut failures: Vec<String> = Vec::new();
    let mut prepared: Vec<Prepared> = Vec::new();
    for path in paths {
        match prepare_one(workspace_root, &path, &settings) {
            Ok(item) => prepared.push(item),
            Err(reason) => failures.push(format!("cannot attach {path}: {reason}")),
        }
    }
    if let Err(reason) = check_prompt_budget(&prepared, settings.max_images) {
        failures.push(reason);
    }
    if failures.is_empty() {
        Ok(prepared)
    } else {
        Err(failures.join("\n"))
    }
}

/// Per-message limits across the prepared attachments.
fn check_prompt_budget(prepared: &[Prepared], max_images: usize) -> Result<(), String> {
    let images = prepared
        .iter()
        .filter(|item| item.meta.kind == AttachmentKind::Image)
        .count();
    if images > max_images {
        return Err(format!(
            "too many attachments: a message carries at most {max_images} images (got {images})"
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
fn prepare_one(
    workspace_root: &Path,
    path: &str,
    settings: &AttachmentSettings,
) -> Result<Prepared, String> {
    let path = normalize_path(workspace_root, path);
    let (bytes, ext) = read_file(&path)?;
    if let Some(media_type) = image_media_type(&bytes) {
        let (bytes, media_type) = fit_image(&bytes, media_type, settings.image_edge)?;
        return Ok(finish(AttachmentKind::Image, &path, bytes, media_type));
    }
    match shuvarie_doc::detect(&bytes, ext.as_deref()) {
        Some(format) => {
            let markdown = document_markdown(&bytes, format, settings)?;
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

/// Converts the detected document to markdown. Legacy `.doc`/`.ppt` go
/// through the configured external converter (the converted container
/// re-enters the facade); without one they are rejected with the option
/// hint. Everything else converts directly.
fn document_markdown(
    bytes: &[u8],
    format: SourceFormat,
    settings: &AttachmentSettings,
) -> Result<String, String> {
    if !matches!(format, SourceFormat::LegacyDoc | SourceFormat::LegacyPpt) {
        return shuvarie_doc::to_markdown(format, bytes)
            .map_err(|error| format!("cannot convert document: {error}"));
    }
    let Some(program) = settings.office_converter.as_deref() else {
        let legacy =
            shuvarie_doc::to_markdown(format, bytes).expect_err("the legacy formats always reject");
        return Err(format!(
            "{legacy} — or set `attachments {{ office-converter \"soffice\" }}` and \
             attach the converted file directly"
        ));
    };
    let converted = crate::office_convert::convert_container(program, format, bytes)?;
    let target = if format == SourceFormat::LegacyDoc {
        "docx"
    } else {
        "pptx"
    };
    let format = shuvarie_doc::detect(&converted, Some(target))
        .ok_or_else(|| "the converter produced an unrecognized file".to_string())?;
    shuvarie_doc::to_markdown(format, &converted)
        .map_err(|error| format!("cannot convert document: {error}"))
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
fn fit_image(
    bytes: &[u8],
    media_type: &'static str,
    image_edge: u32,
) -> Result<(Vec<u8>, &'static str), String> {
    let decoded = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| format!("cannot read image: {error}"))?
        .decode()
        .map_err(decode_error)?;
    let oversized = decoded.width().max(decoded.height()) > image_edge;
    let over_bytes = bytes.len() as u64 > MAX_IMAGE_BYTES;
    if !oversized && !over_bytes {
        return Ok((bytes.to_vec(), media_type));
    }
    let rendered = if oversized {
        decoded.resize(
            image_edge,
            image_edge,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-encode one image to bytes through the given format.
    fn encode(img: &image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
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
        resolve_sync(
            dir,
            paths.iter().map(|p| p.to_string()).collect(),
            AttachmentSettings::default(),
        )
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
        assert_eq!(
            out.width().max(out.height()),
            super::super::DEFAULT_IMAGE_EDGE
        );
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
        use std::io::Write;
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
        let error = check_prompt_budget(&many, super::super::DEFAULT_MAX_IMAGES).unwrap_err();
        assert!(error.contains("at most 8 images"), "{error}");
        let heavy = vec![stub(AttachmentKind::Image, 17 * 1024 * 1024)];
        let error = check_prompt_budget(&heavy, super::super::DEFAULT_MAX_IMAGES).unwrap_err();
        assert!(error.contains("attachments too large"), "{error}");
        assert!(
            check_prompt_budget(
                &[stub(AttachmentKind::Image, 1)],
                super::super::DEFAULT_MAX_IMAGES
            )
            .is_ok()
        );

        // The configured count wins: max-images 2 rejects a third image.
        let error = check_prompt_budget(&many, 2).unwrap_err();
        assert!(error.contains("at most 2 images"), "{error}");
    }

    /// A metadata-only attachment, shaped for the budget checks.
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

    #[cfg(unix)]
    fn unix_script(dir: &std::path::Path, body: &str) -> String {
        let script = dir.join("fake-convert.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.to_string_lossy().into_owned()
    }

    #[cfg(unix)]
    #[test]
    fn legacy_doc_converts_through_the_configured_program() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture.docx");
        std::fs::write(&fixture, include_bytes!("../../tests/fixtures/report.docx")).unwrap();
        let fixture = fixture.to_string_lossy().into_owned();
        let program = unix_script(dir.path(), &format!("cp '{fixture}' \"$6/document.docx\""));
        let settings = AttachmentSettings {
            office_converter: Some(program),
            ..AttachmentSettings::default()
        };
        std::fs::write(dir.path().join("old.doc"), b"legacy binary container").unwrap();
        let prepared =
            resolve_sync(dir.path(), vec!["old.doc".to_string()], settings).expect("converts");
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].meta.kind, AttachmentKind::Document);
        assert_eq!(prepared[0].meta.media_type, "application/msword");
        let markdown = String::from_utf8(prepared[0].bytes.clone().unwrap()).unwrap();
        assert!(markdown.contains("Quarterly Report"), "{markdown}");
    }

    #[cfg(unix)]
    #[test]
    fn legacy_without_a_converter_rejects_with_the_option_hint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("old.ppt"), b"legacy binary container").unwrap();
        let error = resolve_sync(
            dir.path(),
            vec!["old.ppt".to_string()],
            AttachmentSettings::default(),
        )
        .expect_err("no converter configured");
        assert!(error.contains("office-converter"), "{error}");
    }
}
