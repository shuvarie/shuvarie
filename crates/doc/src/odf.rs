//! OpenDocument formats (odt, odp) → GitHub-Flavored Markdown, walked
//! directly over `content.xml`.
//!
//! Character data inside `text:p`/`text:h` is document content by spec, so
//! unlike the OOXML walkers everything below a paragraph is collected;
//! tab/line-break/space helpers translate to markdown-safe equivalents and
//! `text:a` links are preserved.

use crate::{ConvertError, md, xml};

/// ODT: headings (`text:h`), lists, and tables out of `office:text`.
pub(crate) fn odt_to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let contents = content_xml(bytes, "odt")?;
    let root = xml::parse_xml(&contents)?;
    let body = root.find("body").ok_or(ConvertError::Malformed {
        part: Some("odt"),
        detail: "missing office:body".to_string(),
    })?;
    let text = body.find("text").ok_or(ConvertError::Malformed {
        part: Some("odt"),
        detail: "missing office:text body".to_string(),
    })?;
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    walk_text(text, &mut blocks);
    Ok(md::render(&blocks))
}

/// ODP: `draw:page` slides with title/outline/subtitle placeholder frames.
pub(crate) fn odp_to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let contents = content_xml(bytes, "odp")?;
    let root = xml::parse_xml(&contents)?;
    let body = root.find("body").ok_or(ConvertError::Malformed {
        part: Some("odp"),
        detail: "missing office:body".to_string(),
    })?;
    let presentation = body.find("presentation").ok_or(ConvertError::Malformed {
        part: Some("odp"),
        detail: "missing office:presentation body".to_string(),
    })?;
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    let pages: Vec<&xml::Elem> = presentation
        .child_elems()
        .filter(|child| child.local() == "page")
        .collect();
    for (index, page) in pages.iter().enumerate() {
        walk_page(page, &mut blocks);
        if index + 1 < pages.len() {
            blocks.push(md::MdBlock::Rule);
        }
    }
    Ok(md::render(&blocks))
}

/// Reads and decompresses `content.xml` from the archive.
fn content_xml(bytes: &[u8], part: &'static str) -> Result<Vec<u8>, ConvertError> {
    let mut archive = xml::open_zip(bytes)?;
    xml::member(&mut archive, "content.xml").ok_or(ConvertError::Malformed {
        part: Some(part),
        detail: "missing content.xml".to_string(),
    })
}

fn walk_text(text: &xml::Elem, blocks: &mut Vec<md::MdBlock>) {
    for child in text.child_elems() {
        match child.local() {
            "h" => {
                let level = child
                    .attr("outline-level")
                    .and_then(|level| level.parse::<usize>().ok())
                    .unwrap_or(1);
                blocks.push(md::MdBlock::Heading {
                    level,
                    text: paragraph_text(child),
                });
            }
            "p" => blocks.push(md::MdBlock::Para {
                lines: split_lines(paragraph_text(child)),
            }),
            "list" => walk_list(child, 0, blocks),
            "table" => blocks.push(table_block(child)),
            "section" if child.attr("display") != Some("none") => walk_text(child, blocks),
            "frame" => blocks.push(md::MdBlock::Para {
                lines: vec!["[image]".to_string()],
            }),
            _ => {}
        }
    }
}

fn walk_list(list: &xml::Elem, depth: usize, blocks: &mut Vec<md::MdBlock>) {
    for item in list
        .child_elems()
        .filter(|child| child.local() == "list-item")
    {
        for child in item.child_elems() {
            match child.local() {
                "p" | "h" => blocks.push(md::MdBlock::Bullet {
                    indent: depth,
                    text: paragraph_text(child),
                }),
                "list" => walk_list(child, depth + 1, blocks),
                _ => {}
            }
        }
    }
}

fn walk_page(page: &xml::Elem, blocks: &mut Vec<md::MdBlock>) {
    for frame in page.child_elems() {
        match frame.local() {
            "frame" => {
                let class = frame.attr("class");
                let Some(text_box) = frame.find("text-box") else {
                    if frame.has("image") {
                        blocks.push(md::MdBlock::Para {
                            lines: vec!["[image]".to_string()],
                        });
                    }
                    continue;
                };
                let items = text_box_items(text_box);
                if items.is_empty() {
                    continue;
                }
                match class {
                    Some("title") => {
                        let text: Vec<&str> = items
                            .iter()
                            .map(|(text, _)| text.as_str())
                            .filter(|text| !text.trim().is_empty())
                            .collect();
                        if !text.is_empty() {
                            blocks.push(md::MdBlock::Heading {
                                level: 2,
                                text: text.join(" "),
                            });
                        }
                    }
                    Some("notes") => {}
                    _ => {
                        for (text, list_depth) in items {
                            if let Some(depth) = list_depth {
                                blocks.push(md::MdBlock::Bullet {
                                    indent: depth,
                                    text,
                                });
                            } else {
                                blocks.push(md::MdBlock::Para { lines: vec![text] });
                            }
                        }
                    }
                }
            }
            "image" => blocks.push(md::MdBlock::Para {
                lines: vec!["[image]".to_string()],
            }),
            _ => {}
        }
    }
}

