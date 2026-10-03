//! Docx (Office Open XML WordprocessingML) → GitHub-Flavored Markdown,
//! walked directly over `word/document.xml`.

use std::collections::HashMap;

use crate::{ConvertError, md, xml};

/// External relationship targets (hyperlinks), by relationship id.
type Rels = HashMap<String, String>;

/// Where block emission lands: document blocks, or the lines of one table cell.
enum Sink<'a> {
    Blocks(&'a mut Vec<md::MdBlock>),
    CellLines(&'a mut Vec<String>),
}

pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let mut archive = xml::open_zip(bytes)?;
    let rels = read_rels(&mut archive);
    let document =
        xml::member(&mut archive, "word/document.xml").ok_or_else(|| ConvertError::Malformed {
            part: Some("docx"),
            detail: "missing word/document.xml".to_string(),
        })?;
    let root = xml::parse_xml(&document)?;
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    collect_blocks(&root, &rels, &mut Sink::Blocks(&mut blocks));
    Ok(md::render(&blocks))
}

/// Reads external relationship targets from `word/_rels/document.xml.rels`.
fn read_rels<R>(archive: &mut zip::ZipArchive<R>) -> Rels
where
    R: std::io::Read + std::io::Seek,
{
    let mut rels = Rels::new();
    let Some(bytes) = xml::member(archive, "word/_rels/document.xml.rels") else {
        return rels;
    };
    let Ok(root) = xml::parse_xml(&bytes) else {
        return rels;
    };
    for rel in root.find_all("Relationship") {
        let external = rel
            .attr("TargetMode")
            .is_some_and(|mode| mode == "External");
        if external && let (Some(id), Some(target)) = (rel.attr("Id"), rel.attr("Target")) {
            rels.insert(id.to_string(), target.to_string());
        }
    }
    rels
}

/// Walks a container's children, emitting paragraphs and tables. Unknown
/// containers are recursed into (sdt content controls, the body wrapper);
/// property-only elements are skipped.
fn collect_blocks(container: &xml::Elem, rels: &Rels, sink: &mut Sink<'_>) {
    for child in container.child_elems() {
        match child.local() {
            "p" => emit_paragraph(child, rels, sink),
            "tbl" => emit_table(child, rels, sink),
            "sectPr" | "sdtPr" => {}
            _ => collect_blocks(child, rels, sink),
        }
    }
}

/// Emits one `w:p` paragraph, mapping its `pStyle`/`numPr` properties to
/// headings, bullets, quotes, and plain paragraphs.
fn emit_paragraph(paragraph: &xml::Elem, rels: &Rels, sink: &mut Sink<'_>) {
    let pieces = collect_inline(paragraph, rels, false, false, None);
    let text = md::style_pieces(pieces);
    if text.trim().is_empty() {
        return;
    }
    let properties = paragraph.first("pPr");
    let style = properties
        .and_then(|properties| properties.first("pStyle"))
        .and_then(|style| style.attr("val"))
        .map(normalize_style);
    let numbered = properties.is_some_and(|properties| properties.has("numPr"));
    let depth = properties
        .and_then(|properties| properties.find("ilvl"))
        .and_then(|level| level.attr("val"))
        .and_then(|level| level.parse::<usize>().ok())
        .unwrap_or(0)
        .min(4);
    match style.as_deref() {
        Some("title") => sink.push(md::MdBlock::Heading {
            level: 1,
            text: collapse_newlines(&text),
        }),
        Some("heading1" | "heading2" | "heading3" | "heading4" | "heading5" | "heading6") => {
            let level = style
                .as_deref()
                .and_then(|style| style.strip_prefix("heading"))
                .and_then(|level| level.parse::<usize>().ok())
                .unwrap_or(1);
            sink.push(md::MdBlock::Heading {
                level,
                text: collapse_newlines(&text),
            });
        }
        Some("quote") => sink.push(md::MdBlock::Quote {
            lines: text.split('\n').map(str::to_string).collect(),
        }),
        _ if numbered => sink.push(md::MdBlock::Bullet {
            indent: depth,
            text: collapse_newlines(&text),
        }),
        _ => sink.push(md::MdBlock::Para {
            lines: text.split('\n').map(str::to_string).collect(),
        }),
    }
}

/// Normalizes a paragraph style id: `Heading_20_1` (LibreOffice), `Heading 1`,
/// and `Heading1` all compare to `heading1`.
fn normalize_style(style: &str) -> String {
    style
        .to_lowercase()
        .replace("_20_", "")
        .replace([' ', '_'], "")
}

