use serde_json::{json, Value};
use shuvarie_doc::utils::MatchDocumentExt;
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::{
    attachments::AttachmentSettings,
    permissions::{resolve_read, Access, PathKind},
};
use super::{arg_value, ReadCache};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const BINARY_SAMPLE_BYTES: usize = 4096;
const SUPPORTED_EXTENSIONS: &str =
    "docx, pdf, pptx, xls/xlsx/xlsm/xlsb, ods, odt, odp, rtf, epub, csv";

pub(crate) struct ReadFile {
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
    settings: AttachmentSettings,
    accepts_images: bool,
}

impl ReadFile {
    pub(crate) fn new(
        read_cache: ReadCache,
        max_output_chars: usize,
        max_output_bytes: usize,
        access: Access,
        settings: AttachmentSettings,
        accepts_images: bool,
    ) -> Self {
        Self {
            read_cache,
            max_output_chars,
            max_output_bytes,
            access,
            settings,
            accepts_images,
        }
    }
}

/// What path classification decides before any read: which dedupe namespace
/// to use and how the tool presents itself. Classification by extension only
/// picks the cache namespace — the content decides the actual reading mode
/// (a mislabeled `.png` that is really text reads as text).
enum ReadKind {
    /// Image or document extensions: reads via the converted/rich pipeline.
    Rich,
    /// Everything else: the plain text pipeline (with the binary refusal).
    Text,
}

fn read_kind(path: &str) -> ReadKind {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if ext.is_image() || ext.is_document() {
        ReadKind::Rich
    } else {
        ReadKind::Text
    }
}

/// One classified file loaded on the blocking pool, fully converted or with
/// its error already mapped. The async side formats/truncates only.
enum Loaded {
    /// Plain text or code: the raw file text for line-windowed rendering.
    Text(String),
    /// A document converted to markdown (legacy formats through the
    /// configured external converter when set).
    Markdown(String),
    /// An image bounded to the provider limits (possibly downscaled and
    /// re-encoded so its media type may change from the file's).
    Image {
        bytes: Vec<u8>,
        media_type: &'static str,
    },
}