/// Items of a text-box: ODP paragraphs and (in outline placeholders) bullet
/// lists; returns each item with its list depth, if any.
fn text_box_items(text_box: &xml::Elem) -> Vec<(String, Option<usize>)> {
    let mut items: Vec<(String, Option<usize>)> = Vec::new();
    for child in text_box.child_elems() {
        match child.local() {
            "p" | "h" => items.push((paragraph_text(child), None)),
            "list" => push_list_items(child, 0, &mut items),
            _ => {}
        }
    }
    items
}

fn push_list_items(list: &xml::Elem, depth: usize, items: &mut Vec<(String, Option<usize>)>) {
    for item in list
        .child_elems()
        .filter(|child| child.local() == "list-item")
    {
        for child in item.child_elems() {
            match child.local() {
                "p" | "h" => items.push((paragraph_text(child), Some(depth))),
                "list" => push_list_items(child, depth + 1, items),
                _ => {}
            }
        }
    }
}

/// An ODF table; header-row wrappers contribute the leading rows and covered
/// cells (span shadows) render as empty cells.
fn table_block(table: &xml::Elem) -> md::MdBlock {
    let rows: Vec<Vec<String>> = table
        .child_elems()
        .filter(|child| matches!(child.local(), "table-row" | "table-header-rows"))
        .flat_map(|child| match child.local() {
            "table-row" => vec![row_cells(child)],
            _ => child
                .child_elems()
                .filter(|row| row.local() == "table-row")
                .map(row_cells)
                .collect(),
        })
        .collect();
    md::MdBlock::Table { rows }
}

fn row_cells(row: &xml::Elem) -> Vec<String> {
    row.child_elems()
        .filter(|child| matches!(child.local(), "table-cell" | "covered-table-cell"))
        .map(|cell| {
            let mut texts: Vec<String> = Vec::new();
            for child in cell.child_elems() {
                match child.local() {
                    "p" | "h" => texts.push(paragraph_text(child)),
                    "table" => texts.push("(nested table)".to_string()),
                    _ => {}
                }
            }
            texts.join(" ").trim().to_string()
        })
        .collect()
}

/// Collects all character data under a paragraph-level element, translating
/// ODF inline helpers and links.
fn paragraph_text(elem: &xml::Elem) -> String {
    let mut pieces: Vec<md::Piece> = Vec::new();
    collect(elem, None, &mut pieces);
    md::style_pieces(pieces)
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_lines(text: String) -> Vec<String> {
    text.split('\n').map(str::to_string).collect()
}

fn collect(elem: &xml::Elem, link: Option<String>, pieces: &mut Vec<md::Piece>) {
    for node in elem.nodes() {
        match node {
            xml::Node::Text(text) => pieces.push(md::Piece {
                text: text.clone(),
                bold: false,
                italic: false,
                link: link.clone(),
            }),
            xml::Node::Elem(child) => match child.local() {
                "tab" => pieces.push(md::Piece::plain(" ")),
                "line-break" => pieces.push(md::Piece::plain("\n")),
                "s" => {
                    let count = child
                        .attr("c")
                        .and_then(|count| count.parse::<usize>().ok())
                        .unwrap_or(1);
                    pieces.push(md::Piece::plain(" ".repeat(count.min(64))));
                }
                "a" => {
                    let url = child.attr("href").map(str::to_string);
                    collect(child, url, pieces);
                }
                "image" | "frame" => pieces.push(md::Piece::plain("[image] ")),
                "note" | "annotation" | "tracked-changes" => {}
                _ => collect(child, link.clone(), pieces),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paragraph(xml_bytes: &str) -> String {
        let root = xml::parse_xml(format!("<wrap>{xml_bytes}</wrap>").as_bytes()).unwrap();
        paragraph_text(root.find_all("p")[0])
    }

    #[test]
    fn collects_text_links_and_breaks() {
        let out = paragraph(
            "<text:p>Visit <text:a xlink:href=\"https://example.org\">the site</text:a> \
             for<text:line-break/>more,<text:tab/>please</text:p>",
        );
        assert_eq!(
            out,
            "Visit [the site](https://example.org) for\nmore, please"
        );
    }

    #[test]
    fn repeats_spaces() {
        // text:c is the exact number of spaces to insert (default 1).
        let out = paragraph("<text:p>a<text:s text:c=\"3\"/>b</text:p>");
        assert_eq!(out, "a   b");
        let default_one = paragraph("<text:p>a<text:s/>b</text:p>");
        assert_eq!(default_one, "a b");
    }

    #[test]
    fn drops_notes_and_annotations() {
        let out = paragraph(
            "<text:p>kept<office:annotation>dropped annotation</office:annotation>\
             <text:note>dropped note</text:note></text:p>",
        );
        assert_eq!(out, "kept");
    }
}
