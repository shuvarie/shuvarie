//! RTF → GitHub-Flavored Markdown, on top of `rtf-parser` 0.4.
//!
//! The parsed body is a flat list of styled runs; runs are grouped into
//! paragraphs whenever the run's paragraph layout (alignment, indents,
//! spacing) differs from the previous run, which matches how single-paragraph
//! and multi-paragraph RTFs are laid out in practice.

use crate::{ConvertError, md};

pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let text = std::str::from_utf8(bytes).map_err(|error| ConvertError::Malformed {
        part: Some("rtf"),
        detail: error.to_string(),
    })?;
    let document = rtf_parser::document::RtfDocument::try_from(text).map_err(|error| {
        ConvertError::Malformed {
            part: Some("rtf"),
            detail: error.to_string(),
        }
    })?;
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    let mut pieces: Vec<md::Piece> = Vec::new();
    let mut current_layout: Option<String> = None;
    for block in &document.body {
        // Paragraph boundaries: consecutive runs that share the parsed
        // paragraph layout belong to the same RTF paragraph.
        let layout = format!("{:?}", block.paragraph);
        if current_layout.as_deref() != Some(layout.as_str()) {
            flush(&mut pieces, &mut blocks);
            current_layout = Some(layout);
        }
        pieces.push(md::Piece {
            text: block.text.clone(),
            bold: block.painter.bold,
            italic: block.painter.italic,
            link: None,
        });
    }
    flush(&mut pieces, &mut blocks);
    Ok(md::render(&blocks))
}

fn flush(pieces: &mut Vec<md::Piece>, blocks: &mut Vec<md::MdBlock>) {
    let text = md::style_pieces(std::mem::take(pieces));
    let text = text.trim();
    if !text.is_empty() {
        blocks.push(md::MdBlock::Para {
            lines: vec![text.to_string()],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_and_runs() {
        let rtf = r#"{\rtf1\ansi{\fonttbl\f0\fswiss Helvetica;}\f0\pard Voici du texte en {\b gras}.\par}"#;
        let markdown = to_markdown(rtf.as_bytes()).unwrap();
        assert!(markdown.contains("Voici du texte en"), "{markdown}");
        assert!(markdown.contains("**gras**"), "{markdown}");
    }

    #[test]
    fn groups_runs_into_paragraphs_by_layout() {
        // Distinct paragraph layout (right-aligned second paragraph) marks a
        // boundary; identical-layout runs in one paragraph merge.
        let rtf = concat!(
            r"{\rtf1\ansi\pard First paragraph.\par",
            r"\pard\qr Second paragraph.\par}"
        );
        let markdown = to_markdown(rtf.as_bytes()).unwrap();
        assert!(markdown.contains("First paragraph."), "{markdown}");
        assert!(markdown.contains("Second paragraph."), "{markdown}");
        assert!(markdown.contains("\n\n"), "{markdown}");
    }

    #[test]
    fn rejects_non_rtf_input() {
        assert!(matches!(
            to_markdown(b"not an rtf document at all"),
            Err(ConvertError::Malformed {
                part: Some("rtf"),
                ..
            })
        ));
    }
}
