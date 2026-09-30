//! CSV → GitHub-Flavored Markdown table.

use std::io::Cursor;

use crate::{ConvertError, md};

/// Converts CSV records to a single GFM table; the first record is the header.
/// Quoted fields (including embedded newlines and `|`) are handled by the
/// `csv` reader.
pub(crate) fn to_markdown(bytes: &[u8]) -> Result<String, ConvertError> {
    // A UTF-8 BOM would otherwise land in the first header cell.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(Cursor::new(bytes));
    let mut rows: Vec<Vec<String>> = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|error| ConvertError::Malformed {
            part: Some("csv"),
            detail: error.to_string(),
        })?;
        rows.push(record.iter().map(str::to_string).collect());
    }
    if rows.is_empty() {
        return Ok(String::new());
    }
    Ok(md::render(&[md::MdBlock::Table { rows }]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_records_and_escapes_pipes() {
        let markdown = to_markdown(b"id,city\n1,Reykjavik\n2,Tromso\n").unwrap();
        assert!(markdown.contains("| id | city |"), "{markdown}");
        assert!(markdown.contains("| --- | --- |"), "{markdown}");
        assert!(markdown.contains("| 1 | Reykjavik |"), "{markdown}");
        assert!(markdown.contains("| 2 | Tromso |"), "{markdown}");
        let piped = to_markdown(b"a,b\n1,2|3\n").unwrap();
        assert!(piped.contains("2\\|3"), "{piped}");
    }

    #[test]
    fn pads_uneven_rows() {
        let markdown = to_markdown(b"a,b\njust-one\n").unwrap();
        assert!(markdown.contains("| just-one |  |"), "{markdown}");
    }

    #[test]
    fn strips_utf8_bom() {
        let markdown = to_markdown(&b"\xEF\xBB\xBFh1,h2[..]\n".to_vec()[..]).unwrap();
        assert!(markdown.contains("h1"), "{markdown}");
    }
}
