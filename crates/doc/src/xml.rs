//! Shared helpers for the zip-container and XML-backed formats (docx, pptx,
//! odt, odp, epub): a minimal XML element tree and zip member access.

use std::io::{Cursor, Read};

use crate::ConvertError;
use quick_xml::events::Event;

/// A conservative cap on a single decompressed XML member (text parts only;
/// embedded media members are never read).
const MAX_MEMBER_BYTES: u64 = 128 * 1024 * 1024;

/// Opens `bytes` as a zip archive for member sniffing and reading.
pub(crate) fn open_zip(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, ConvertError> {
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|error| ConvertError::Malformed {
        part: Some("zip"),
        detail: error.to_string(),
    })
}

/// Reads one decompressed member, returning `None` when absent or empty.
pub(crate) fn member<R>(archive: &mut zip::ZipArchive<R>, name: &str) -> Option<Vec<u8>>
where
    R: std::io::Read + std::io::Seek,
{
    let mut file = archive.by_name(name).ok()?;
    if !file.is_file() {
        return None;
    }
    if file.size() > MAX_MEMBER_BYTES {
        return None;
    }
    let mut contents = Vec::with_capacity(file.size().min(MAX_MEMBER_BYTES) as usize);
    file.read_to_end(&mut contents).ok()?;
    (!contents.is_empty()).then_some(contents)
}

/// A minimal parsed XML element: qualified tag name, namespace-agnostic
/// attribute access, string children. Namespace prefixes are kept as written
/// (`w:p`) but compared via [`Elem::local`], which strips the prefix.
#[derive(Debug)]
pub(crate) struct Elem {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug)]
pub(crate) enum Node {
    Text(String),
    Elem(Elem),
}

impl Elem {
    pub(crate) fn local(&self) -> &str {
        self.name.rsplit(':').next().unwrap_or(&self.name)
    }

    /// The last attribute whose name matches `local` (namespace-agnostic);
    /// later duplicate attributes override earlier ones per XML.
    pub(crate) fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .rev()
            .find(|(name, _)| name.rsplit(':').next().is_some_and(|key| key == local))
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn attr_bool_off(&self, local: &str) -> Option<bool> {
        self.attr(local).map(|value| {
            !matches!(
                value.to_ascii_lowercase().as_str(),
                "false" | "0" | "none" | "off"
            )
        })
    }

    pub(crate) fn child_elems(&self) -> impl Iterator<Item = &Elem> {
        self.children.iter().filter_map(Node::as_elem)
    }

    pub(crate) fn text_children(&self) -> impl Iterator<Item = &str> {
        self.children.iter().filter_map(Node::as_text)
    }

    /// Raw child nodes, for walkers that treat all character data as content.
    pub(crate) fn nodes(&self) -> &[Node] {
        &self.children
    }

    pub(crate) fn first(&self, local: &str) -> Option<&Elem> {
        self.child_elems().find(|child| child.local() == local)
    }

    /// Depth-first search for the first element with the given local name.
    pub(crate) fn find(&self, local: &str) -> Option<&Elem> {
        for child in self.child_elems() {
            if child.local() == local {
                return Some(child);
            }
            if let Some(found) = child.find(local) {
                return Some(found);
            }
        }
        None
    }

    pub(crate) fn has(&self, local: &str) -> bool {
        self.find(local).is_some()
    }

    /// Depth-first search for all elements with the given local name,
    /// parent-first.
    pub(crate) fn find_all<'elem>(&'elem self, local: &str) -> Vec<&'elem Elem> {
        let mut found = Vec::new();
        self.find_all_into(local, &mut found);
        found
    }

    fn find_all_into<'elem>(&'elem self, local: &str, found: &mut Vec<&'elem Elem>) {
        for child in self.child_elems() {
            if child.local() == local {
                found.push(child);
            }
            child.find_all_into(local, found);
        }
    }
}

