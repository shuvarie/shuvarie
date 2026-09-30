//! Pptx (Office Open XML PresentationML) → GitHub-Flavored Markdown, walked
//! directly over `ppt/slides/slideN.xml` members in numeric order.

use crate::{ConvertError, md, xml};

pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let mut archive = xml::open_zip(bytes)?;
    let mut slides: Vec<(u32, String)> = archive.file_names().filter_map(slide_member).collect();
    slides.sort_by_key(|(number, _)| *number);
    let total = slides.len();
    if total == 0 {
        return Err(ConvertError::Malformed {
            part: Some("pptx"),
            detail: "no ppt/slides/slideN.xml members".to_string(),
        });
    }
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    for (index, (_, name)) in slides.into_iter().enumerate() {
        let contents = xml::member(&mut archive, &name).ok_or_else(|| ConvertError::Malformed {
            part: Some("pptx"),
            detail: format!("unreadable slide member {name}"),
        })?;
        let root = xml::parse_xml(&contents)?;
        collect_slide(&root, &mut blocks);
        if index + 1 < total {
            blocks.push(md::MdBlock::Rule);
        }
    }
    Ok(md::render(&blocks))
}

/// Recognizes `ppt/slides/slideN.xml` members.
fn slide_member(name: &str) -> Option<(u32, String)> {
    let stem = name
        .strip_prefix("ppt/slides/")?
        .strip_prefix("slide")?
        .strip_suffix(".xml")?;
    let number = stem.parse::<u32>().ok()?;
    Some((number, name.to_string()))
}

/// Walks a slide tree collecting shapes, grouped shapes, and tables.
fn collect_slide(elem: &xml::Elem, blocks: &mut Vec<md::MdBlock>) {
    for child in elem.child_elems() {
        match child.local() {
            "sp" => emit_shape(child, blocks),
            "pic" => blocks.push(md::MdBlock::Para {
                lines: vec!["[image]".to_string()],
            }),
            "graphicFrame" => match child.find("tbl") {
                Some(table) => blocks.push(pptx_table(table)),
                None => collect_slide(child, blocks),
            },
            "grpSp" => collect_slide(child, blocks),
            _ => collect_slide(child, blocks),
        }
    }
}

/// Emits one `p:sp` shape: title placeholders become `##` headings, bullet
/// paragraphs become list items, everything else plain paragraphs.
fn emit_shape(shape: &xml::Elem, blocks: &mut Vec<md::MdBlock>) {
    let shape_start = blocks.len();
    let Some(body) = shape.find("txBody") else {
        return;
    };
    let placeholder = shape.find("ph");
    let class = placeholder.and_then(|p| p.attr("type")).unwrap_or_default();
    let title = matches!(class, "title" | "ctrTitle");
    // A placeholder with no type attribute is a content (body) placeholder;
    // its paragraphs are bullets unless the run properties opt out.
    let body_placeholder =
        placeholder.is_some_and(|ph| matches!(ph.attr("type"), None | Some("body")));
    let mut title_parts: Vec<String> = Vec::new();
    for paragraph in body.child_elems().filter(|child| child.local() == "p") {
        let properties = paragraph.first("pPr");
        let bullet = if title {
            false
        } else if body_placeholder {
            !properties.is_some_and(|properties| properties.has("buNone"))
        } else {
            properties
                .is_some_and(|properties| properties.has("buChar") || properties.has("buAutoNum"))
        };
        let depth = properties
            .and_then(|properties| properties.attr("lvl"))
            .and_then(|level| level.parse::<usize>().ok())
            .unwrap_or(0)
            .min(4);
        let text = md::style_pieces(inline(paragraph));
        if text.trim().is_empty() {
            continue;
        }
        if bullet {
            blocks.push(md::MdBlock::Bullet {
                indent: depth,
                text,
            });
        } else if title {
            title_parts.push(text);
        } else {
            blocks.push(md::MdBlock::Para { lines: vec![text] });
        }
    }
    // The shape's title heading leads everything the shape emitted.
    if title && !title_parts.is_empty() {
        blocks.insert(
            shape_start,
            md::MdBlock::Heading {
                level: 2,
                text: title_parts.join(" "),
            },
        );
    }
}

/// Collects DrawingML inline text (`a:t` runs, `a:br` breaks).
fn inline(container: &xml::Elem) -> Vec<md::Piece> {
    let mut pieces: Vec<md::Piece> = Vec::new();
    for child in container.child_elems() {
        match child.local() {
            "t" => {
                let text: String = child.text_children().collect::<Vec<_>>().concat();
                pieces.push(md::Piece::plain(text));
            }
            "br" => pieces.push(md::Piece::plain("\n")),
            "r" | "fld" | "smartTag" => pieces.extend(inline(child)),
            _ => pieces.extend(inline(child)),
        }
    }
    pieces
}

fn pptx_table(table: &xml::Elem) -> md::MdBlock {
    let mut rows: Vec<Vec<String>> = Vec::new();
    for row in table.child_elems().filter(|child| child.local() == "tr") {
        let cells: Vec<String> = row
            .child_elems()
            .filter(|child| child.local() == "tc")
            .map(|cell| {
                let texts: Vec<String> = cell.text_children().map(str::to_string).collect();
                texts.join(" ").trim().to_string()
            })
            .collect();
        rows.push(cells);
    }
    md::MdBlock::Table { rows }
}
