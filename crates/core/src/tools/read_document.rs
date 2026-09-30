use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::permissions::{Access, PathKind, resolve_read};

use super::{ReadCache, arg_value};

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_LINE_SUFFIX: &str = "... (line truncated to 2000 chars)";
const SUPPORTED_EXTENSIONS: &str =
    "docx, pdf, pptx, xls/xlsx/xlsm/xlsb, ods, odt, odp, rtf, epub, csv";

pub(crate) struct ReadDocument {
    read_cache: ReadCache,
    max_output_chars: usize,
    max_output_bytes: usize,
    access: Access,
}

impl ReadDocument {
    pub(crate) fn new(
        read_cache: ReadCache,
        max_output_chars: usize,
        max_output_bytes: usize,
        access: Access,
    ) -> Self {
        Self {
            read_cache,
            max_output_chars,
            max_output_bytes,
            access,
        }
    }
}

impl Tool for ReadDocument {
    const NAME: &'static str = "read_document";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Read a document file (docx, pdf, pptx, xls/xlsx/xlsm/xlsb, odt/ods/odp, rtf, epub, csv) and return its content converted to GitHub-Flavored Markdown. Use read_file for plain text files and code; scanned or image-only pages cannot be read (no OCR). Legacy binary .doc/.ppt are not readable; ask the user to convert them first (e.g. `soffice --convert-to docx file.doc`)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path of the document, relative to the workspace root" },
                "offset": { "type": "integer", "minimum": 1, "description": "First line of the converted markdown to return (1-based). Defaults to 1" },
                "limit": { "type": "integer", "minimum": 1, "description": format!("Maximum number of lines to return. Defaults to {DEFAULT_READ_LIMIT}") }
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
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let offset = args.get("offset").and_then(Value::as_u64);
            let limit = args.get("limit").and_then(Value::as_u64);
            if read_cache.mark_document(&path, offset, limit) {
                return Ok(ToolOutput::text(format!(
                    "(already read {path} — see the earlier result; use a different offset/limit to re-read a range)"
                )));
            }
            let abs = resolve_read(&path)?;
            access.authorize_path(PathKind::Read, &abs, &path).await?;
            if abs.is_dir() {
                return Err(format!("'{path}' is a directory, not a document"));
            }
            let data =
                tokio::fs::read(&abs).await.map_err(|e| format!("read {path}: {e}"))?;
            if data.is_empty() {
                return Err(format!("'{path}' is empty"));
            }
            // Content signature wins; the extension only breaks ties for
            // signature-less formats (csv) and mislabeled files.
            let extension =
                std::path::Path::new(&path).extension().and_then(std::ffi::OsStr::to_str);
            let format = shuvarie_doc::detect(&data, extension)
                .ok_or_else(|| {
                    format!(
                        "'{path}' is not a recognized document; read_document supports {SUPPORTED_EXTENSIONS}. Plain text and code belong to read_file."
                    )
                })?;
            let markdown =
                tokio::task::spawn_blocking(move || shuvarie_doc::to_markdown(format, &data))
                    .await
                    .map_err(|e| format!("read {path}: conversion failed: {e}"))?
                    .map_err(|e| document_error_message(&path, &e))?;
            let out = paginate_markdown(&markdown, offset, limit)?;
            let hint = format!("use offset/limit to read more of {path}");
            if let Some(capped) = crate::truncate::truncate_output(&out, max_output_chars, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            if let Some(capped) = crate::truncate::truncate_bytes(&out, max_output_bytes, &hint) {
                return Ok(ToolOutput::text(capped));
            }
            Ok(ToolOutput::text(out))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// Renders the converted markdown with read_file-style line pagination and
/// over-long-line capping. Converted documents are not edited by line, so no
/// line-number gutter is emitted; `offset`/`limit` index into the converted
/// markdown lines.
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

/// Maps a conversion failure to a message the model can act on.
fn document_error_message(path: &str, error: &shuvarie_doc::ConvertError) -> String {
    match error {
        shuvarie_doc::ConvertError::NeedsOcr { pages, page_count } => {
            let listed = pages
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "{path}: pages {listed} look scanned or image-only and need OCR, which read_document does not provide (document has {page_count} pages)"
            )
        }
        shuvarie_doc::ConvertError::Encrypted => {
            format!(
                "{path}: the document is encrypted or password-protected; decrypt or export an unencrypted copy first"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{new_ctx, tempdir};

    fn tool() -> ReadDocument {
        ReadDocument::new(ReadCache::new(), 0, 0, crate::test_util::access())
    }

    fn fixture(name: &str) -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn paginate_pages_with_footers() {
        let markdown = "one\ntwo\nthree\nfour\n";
        let full = paginate_markdown(markdown, None, Some(2)).unwrap();
        assert!(full.contains("one\n"), "{full}");
        assert!(full.contains("Showing lines 1-2 of 4"), "{full}");
        assert!(full.contains("Use offset=3"), "{full}");
        let rest = paginate_markdown(markdown, Some(3), Some(2)).unwrap();
        assert!(rest.contains("three\n"), "{rest}");
        assert!(rest.contains("End of document - total 4 lines"), "{rest}");
    }

    #[test]
    fn paginate_rejects_out_of_range_offsets() {
        let err = paginate_markdown("one\ntwo\n", Some(10), None).unwrap_err();
        assert!(err.contains("out of range"), "{err}");
        assert!(err.contains("2 lines"), "{err}");
        let ok = paginate_markdown("", None, None).unwrap();
        assert!(ok.contains("total 0 lines"), "{ok}");
    }

    #[test]
    fn paginate_caps_long_lines() {
        let line = "z".repeat(3000);
        let out = paginate_markdown(&line, None, None).unwrap();
        assert!(out.contains(MAX_LINE_SUFFIX), "{out}");
        assert!(out.chars().count() < 2200);
    }

    #[test]
    fn ocr_errors_name_the_pages() {
        let error = shuvarie_doc::ConvertError::NeedsOcr {
            pages: vec![1, 3],
            page_count: 4,
        };
        let message = document_error_message("scan.pdf", &error);
        assert!(message.contains("pages 1, 3"), "{message}");
        assert!(message.contains("OCR"), "{message}");
        assert!(message.contains("4 pages"), "{message}");
        let encrypted = document_error_message("doc.docx", &shuvarie_doc::ConvertError::Encrypted);
        assert!(encrypted.contains("encrypted"), "{encrypted}");
    }

    #[tokio::test]
    async fn reads_docx_fixture() {
        let (dir, _guard) = tempdir();
        std::fs::copy(fixture("report.docx"), "report.docx").unwrap();
        let out = tool()
            .call(&mut new_ctx(), json!({ "path": "report.docx" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Quarterly Report"), "{text}");
        assert!(text.contains("42 percent"), "{text}");
        assert!(text.contains("Anvil"), "{text}");
        assert!(text.contains('|'), "expected a GFM table: {text}");
        drop(dir);
    }

    #[tokio::test]
    async fn reads_pdf_fixture() {
        let (dir, _guard) = tempdir();
        std::fs::copy(fixture("plan.pdf"), "plan.pdf").unwrap();
        let out = tool()
            .call(&mut new_ctx(), json!({ "path": "plan.pdf" }))
            .await
            .unwrap();
        let text = out.as_text().unwrap();
        assert!(text.contains("Shuvarie reads PDF documents"), "{text}");
        drop(dir);
    }

    #[tokio::test]
    async fn reads_csv_as_table_and_dedupes() {
        let (dir, _guard) = tempdir();
        std::fs::write("data.csv", "id,city\n1,Reykjavik\n2,Tromso\n").unwrap();
        let tool = tool();
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
        let out = tool()
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
    async fn rejects_unrecognized_documents() {
        let (dir, _guard) = tempdir();
        std::fs::write("notes.txt", "plain text belongs to read_file\n").unwrap();
        let err = tool()
            .call(&mut new_ctx(), json!({ "path": "notes.txt" }))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("notes.txt"), "{}", err.to_string());
        assert!(err.to_string().contains("read_file"), "{}", err.to_string());
        drop(dir);
    }

    #[tokio::test]
    async fn rejects_malformed_containers() {
        let (dir, _guard) = tempdir();
        std::fs::write("junk.docx", b"PK\x03\x04 this is not really a zip").unwrap();
        let err = tool()
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
            let err = tool()
                .call(&mut new_ctx(), json!({ "path": case }))
                .await
                .unwrap_err();
            assert!(
                err.to_string().contains(case),
                "case {case}: {}",
                err.to_string()
            );
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
            let err = tool()
                .call(&mut new_ctx(), json!({ "path": path }))
                .await
                .unwrap_err();
            assert!(err.to_string().contains(path), "{err}");
            assert!(err.to_string().contains(hint), "{err}");
        }
        drop(dir);
    }

    #[tokio::test]
    async fn denied_by_rule_errors() {
        let (dir, _guard) = tempdir();
        std::fs::write("a.docx", b"PK\x03\x04").unwrap();
        let tool = ReadDocument::new(
            ReadCache::new(),
            0,
            0,
            crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
                default: Some(shuvarie_config::Verb::Deny),
                paths: shuvarie_config::RuleSet::default(),
                ..shuvarie_config::PermissionsConfig::builtin()
            }),
        );
        let err = tool
            .call(&mut new_ctx(), json!({ "path": "a.docx" }))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("permission denied"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }
}