impl Node {
    fn as_elem(&self) -> Option<&Elem> {
        match self {
            Node::Elem(elem) => Some(elem),
            Node::Text(_) => None,
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Node::Text(text) => Some(text),
            Node::Elem(_) => None,
        }
    }
}

/// Parses XML into a single-root element tree. Text is preserved verbatim
/// (whitespace included) so office XML runs keep their meaningful leading and
/// trailing spaces; walkers collect text only where it is semantically part
/// of the document. Nesting deeper than 256 levels is rejected before
/// recursion-based walkers could overflow the stack on hostile input.
pub(crate) fn parse_xml(bytes: &[u8]) -> Result<Elem, ConvertError> {
    const MAX_XML_DEPTH: usize = 256;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut stack: Vec<Elem> = Vec::new();
    let mut roots: Vec<Elem> = Vec::new();
    let mut event_buffer = Vec::new();
    loop {
        event_buffer.clear();
        let event =
            reader
                .read_event_into(&mut event_buffer)
                .map_err(|error| ConvertError::Malformed {
                    part: Some("xml"),
                    detail: error.to_string(),
                })?;
        match event {
            Event::Eof => break,
            Event::Start(start) => {
                if stack.len() >= MAX_XML_DEPTH {
                    return Err(ConvertError::Malformed {
                        part: Some("xml"),
                        detail: "document too deeply nested".to_string(),
                    });
                }
                stack.push(elem_from(&start));
            }
            Event::Empty(start) => attach_elem(&mut roots, &mut stack, elem_from(&start)),
            Event::End(_) => {
                let Some(elem) = stack.pop() else {
                    return Err(malformed("unbalanced XML end tag"));
                };
                attach_elem(&mut roots, &mut stack, elem);
            }
            // Entity references arrive as their own events in quick-xml 0.42;
            // plain `Event::Text` therefore needs no unescaping.
            Event::Text(text) => attach_text(&mut stack, String::from(&*text)),
            Event::CData(data) => {
                attach_text(&mut stack, String::from(&*data));
            }
            Event::GeneralRef(entity) => {
                let resolved = entity_ref_text(&entity);
                if !resolved.is_empty() {
                    attach_text(&mut stack, resolved);
                }
            }
            _ => {}
        }
    }
    if let Some(elem) = stack.pop() {
        attach_elem(&mut roots, &mut stack, elem);
    }
    roots.pop().ok_or_else(|| malformed("no root element"))
}

/// Resolves an XML entity reference: character refs via the parser, the
/// five predefined named entities by hand, unknown entities as empty text.
fn entity_ref_text(entity: &quick_xml::events::BytesRef<'_>) -> String {
    if let Some(character) = entity.resolve_char_ref().ok().flatten() {
        return character.to_string();
    }
    match entity.as_ref() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => "",
    }
    .to_string()
}

fn malformed(detail: impl Into<String>) -> ConvertError {
    ConvertError::Malformed {
        part: None,
        detail: detail.into(),
    }
}

fn elem_from(start: &quick_xml::events::BytesStart<'_>) -> Elem {
    // Names deref to `str` in quick-xml 0.42; attribute values are still
    // escaped and unescape via `escape::unescape`.
    let name = start.name().0.to_string();
    let attrs = start
        .attributes()
        .filter_map(|attr| {
            let attr = attr.ok()?;
            let value = quick_xml::escape::unescape(&attr.value).ok()?.into_owned();
            Some((attr.key.0.to_string(), value))
        })
        .collect();
    Elem {
        name,
        attrs,
        children: Vec::new(),
    }
}

/// Attaches a completed element to its parent (or roots).
fn attach_elem(roots: &mut Vec<Elem>, stack: &mut [Elem], elem: Elem) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(Node::Elem(elem)),
        None => roots.push(elem),
    }
}

fn attach_text(stack: &mut [Elem], text: String) {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(Node::Text(text));
    }
}
