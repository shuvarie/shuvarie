//! `shuvarie-doc`: converts document files to GitHub-Flavored Markdown.
//!
//! Formats are converted by the best available engine:
//!
//! - **Delegated parsers** for formats with mature Rust implementations:
//!   spreadsheets via `calamine` (xlsx/xlsm/xlsb/xls/ods), PDF text via
//!   `pdf-extract`, RTF via `rtf-parser`, CSV via `csv`.
//! - **Hand-rolled walkers** for formats that are just zip + XML (docx, pptx,
//!   odt, odp) and epub (XHTML spine), built on `quick-xml`/`zip` and
//!   `html-to-markdown-rs`.
//!
//! Legacy binary Word (.doc) and PowerPoint (.ppt) files are recognized but
//! rejected with a conversion hint; no maintained pure-Rust parser exists for
//! them.
//!
//! The public surface is deliberately small: [`detect`] resolves a format,
//! [`to_markdown`] converts it, and [`ConvertError`] reports actionable
//! failure classes (scanned pages needing OCR, encryption, malformed input,
//! unsupported formats).

mod csv_doc;
mod docx;
mod epub;
mod md;
mod odf;
mod pdf;
mod pptx;
mod rtf;
mod sheet;
mod xml;

use std::fmt;

#[cfg(test)]
mod tests;

/// Document formats that document conversion understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceFormat {
    Csv,
    Docx,
    Epub,
    Odp,
    Ods,
    Odt,
    Pdf,
    Pptx,
    Rtf,
    Xls,
    Xlsb,
    Xlsm,
    Xlsx,
    /// Legacy binary Word document (OLE2 container).
    LegacyDoc,
    /// Legacy binary PowerPoint presentation (OLE2 container).
    LegacyPpt,
}

impl SourceFormat {
    /// The label used when reporting malformed input for this format.
    pub(crate) fn part(self) -> &'static str {
        match self {
            SourceFormat::Csv => "csv",
            SourceFormat::Docx => "docx",
            SourceFormat::Epub => "epub",
            SourceFormat::Odp => "odp",
            SourceFormat::Ods => "ods",
            SourceFormat::Odt => "odt",
            SourceFormat::Pdf => "pdf",
            SourceFormat::Pptx => "pptx",
            SourceFormat::Rtf => "rtf",
            SourceFormat::Xls => "xls",
            SourceFormat::Xlsb => "xlsb",
            SourceFormat::Xlsm => "xlsm",
            SourceFormat::Xlsx => "xlsx",
            SourceFormat::LegacyDoc => "doc",
            SourceFormat::LegacyPpt => "ppt",
        }
    }
}

/// Failure classes surfaced while converting a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConvertError {
    /// No extractable text where text was expected; the listed 1-based pages
    /// look scanned or image-only and need OCR.
    NeedsOcr {
        pages: Vec<usize>,
        page_count: usize,
    },
    /// The document is encrypted or password-protected.
    Encrypted,
    /// The recognized container/parser choked on malformed input.
    Malformed {
        part: Option<&'static str>,
        detail: String,
    },
    /// A recognized but unreadable format family (e.g. legacy binary).
    Unsupported(String),
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConvertError::NeedsOcr { pages, page_count } => {
                let listed = pages.iter().map(|page| *page as u32).collect::<Vec<_>>();
                write!(
                    f,
                    "no extractable text: pages {listed:?} look scanned or image-only and need OCR (document has {page_count} pages)"
                )
            }
            ConvertError::Encrypted => {
                write!(f, "the document is encrypted or password-protected")
            }
            ConvertError::Malformed {
                part: Some(part),
                detail,
            } => {
                write!(f, "malformed {part} document: {detail}")
            }
            ConvertError::Malformed { part: None, detail } => {
                write!(f, "malformed document: {detail}")
            }
            ConvertError::Unsupported(detail) => write!(f, "unsupported document format: {detail}"),
        }
    }
}

impl std::error::Error for ConvertError {}

/// Identifies the format of `bytes`, preferring a content signature over the
/// file `ext`ension (which only breaks ties for signature-less formats such
/// as CSV and resolves mislabeled files).
pub fn detect(bytes: &[u8], ext: Option<&str>) -> Option<SourceFormat> {
    signature(bytes).or_else(|| from_ext(ext))
}

/// Reads the format from the file extension, if any.
fn from_ext(ext: Option<&str>) -> Option<SourceFormat> {
    ext.map(str::to_ascii_lowercase)
        .and_then(|ext| match ext.as_str() {
            "csv" => Some(SourceFormat::Csv),
            "docx" => Some(SourceFormat::Docx),
            "doc" => Some(SourceFormat::LegacyDoc),
            "epub" => Some(SourceFormat::Epub),
            "odp" => Some(SourceFormat::Odp),
            "ods" => Some(SourceFormat::Ods),
            "odt" => Some(SourceFormat::Odt),
            "pdf" => Some(SourceFormat::Pdf),
            "pptx" => Some(SourceFormat::Pptx),
            "ppt" => Some(SourceFormat::LegacyPpt),
            "rtf" => Some(SourceFormat::Rtf),
            "xls" => Some(SourceFormat::Xls),
            "xlsb" => Some(SourceFormat::Xlsb),
            "xlsm" => Some(SourceFormat::Xlsm),
            "xlsx" => Some(SourceFormat::Xlsx),
            _ => None,
        })
}

/// Reads the format from a content signature (magic bytes, zip members).
fn signature(bytes: &[u8]) -> Option<SourceFormat> {
    if let Some(pdf_or_rtf) = prefix_signature(bytes) {
        return Some(pdf_or_rtf);
    }
    zip_signature(bytes)
}