impl Tool for ReadFile {
    const NAME: &'static str = "read_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        let images = if self.accepts_images {
            "images (png, jpeg, gif, webp) are returned to the model as image content"
        } else {
            "the current model does not accept image content, so images are refused"
        };
        let legacy = if self.settings.office_converter.is_some() {
            "Legacy binary .doc/.ppt go through the configured external converter and may take a while."
        } else {
            "Legacy binary .doc/.ppt are not readable; ask the user to convert them first (e.g. `soffice --convert-to docx file.doc`)."
        };
        format!(
            "Read a file from the workspace. Plain text and code return the requested line range. \
             Documents ({SUPPORTED_EXTENSIONS}) return their content converted to GitHub-Flavored \
             Markdown. {images}. {legacy} Scanned or image-only document pages cannot be read (no \
             OCR). Returns an error when the path does not exist, is a directory, or is an \
             unreadable binary."
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the file, relative to the workspace root" },
                "offset": { "type": "integer", "minimum": 1, "description": "First line to read (1-based). Defaults to 1. Ignored for images" },
                "limit": { "type": "integer", "minimum": 1, "description": format!("Maximum number of lines to read. Defaults to {DEFAULT_READ_LIMIT}. Ignored for images") }
            },
            "required": ["path"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let read_cache = self.read_cache.clone();
        let max_output_chars = self.max_output_chars;
        let max_output_bytes = self.max_output_bytes;
        let access = self.access.clone();
        let settings = self.settings.clone();
        let accepts_images = self.accepts_images;
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            match read_kind(&path) {
                // Rich reads dedupe in the converted-read namespace so a
                // document/image read and a later plain text read of the same
                // path never collide, while both still count for write_file's
                // read-first check.
                ReadKind::Rich => {
                    if read_cache.mark_document(&path, offset, limit) {
                        return Ok(ToolOutput::text(format!(
                            "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                        )));
                    }
                }
                ReadKind::Text => {
                    if read_cache.mark(&path, offset, limit) {
                        return Ok(ToolOutput::text(format!(
                            "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                        )));
                    }
                }
            }
            let abs = resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a file"));
            }
            let data = tokio::fs::read(&abs).await.map_err(|e| format!("read {path}: {e}"))?;
            if data.is_empty() && matches!(read_kind(&path), ReadKind::Rich) {
                return Err(format!("'{path}' is empty"));
            }
            // Content-first classification on the blocking pool (document and
            // image conversion are CPU-heavy); the extension only chose the
            // dedupe namespace above.
            let extension = std::path::Path::new(&path)
                .extension()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_string);
            let error_of = |error: String| format!("read {path}: {error}");
            let classify_path = path.clone();
            let loaded = tokio::task::spawn_blocking(move || classify(&data, extension.as_deref(), &settings, &classify_path))
                .await
                .map_err(|error| error_of(format!("conversion failed: {error}")))?
                .map_err(|error| if matches!(error, LoadError::Unrecognized) {
                    format!("'{path}' appears to be binary; refusing to read")
                } else {
                    error_of(error.message())
                })?;
            match loaded {
                Loaded::Image { bytes, media_type } => {
                    if !accepts_images {
                        return Err(format!(
                            "'{path}' is an image, but the current model does not accept image \
                             content; describe the image by name instead or ask the user to view it"
                        ));
                    }
                    let name = std::path::Path::new(&path)
                        .file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or(&path)
                        .to_string();
                    let note = format!(
                        "Returned image {name}: {media_type}, {} (bounded to the model's image limits)",
                        shuvarie_llm::format_size(bytes.len() as u64)
                    );
                    let blocks = shuvarie_llm::tool_content_with_image(
                        note,
                        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
                        media_type,
                    )
                    .map_err(|e| format!("read {path}: {e}"))?;
                    ToolOutput::content(blocks)
                        .map_err(|e| format!("read {path}: {e}"))
                }
                Loaded::Markdown(markdown) => {
                    let out = paginate_markdown(&markdown, offset, limit)?;
                    capped_text(&path, out, max_output_chars, max_output_bytes)
                }
                Loaded::Text(text) => {
                    let out = render_text_lines(&text, offset, limit)?;
                    capped_text(&path, out, max_output_chars, max_output_bytes)
                }
            }
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// Classification errors whose message is already user-facing; the
/// unrecognized-binary case keeps read_file's canonical refusal wording.
enum LoadError {
    Unrecognized,
    Message(String),
}

impl LoadError {
    fn message(&self) -> String {
        match self {
            LoadError::Unrecognized => String::new(),
            LoadError::Message(message) => message.clone(),
        }
    }
}

/// Content-first classification: image sniff wins, then document detection
/// (the legacy formats route through the configured external converter;
/// failures keep the actionable taxonomy), then plain UTF-8 text, then the
/// binary refusal.
fn classify(
    data: &[u8],
    extension: Option<&str>,
    settings: &AttachmentSettings,
    path: &str,
) -> Result<Loaded, LoadError> {
    if let Some(media_type) = crate::attachments::image_media_type(data) {
        let (bytes, media_type) =
            crate::attachments::fit_image(data, media_type, settings.image_edge)
                .map_err(LoadError::Message)?;
        return Ok(Loaded::Image { bytes, media_type });
    }
    match shuvarie_doc::detect(data, extension) {
        Some(format) => {
            let markdown = crate::attachments::document_convert(data, format, settings).map_err(
                |failure| {
                    let message = match failure {
                        crate::attachments::DocumentFailure::Convert(error) => {
                            document_error_message(path, &error)
                        }
                        crate::attachments::DocumentFailure::External(error) => error,
                    };
                    LoadError::Message(message)
                },
            )?;
            Ok(Loaded::Markdown(markdown))
        }
        None => {
            if is_binary_file(data) || String::from_utf8(data.to_vec()).is_err() {
                Err(LoadError::Unrecognized)
            } else {
                Ok(Loaded::Text(String::from_utf8_lossy(data).into_owned()))
            }
        }
    }
}

/// Renders a text file with read_file's line-number gutter, range, and
/// footers.
fn render_text_lines(
    text: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<String, String> {
    let lines: Vec<&str> = text.lines().collect();
    let offset = offset.unwrap_or(1).max(1) as usize;
    if offset > lines.len() && !(offset == 1 && lines.is_empty()) {
        return Err(format!(
            "Offset {offset} is out of range for this file ({} lines)",
            lines.len()
        ));
    }
    let limit = limit.map(|n| n as usize).unwrap_or(DEFAULT_READ_LIMIT);
    let start = offset - 1;
    let end = (start + limit).min(lines.len());
    let mut out = String::new();
    let mut long_lines = false;
    for (i, line) in lines[start..end].iter().enumerate() {
        let line = if line.chars().count() > MAX_LINE_LENGTH {
            long_lines = true;
            let head: String = line.chars().take(MAX_LINE_LENGTH).collect();
            format!("{head}{MAX_LINE_SUFFIX}")
        } else {
            line.to_string()
        };
        out.push_str(&format!("{:>6} | {line}\n", start + i + 1));
    }
    let last = start + (end - start);
    let truncated = limit < lines.len() - start.min(lines.len());
    if truncated {
        out.push_str(&format!(
            "\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
            offset,
            last,
            lines.len(),
            last + 1
        ));
    } else {
        out.push_str(&format!("\n(End of file - total {} lines)", lines.len()));
    }
    if long_lines {
        out.push_str(
            "\n(long lines truncated; use run_shell, e.g. `sed -n 'Np' file | cut -c1-2000`, to read one exactly)",
        );
    }
    Ok(out)
}

/// Renders a converted document's markdown with pagination and no line-number
/// gutter (converted documents are not edited by line).
fn paginate_markdown(
    markdown: &str,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<String, String> {
    let lines: Vec<&str> = markdown.lines().collect();
    let offset = offset.unwrap_or(1).max(1) as usize;
    if offset > lines.len() && !(offset == 1 && lines.is_empty()) {
        return Err(format!(
            "Offset {offset} is out of range for this conversion ({} lines)",
            lines.len()
        ));
    }
    let limit = limit.map(|n| n as usize).unwrap_or(DEFAULT_READ_LIMIT);
    let start = offset - 1;
    let end = (start + limit).min(lines.len());
    let mut out = String::new();
    for line in &lines[start..end] {
        let line = if line.chars().count() > MAX_LINE_LENGTH {
            let head: String = line.chars().take(MAX_LINE_LENGTH).collect();
            format!("{head}{MAX_LINE_SUFFIX}")
        } else {
            (*line).to_string()
        };
        out.push_str(&line);
        out.push('\n');
    }
    let last = start + (end - start);
    if last < lines.len() {
        out.push_str(&format!(
            "\n(Showing lines {}-{} of {}. Use offset={} to continue.)",
            offset,
            last,
            lines.len(),
            last + 1
        ));
    } else {
        out.push_str(&format!(
            "\n(End of document - total {} lines)",
            lines.len()
        ));
    }
    Ok(out)
}

fn capped_text(
    path: &str,
    out: String,
    max_output_chars: usize,
    max_output_bytes: usize,
) -> Result<ToolOutput, String> {
    let hint = format!("use offset/limit to read more of {path}");
    if let Some(capped) = crate::truncate::truncate_output(&out, max_output_chars, &hint) {
        return Ok(ToolOutput::text(capped));
    }
    if let Some(capped) = crate::truncate::truncate_bytes(&out, max_output_bytes, &hint) {
        return Ok(ToolOutput::text(capped));
    }
    Ok(ToolOutput::text(out))
}

/// Maps a document conversion failure to a message the model can act on.
pub(crate) fn document_error_message(path: &str, error: &shuvarie_doc::ConvertError) -> String {
    match error {
        shuvarie_doc::ConvertError::NeedsOcr { pages, page_count } => {
            let listed = pages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{path}: pages {listed} look scanned or image-only and need OCR, which \
                 read_file does not provide (document has {page_count} pages)"
            )
        }
        shuvarie_doc::ConvertError::Encrypted => {
            format!(
                "{path}: the document is encrypted or password-protected; decrypt or export an \
                 unencrypted copy first"
            )
        }
        shuvarie_doc::ConvertError::Unsupported(detail) => {
            format!("{path}: unsupported document format ({detail})")
        }
        shuvarie_doc::ConvertError::Malformed {
            part: Some(part),
            detail,
        } => {
            format!("{path}: malformed document [{part}]: {detail}")
        }
        shuvarie_doc::ConvertError::Malformed { part: None, detail } => {
            format!("{path}: malformed document: {detail}")
        }
    }
}

fn is_binary_file(data: &[u8]) -> bool {
    if data.contains(&0) {
        return true;
    }
    let sample = &data[..data.len().min(BINARY_SAMPLE_BYTES)];
    if sample.is_empty() {
        return false;
    }
    let non_printable = sample
        .iter()
        .filter(|&&b| b < 9 || (b > 13 && b < 32))
        .count();
    non_printable * 10 > sample.len() * 3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    fn read_file_tool() -> ReadFile {
        ReadFile::new(
            ReadCache::new(),
            0,
            0,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings::default(),
            true,
        )
    }

    fn fixture(name: &str) -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    fn png_bytes(side: u32) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(side, side)
            .write_to(&mut buffer, image::ImageFormat::Png)
            .unwrap();
        buffer.into_inner()
    }

    #[tokio::test]
    async fn read_denied_by_rule_errors() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\n").unwrap();
        let tool = ReadFile::new(
            ReadCache::new(),
            0,
            0,
            crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
                default: Some(shuvarie_config::Verb::Deny),
                paths: shuvarie_config::RuleSet::default(),
                ..shuvarie_config::PermissionsConfig::builtin()
            }),
            crate::attachments::AttachmentSettings::default(),
            true,
        );
        let err = tool
            .call(&mut new_ctx(), json!({ "path": "a.txt" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission denied"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_file_with_range() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\nthree\n").unwrap();
        let out = read_file_tool()
            .call(
                &mut new_ctx(),
                json!({ "path": "a.txt", "offset": 2, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("two"),
            "{}",
            out.as_text().unwrap()
        );
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "missing.txt" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing.txt"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_refuses_binary() {
        let (dir, _guard) = tempdir();
        std::fs::write("bin.dat", [0, 1, 2, 3]).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "bin.dat" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_by_bytes() {
        let (dir, _guard) = tempdir();
        std::fs::write("bytes.txt", "abcdef\n").unwrap();
        let tool = ReadFile::new(
            ReadCache::new(),
            0,
            3,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings::default(),
            true,
        );
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "bytes.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(
            text.contains("bytes omitted") && text.contains("use offset/limit"),
            "{text}"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_cache_dedupes_repeated_reads() {
        let (dir, _guard) = tempdir();
        std::fs::write("dup.txt", "line\n").unwrap();
        let cache = ReadCache::new();
        let tool = ReadFile::new(
            cache.clone(),
            0,
            0,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings::default(),
            true,
        );
        let first = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(first.as_text().unwrap().contains("line"));
        let second = tool
            .call(&mut new_ctx(), json!({ "path": "dup.txt" }))
            .await
            .unwrap();
        assert!(
            second.as_text().unwrap().contains("already read"),
            "{}",
            second.as_text().unwrap()
        );
        assert!(!second.as_text().unwrap().contains("line |"));
        let ranged = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "dup.txt", "offset": 1, "limit": 1 }),
            )
            .await
            .unwrap();
        assert!(
            ranged.as_text().unwrap().contains("line"),
            "{}",
            ranged.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn read_truncates_large_output() {
        let (dir, _guard) = tempdir();
        let big = "x".repeat(10_000) + "\n";
        std::fs::write("big.txt", &big).unwrap();
        let tool = ReadFile::new(
            ReadCache::new(),
            100,
            0,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings::default(),
            true,
        );
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "big.txt" }))
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("truncated"),
            "{}",
            out.as_text().unwrap()
        );
        assert!(out.as_text().unwrap().chars().count() < big.len() + 200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_defaults_to_2000_lines_with_footers() {
        let (dir, _guard) = tempdir();
        let content: String = (1..=2100).map(|i| format!("line{i}\n")).collect();
        std::fs::write("many.txt", &content).unwrap();
        let tool = read_file_tool();
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "many.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Showing lines 1-2000 of 2100"), "{text}");
        assert!(text.contains("Use offset=2001"), "{text}");
        let rest = tool
            .call(
                &mut new_ctx(),
                json!({ "path": "many.txt", "offset": 2001 }),
            )
            .await
            .unwrap();
        let text = rest.as_text().unwrap();
        assert!(text.contains("End of file - total 2100 lines"), "{text}");
        assert!(text.contains("2100 | line2100"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn read_offset_out_of_range_errors() {
        let (dir, _guard) = tempdir();
        std::fs::write("small.txt", "a\nb\nc\n").unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "small.txt", "offset": 10 }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("out of range"),
            "{}",
            err.to_string()
        );
        assert!(err.to_string().contains("3 lines"));
        drop(dir);
    }

    #[tokio::test]
    async fn read_caps_long_lines() {
        let (dir, _guard) = tempdir();
        std::fs::write("long.txt", "z".repeat(3000) + "\n").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "long.txt" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains(MAX_LINE_SUFFIX), "{text}");
        assert!(text.chars().count() < 2200);
        drop(dir);
    }

    #[tokio::test]
    async fn read_detects_binary_by_nonprintable_ratio() {
        let (dir, _guard) = tempdir();
        let mut data = vec![b'a'; 600];
        data.extend(std::iter::repeat_n(0x07u8, 600));
        std::fs::write("weird.bin", &data).unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "weird.bin" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("binary"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn read_reaches_beyond_64kb() {
        let (dir, _guard) = tempdir();
        let line = "y".repeat(1000) + "\n";
        std::fs::write("wide.txt", line.repeat(100)).unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "wide.txt", "offset": 90 }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("90 |"), "{text}");
        assert!(text.contains("End of file - total 100 lines"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn reads_docx_fixture() {
        let (dir, _guard) = tempdir();
        std::fs::copy(fixture("report.docx"), "report.docx").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "report.docx" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Quarterly Report"), "{text}");
        assert!(text.contains("Anvil"), "{text}");
        assert!(text.contains('|'), "expected a GFM table: {text}");
        drop(dir);
    }

    #[tokio::test]
    async fn reads_pdf_fixture() {
        let (dir, _guard) = tempdir();
        std::fs::copy(fixture("plan.pdf"), "plan.pdf").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "plan.pdf" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Shuvarie reads PDF documents"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn reads_csv_as_table_and_dedupes_with_document_namespace() {
        let (dir, _guard) = tempdir();
        std::fs::write("data.csv", "id,city\n1,Reykjavik\n2,Tromso\n").unwrap();
        let tool = read_file_tool();
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "data.csv" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Tromso"), "{text}");
        assert!(text.contains('|'), "expected markdown table: {text}");
        let again = tool
            .call(&mut new_ctx(), json!({ "path": "data.csv" }))
            .await
            .unwrap();
        assert!(
            again.as_text().unwrap().contains("already read"),
            "{}",
            again.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn reads_rtf_fixture() {
        let (dir, _guard) = tempdir();
        std::fs::write("notes.rtf", r"{\rtf1\ansi Shuvarie parses RTF files.\par}").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "notes.rtf" }))
            .await
            .unwrap();
        assert!(
            out.as_text().unwrap().contains("Shuvarie parses RTF files"),
            "{}",
            out.as_text().unwrap()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn rejects_malformed_containers() {
        let (dir, _guard) = tempdir();
        std::fs::write("junk.docx", b"PK\x03\x04 this is not really a zip").unwrap();
        let err = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "junk.docx" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("junk.docx"), "{}", err.to_string());
        drop(dir);
    }

    #[tokio::test]
    async fn missing_empty_and_directory_paths_error() {
        let (dir, _guard) = tempdir();
        std::fs::create_dir("empty.csv").unwrap();
        std::fs::write("void.docx", "").unwrap();
        for case in ["missing.docx", "void.docx", "empty.csv"] {
            let err = read_file_tool()
                .call(&mut new_ctx(), json!({ "path": case }))
                .await
                .unwrap_err();
            assert!(err.to_string().contains(case), "case {case}: {err}");
        }
        drop(dir);
    }

    #[tokio::test]
    async fn legacy_doc_and_ppt_get_conversion_hints() {
        let (dir, _guard) = tempdir();
        std::fs::write("legacy.doc", b"\xD0\xCF\x11\xE0 legacy container").unwrap();
        std::fs::write("deck.ppt", b"\xD0\xCF\x11\xE0 legacy container").unwrap();
        for (path, hint) in [
            ("legacy.doc", "soffice --convert-to docx"),
            ("deck.ppt", "soffice --convert-to pptx"),
        ] {
            let err = read_file_tool()
                .call(&mut new_ctx(), json!({ "path": path }))
                .await
                .unwrap_err();
            assert!(err.to_string().contains(path), "{err}");
            assert!(err.to_string().contains(hint), "{err}");
        }
        drop(dir);
    }

    /// A configured legacy-converter converts the .doc through it: a fake
    /// converter script (unix) copies a real docx fixture as its output.
    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_doc_converts_through_the_configured_program() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _guard) = tempdir();
        std::fs::write("legacy.doc", b"legacy binary container").unwrap();
        let script = dir.path().join("fake-convert.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n# $6 is the --outdir argument\ncp '{}' \"$6/document.docx\"\n",
                fixture("report.docx")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let tool = ReadFile::new(
            ReadCache::new(),
            0,
            0,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings {
                office_converter: Some(script.to_string_lossy().into_owned()),
                ..crate::attachments::AttachmentSettings::default()
            },
            true,
        );
        let out = tool
            .call(&mut new_ctx(), json!({ "path": "legacy.doc" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Quarterly Report"), "{text}");
        drop(dir);
    }

    /// An image read returns the image to the model as tool-result content
    /// plus a text note describing it.
    #[tokio::test]
    async fn reads_png_as_image_content() {
        let (_dir, _guard) = tempdir();
        let raw = png_bytes(32);
        std::fs::write("shot.png", &raw).unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "shot.png" }))
            .await
            .unwrap();
        let blocks = out.as_content();
        assert_eq!(blocks.len(), 2, "note + image");
        let note = blocks[0].as_text().unwrap_or_default();
        assert!(note.contains("shot.png"), "{note}");
        assert!(note.contains("image/png"), "{note}");
    }

    /// An oversized image is downscaled to the configured edge cap before it
    /// reaches the model.
    #[tokio::test]
    async fn oversized_image_downscales_to_the_edge_cap() {
        let (dir, _guard) = tempdir();
        std::fs::write("big.png", png_bytes(2000)).unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "big.png" }))
            .await
            .unwrap();
        let blocks = out.as_content();
        let encoded = blocks
            .iter()
            .find_map(|block| match block {
                shuvarie_llm::ToolResultContent::Image(_) => Some(block.clone()),
                _ => None,
            })
            .expect("an image block");
        let Some(image) = block_image(&encoded) else {
            panic!("image block missing");
        };
        let decoded = image::load_from_memory(&image).unwrap();
        assert_eq!(decoded.width().max(decoded.height()), 1568);
        drop(dir);
    }

    fn block_image(block: &shuvarie_llm::ToolResultContent) -> Option<Vec<u8>> {
        let shuvarie_llm::ToolResultContent::Image(image) = block else {
            return None;
        };
        match &image.data {
            shuvarie_llm::DocumentSourceKind::Base64(encoded) => {
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded).ok()
            }
            _ => None,
        }
    }

    /// A non-image-capable model must not receive image blocks: the read
    /// errors with an actionable message.
    #[tokio::test]
    async fn image_read_refused_when_model_lacks_image_support() {
        let (dir, _guard) = tempdir();
        std::fs::write("shot.png", png_bytes(4)).unwrap();
        let tool = ReadFile::new(
            ReadCache::new(),
            0,
            0,
            crate::test_util::access(),
            crate::attachments::AttachmentSettings::default(),
            false,
        );
        let err = tool
            .call(&mut new_ctx(), json!({ "path": "shot.png" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("shot.png"), "{}", err);
        assert!(err.to_string().contains("image"), "{}", err);
        drop(dir);
    }

    /// Classification is content-first: a text file with a misleading image
    /// extension still reads as text.
    #[tokio::test]
    async fn mislabeled_extension_falls_back_to_content() {
        let (dir, _guard) = tempdir();
        std::fs::write("notes.png", "plain text under a png name\n").unwrap();
        let out = read_file_tool()
            .call(&mut new_ctx(), json!({ "path": "notes.png" }))
            .await
            .unwrap();
        assert!(
            out.as_text()
                .unwrap()
                .contains("1 | plain text under a png name"),
            "{}",
            out.as_text().unwrap()
        );
        drop(dir);
    }
}
