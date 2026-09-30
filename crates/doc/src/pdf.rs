//! PDF → GitHub-Flavored Markdown text, on top of `pdf-extract`.
//!
//! Extraction is text-layer only: paragraphs are rebuilt from extraction
//! lines (dehyphenating words broken across lines), pages are separated by
//! blank lines, and pages that yield no text are flagged (scanned PDFs).

use crate::{ConvertError, md};

pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let pages = pdf_extract::extract_text_from_mem_by_pages(bytes)
        .map_err(|error| classify(bytes, error))?;
    let page_count = pages.len();
    let empty: Vec<usize> = pages
        .iter()
        .enumerate()
        .filter(|(_, text)| text.trim().is_empty())
        .map(|(index, _)| index + 1)
        .collect();
    if empty.len() == page_count && page_count > 0 {
        return Err(ConvertError::NeedsOcr {
            pages: empty,
            page_count,
        });
    }
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    for text in &pages {
        page_blocks(text, &mut blocks);
    }
    if !empty.is_empty() {
        let listed: Vec<u32> = empty.iter().map(|page| *page as u32).collect();
        blocks.push(md::MdBlock::Para {
            lines: vec![format!(
                "_({listed:?} of {page_count} pages contained no extractable text; likely scanned or image-only)_"
            )],
        });
    }
    Ok(md::render(&blocks))
}

/// Classifies a PDF extraction failure: password/security-handler failures
/// (or an `/Encrypt` marker in the raw bytes) map to `Encrypted`, everything
/// else to `Malformed`.
fn classify(bytes: &[u8], error: pdf_extract::OutputError) -> ConvertError {
    let encrypted = matches!(
        &error,
        pdf_extract::OutputError::PdfError(
            pdf_extract::Error::InvalidPassword
                | pdf_extract::Error::AlreadyEncrypted
                | pdf_extract::Error::Decryption(_)
                | pdf_extract::Error::UnsupportedSecurityHandler(_)
        )
    ) || bytes.windows(8).any(|window| window == b"/Encrypt");
    if encrypted {
        return ConvertError::Encrypted;
    }
    ConvertError::Malformed {
        part: Some("pdf"),
        detail: error.to_string(),
    }
}

/// Groups extraction lines into paragraphs: blank lines separate, single
/// line breaks join with a space, and hyphen-broken words are re-joined.
fn page_blocks(text: &str, blocks: &mut Vec<md::MdBlock>) {
    let mut paragraph = String::new();
    let flush = |paragraph: &mut String, blocks: &mut Vec<md::MdBlock>| {
        let text = paragraph.trim();
        if !text.is_empty() {
            blocks.push(md::MdBlock::Para {
                lines: vec![text.to_string()],
            });
        }
        paragraph.clear();
    };
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            flush(&mut paragraph, blocks);
            continue;
        }
        if paragraph.is_empty() {
            paragraph.push_str(line);
        } else if paragraph.ends_with('-')
            && !paragraph.ends_with("--")
            && matches!(line.chars().next(), Some('a'..='z'))
        {
            // "intra-\nline" → "intraline": the extraction split a word.
            paragraph.pop();
            paragraph.push_str(line);
        } else {
            paragraph.push(' ');
            paragraph.push_str(line);
        }
    }
    flush(&mut paragraph, blocks);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_paragraphs_and_dehyphenates() {
        let mut blocks: Vec<md::MdBlock> = Vec::new();
        page_blocks("first line\nsecond line\n\nnew paragraph\n", &mut blocks);
        let rendered = md::render(&blocks);
        assert!(rendered.contains("first line second line"), "{rendered}");
        assert!(rendered.contains("\n\nnew paragraph"), "{rendered}");
        let hyphenated = to_markdown(b"%PDF-1.4 dummy").err(); // no pages
        assert!(hyphenated.is_some());
        let mut joined: Vec<md::MdBlock> = Vec::new();
        page_blocks("intra-\nline wrap\n", &mut joined);
        assert!(
            md::render(&joined).contains("intraline wrap"),
            "{}",
            md::render(&joined)
        );
    }
}