fn prefix_signature(bytes: &[u8]) -> Option<SourceFormat> {
    // PDF headers may be preceded by a small amount of binary junk.
    let head = bytes.get(..1024).unwrap_or(bytes);
    if head.windows(4).any(|window| window == b"%PDF") {
        return Some(SourceFormat::Pdf);
    }
    if bytes.starts_with(b"{\\rtf") {
        return Some(SourceFormat::Rtf);
    }
    None
}

/// Sniffs zip containers by their structural members. A junk file that merely
/// starts with the zip magic yields `None` and leaves the decision to the
/// extension fallback.
fn zip_signature(bytes: &[u8]) -> Option<SourceFormat> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return None;
    }
    let Ok(mut archive) = xml::open_zip(bytes) else {
        return None;
    };
    let members: Vec<String> = archive.file_names().map(str::to_string).collect();
    let has = |name: &str| members.iter().any(|member| member == name);
    // EPUB first: it carries an explicit manifest pointer.
    if has("META-INF/container.xml") {
        return Some(SourceFormat::Epub);
    }
    if has("word/document.xml") {
        return Some(SourceFormat::Docx);
    }
    if has("ppt/presentation.xml") {
        return Some(SourceFormat::Pptx);
    }
    // OpenDocument packages advertise themselves in the required `mimetype`
    // member, stored uncompressed first.
    if has("mimetype") {
        let mimetype = xml::member(&mut archive, "mimetype").unwrap_or_default();
        let mimetype = String::from_utf8_lossy(&mimetype);
        if let Some(source_format) = mime_signature(mimetype.trim()) {
            return Some(source_format);
        }
    }
    if has("xl/workbook.bin") {
        return Some(SourceFormat::Xlsb);
    }
    if has("xl/workbook.xml") {
        return Some(if has("xl/vbaProject.bin") {
            SourceFormat::Xlsm
        } else {
            SourceFormat::Xlsx
        });
    }
    None
}

fn mime_signature(mimetype: &str) -> Option<SourceFormat> {
    let opendocument = "application/vnd.oasis.opendocument.";
    mimetype
        .strip_prefix(opendocument)
        .and_then(|family| match family {
            "text" => Some(SourceFormat::Odt),
            "spreadsheet" => Some(SourceFormat::Ods),
            "presentation" => Some(SourceFormat::Odp),
            _ => None,
        })
}

/// Converts `bytes` in the given [`SourceFormat`] to GitHub-Flavored Markdown.
///
/// The result is normalized: `\r` sequences are folded to `\n`, runs of blank
/// lines collapse to a single blank line, and trailing whitespace is trimmed.
pub fn to_markdown(format: SourceFormat, bytes: &[u8]) -> Result<String, ConvertError> {
    // Document parsers come from a mix of crates and hand-rolled walkers; a
    // parser hitting an unwrap on hostile input degrades to a Malformed error
    // instead of taking the tool call down.
    let part = format.part();
    let result = run_quietly(part, move || match format {
        SourceFormat::Csv => csv_doc::to_markdown(bytes),
        SourceFormat::Docx => docx::to_markdown(bytes),
        SourceFormat::Epub => epub::to_markdown(bytes),
        SourceFormat::Odp => odf::odp_to_markdown(bytes),
        SourceFormat::Ods
        | SourceFormat::Xls
        | SourceFormat::Xlsb
        | SourceFormat::Xlsm
        | SourceFormat::Xlsx => sheet::to_markdown(format, bytes),
        SourceFormat::Odt => odf::odt_to_markdown(bytes),
        SourceFormat::Pdf => pdf::to_markdown(bytes),
        SourceFormat::Pptx => pptx::to_markdown(bytes),
        SourceFormat::Rtf => rtf::to_markdown(bytes),
        SourceFormat::LegacyDoc => legacy_message(format),
        SourceFormat::LegacyPpt => legacy_message(format),
    });
    result.map(&normalize)
}

/// The legacy binary formats have no maintained pure-Rust reader; point the
/// caller at a conversion command instead.
fn legacy_message(format: SourceFormat) -> Result<String, ConvertError> {
    let detail = match format {
        SourceFormat::LegacyDoc => {
            "legacy binary Word document (.doc); convert it first, e.g. `soffice --convert-to docx <file>` and read the .docx"
        }
        SourceFormat::LegacyPpt => {
            "legacy binary PowerPoint presentation (.ppt); convert it first, e.g. `soffice --convert-to pptx <file>` and read the .pptx"
        }
        _ => return Ok(String::new()),
    };
    Err(ConvertError::Unsupported(detail.to_string()))
}

/// Runs a document parser with panics demoted to [`ConvertError::Malformed`]
/// and the global panic hook silenced for the duration (its stderr chatter
/// would otherwise bleed into the TUI).
fn run_quietly(
    part: &'static str,
    f: impl FnOnce() -> Result<String, ConvertError>,
) -> Result<String, ConvertError> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(previous);
    result.unwrap_or_else(|panic| {
        let detail = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                panic
                    .downcast_ref::<&str>()
                    .map(|message| (*message).to_string())
            })
            .unwrap_or_else(|| "internal parser failure".to_string());
        Err(ConvertError::Malformed {
            part: Some(part),
            detail: format!("parser failed: {detail}"),
        })
    })
}

/// Normalizes converted markdown: folds `\r`, collapses runs of blank lines,
/// and trims trailing whitespace while keeping the end newline convention.
fn normalize(markdown: String) -> String {
    let mut out = String::with_capacity(markdown.len());
    let mut newlines = 0;
    for ch in markdown.replace("\r\n", "\n").replace('\r', "\n").chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                out.push('\n');
            }
        } else {
            newlines = 0;
            out.push(ch);
        }
    }
    if out.ends_with("\n\n") {
        out.pop();
    }
    out
}
