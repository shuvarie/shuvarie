//! Epub → GitHub-Flavored Markdown: container manifest → spine → XHTML
//! chapters converted with `html-to-markdown-rs` (the same engine webfetch
//! uses), so chapter tables and lists convert consistently.

use crate::{ConvertError, md, xml};

pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    let mut archive = xml::open_zip(bytes)?;
    let container =
        xml::member(&mut archive, "META-INF/container.xml").ok_or(ConvertError::Malformed {
            part: Some("epub"),
            detail: "missing META-INF/container.xml".to_string(),
        })?;
    let container = xml::parse_xml(&container)?;
    let opf_path = container
        .find_all("rootfile")
        .iter()
        .filter_map(|rootfile| rootfile.attr("full-path"))
        .map(str::to_string)
        .next()
        .ok_or(ConvertError::Malformed {
            part: Some("epub"),
            detail: "no rootfile in container".to_string(),
        })?;

    let opf = xml::member(&mut archive, &opf_path).ok_or(ConvertError::Malformed {
        part: Some("epub"),
        detail: format!("missing package document {opf_path}"),
    })?;
    let package = xml::parse_xml(&opf)?;
    let title = package
        .find("title")
        .map(|title| title.text_children().collect::<String>());
    let base = base_dir(&opf_path);
    let manifest: Vec<(String, String)> = package
        .find_all("item")
        .iter()
        .filter_map(|item| Some((item.attr("id")?.to_string(), item.attr("href")?.to_string())))
        .collect();
    let hrefs: Vec<String> = package
        .find("spine")
        .into_iter()
        .flat_map(|spine| spine.child_elems())
        .filter(|item| item.local() == "itemref")
        .filter_map(|itemref| itemref.attr("idref"))
        .filter_map(|idref| {
            manifest
                .iter()
                .find(|(id, _)| id == idref)
                .map(|(_, href)| href.clone())
        })
        .collect();

    let mut blocks: Vec<md::MdBlock> = Vec::new();
    if let Some(title) = title {
        blocks.push(md::MdBlock::Heading {
            level: 1,
            text: title,
        });
    }
    for href in &hrefs {
        let path = resolve_href(&base, href);
        let Some(document) = xml::member(&mut archive, &path) else {
            blocks.push(md::MdBlock::Para {
                lines: vec![format!("_(missing chapter {href})_")],
            });
            continue;
        };
        let html = String::from_utf8_lossy(&document);
        let markdown = convert_chapter(&html)
            .unwrap_or_else(|error| format!("_(chapter could not be converted: {error})_"));
        blocks.push(md::MdBlock::Verbatim(markdown));
    }
    Ok(md::render(&blocks))
}

fn convert_chapter(html: &str) -> Result<String, String> {
    let options = html_to_markdown_rs::ConversionOptions::builder()
        .bullets("-".to_string())
        .skip_images(true)
        .extract_metadata(false)
        .extract_images(false)
        .compact_tables(true)
        .include_document_structure(false)
        .output_format(html_to_markdown_rs::OutputFormat::Markdown)
        .build();
    html_to_markdown_rs::convert(html, options)
        .map(|output| output.content.unwrap_or_default())
        .map_err(|error| error.to_string())
}

/// Directory of the package document, for resolving relative chapter hrefs.
fn base_dir(path: &str) -> String {
    match path.rfind('/') {
        Some(index) => path[..=index].to_string(),
        None => String::new(),
    }
}

/// Resolves a spine href against the package directory, percent-decoding and
/// normalizing `.`/`..` segments over the joined path.
fn resolve_href(base: &str, href: &str) -> String {
    let href = percent_decode(href);
    if let Some(absolute) = href.strip_prefix('/') {
        return absolute.to_string();
    }
    let joined = format!("{base}{href}");
    let mut segments: Vec<&str> = Vec::new();
    for segment in joined.split('/') {
        match segment {
            "." | "" => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    segments.join("/")
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() + 1 && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_percent_escapes() {
        assert_eq!(percent_decode("foo%20bar%2Fbaz"), "foo bar/baz");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
        assert_eq!(percent_decode("half%2"), "half%2");
    }

    #[test]
    fn resolves_relative_and_absolute_hrefs() {
        assert_eq!(
            resolve_href("OEBPS/", "chapter1.xhtml"),
            "OEBPS/chapter1.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/sub/", "../chapter1.xhtml"),
            "OEBPS/chapter1.xhtml"
        );
        assert_eq!(resolve_href("", "a/b.xhtml"), "a/b.xhtml");
        assert_eq!(resolve_href("OEBPS/", "/root.xhtml"), "root.xhtml");
    }
}