fn collapse_newlines(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Sink<'_> {
    fn push(&mut self, block: md::MdBlock) {
        match self {
            Sink::Blocks(blocks) => blocks.push(block),
            Sink::CellLines(lines) => match block {
                md::MdBlock::Heading { text, .. } => lines.push(text),
                md::MdBlock::Para { lines: para_lines } => lines.extend(para_lines),
                md::MdBlock::Bullet { indent, text } => {
                    lines.push(format!("{}- {}", "  ".repeat(indent), text))
                }
                md::MdBlock::Quote { lines: quoted } => lines.extend(quoted),
                md::MdBlock::Table { .. } => lines.push("(nested table)".to_string()),
                md::MdBlock::Rule => {}
                md::MdBlock::Verbatim(text) => lines.push(text),
            },
        }
    }
}

/// Walks inline content: runs (`w:r`) with their `w:rPr` styling, hyperlinks,
/// line/paragraph breaks, and image/object placeholders. Property-only and
/// metadata elements are skipped; unknown text-carrying elements are ignored
/// to avoid resurrecting deleted text (`w:instrText`, `w:delText`).
fn collect_inline(
    container: &xml::Elem,
    rels: &Rels,
    bold: bool,
    italic: bool,
    link: Option<String>,
) -> Vec<md::Piece> {
    let mut pieces: Vec<md::Piece> = Vec::new();
    for child in container.child_elems() {
        match child.local() {
            "r" => {
                let properties = child.first("rPr");
                let run_bold = match properties.and_then(|p| p.first("b")) {
                    Some(on) => on.attr_bool_off("val") != Some(false),
                    None => bold,
                };
                let run_italic = match properties.and_then(|p| p.first("i")) {
                    Some(on) => on.attr_bool_off("val") != Some(false),
                    None => italic,
                };
                pieces.extend(collect_inline(
                    child,
                    rels,
                    run_bold,
                    run_italic,
                    link.clone(),
                ));
            }
            "hyperlink" => {
                let url = child
                    .attr("id")
                    .and_then(|id| rels.get(id))
                    .filter(|target| {
                        target.starts_with("http://")
                            || target.starts_with("https://")
                            || target.starts_with("mailto:")
                    })
                    .cloned();
                pieces.extend(collect_inline(child, rels, bold, italic, url));
            }
            "t" => {
                let text: String = child.text_children().collect::<Vec<_>>().concat();
                pieces.push(md::Piece {
                    text,
                    bold,
                    italic,
                    link: link.clone(),
                });
            }
            "br" | "cr" => pieces.push(md::Piece::plain("\n")),
            "tab" => pieces.push(md::Piece::plain(" ")),
            "noBreakHyphen" => pieces.push(md::Piece::plain("-")),
            "drawing" | "pict" => pieces.push(md::Piece::plain("[image] ")),
            "object" | "oleObject" => pieces.push(md::Piece::plain("[embedded object] ")),
            "sdt" | "sdtContent" | "smartTag" | "fldSimple" => {
                pieces.extend(collect_inline(child, rels, bold, italic, link.clone()));
            }
            _ => {}
        }
    }
    pieces
}

/// Emits a `w:tbl` table; the first row (which carries `tblHeader`
/// conventionally) becomes the GFM header. Single-cell tables degrade to
/// paragraphs; layout tables of all-empty cells are dropped.
fn emit_table(table: &xml::Elem, rels: &Rels, sink: &mut Sink<'_>) {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in table.child_elems().filter(|child| child.local() == "tr") {
        let mut cells: Vec<String> = Vec::new();
        for cell in row.child_elems().filter(|child| child.local() == "tc") {
            let mut lines: Vec<String> = Vec::new();
            collect_blocks(cell, rels, &mut Sink::CellLines(&mut lines));
            cells.push(lines.join("\n"));
        }
        rows.push(cells);
    }
    rows.retain(|row| row.iter().any(|cell| !cell.trim().is_empty()));
    match rows.as_slice() {
        [] => {}
        [only] if only.len() == 1 => sink.push(md::MdBlock::Para {
            lines: only[0].split('\n').map(str::to_string).collect(),
        }),
        _ => sink.push(md::MdBlock::Table { rows }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_leading_markers_in_paragraphs() {
        let markdown = md::render(&[md::MdBlock::Para {
            lines: vec!["# not a heading".to_string()],
        }]);
        assert!(markdown.contains("\\# not a heading"), "{markdown}");
    }
}
