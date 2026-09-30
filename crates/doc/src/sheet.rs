//! Spreadsheets (xlsx/xlsm/xlsb/xls/ods) → GitHub-Flavored Markdown tables,
//! on top of `calamine`'s concrete readers, dispatched by the detected
//! [`SourceFormat`].

use std::io::Cursor;

use calamine::{Data, DataType as _, Ods, Reader, Xls, Xlsb, Xlsx, open_workbook_from_rs};

use crate::{ConvertError, SourceFormat, md};

/// Per-sheet cap on converted rows; the tool paginates, but pathological
/// sheets should not materialize unbounded markdown.
const MAX_SHEET_ROWS: usize = 5000;

pub(crate) fn to_markdown(format: SourceFormat, bytes: &[u8]) -> Result<String, ConvertError> {
    let part = format.part();
    let malformed = |error: &dyn std::fmt::Display| ConvertError::Malformed {
        part: Some(part),
        detail: error.to_string(),
    };
    let cursor = Cursor::new(bytes);
    match format {
        SourceFormat::Xlsx | SourceFormat::Xlsm => {
            let workbook: Xlsx<_> =
                open_workbook_from_rs(cursor).map_err(|error| malformed(&error))?;
            sheets_markdown(workbook)
        }
        SourceFormat::Xlsb => {
            let workbook: Xlsb<_> =
                open_workbook_from_rs(cursor).map_err(|error| malformed(&error))?;
            sheets_markdown(workbook)
        }
        SourceFormat::Xls => {
            let workbook: Xls<_> =
                open_workbook_from_rs(cursor).map_err(|error| malformed(&error))?;
            sheets_markdown(workbook)
        }
        SourceFormat::Ods => {
            let workbook: Ods<_> =
                open_workbook_from_rs(cursor).map_err(|error| malformed(&error))?;
            sheets_markdown(workbook)
        }
        _ => Err(ConvertError::Malformed {
            part: Some(part),
            detail: "not a spreadsheet".to_string(),
        }),
    }
}

/// Emits one GFM table per non-empty worksheet, headed `## <sheet name>`.
fn sheets_markdown<RS, W>(mut workbook: W) -> Result<String, ConvertError>
where
    RS: std::io::Read + std::io::Seek,
    W: Reader<RS>,
    W::Error: std::fmt::Display,
{
    let mut blocks: Vec<md::MdBlock> = Vec::new();
    let mut readable = false;
    for name in workbook.sheet_names() {
        match workbook.worksheet_range(&name) {
            Ok(range) if !range_is_empty(&range) => {
                blocks.push(md::MdBlock::Heading {
                    level: 2,
                    text: name.clone(),
                });
                blocks.push(md::MdBlock::Table {
                    rows: range_rows(&range),
                });
                readable = true;
            }
            Err(error) => blocks.push(md::MdBlock::Para {
                lines: vec![format!("_(worksheet '{name}' could not be read: {error})_")],
            }),
            Ok(_) => {}
        }
    }
    if !readable {
        return Ok("(workbook contains no non-empty worksheets)".to_string());
    }
    Ok(md::render(&blocks))
}

/// Trailing all-empty rows are trimmed; a hard row cap is applied.
fn range_rows(range: &calamine::Range<Data>) -> Vec<Vec<String>> {
    let rows: Vec<Vec<String>> = range
        .rows()
        .map(|row| row.iter().map(cell_text).collect())
        .collect();
    let rows: &[Vec<String>] = match rows
        .iter()
        .rposition(|row| row.iter().any(|cell| !cell.is_empty()))
    {
        Some(index) => &rows[..=index],
        None => &[],
    };
    if rows.len() > MAX_SHEET_ROWS {
        let mut rows: Vec<Vec<String>> = rows[..MAX_SHEET_ROWS].to_vec();
        rows.push(vec![format!(
            "_({MAX_SHEET_ROWS} rows shown; sheet truncated)_"
        )]);
        rows
    } else {
        rows.to_vec()
    }
}

fn range_is_empty(range: &calamine::Range<Data>) -> bool {
    range
        .rows()
        .all(|row| row.iter().all(|cell| cell.is_empty()))
}

fn cell_text(value: &Data) -> String {
    match value {
        Data::Empty => String::new(),
        Data::Bool(true) => "TRUE".to_string(),
        Data::Bool(false) => "FALSE".to_string(),
        Data::String(text) => text.clone(),
        Data::Int(number) => number.to_string(),
        Data::Float(number) => format_float(*number),
        Data::Error(error) => error.to_string(),
        other => format!("{other:?}"),
    }
}

fn format_float(number: f64) -> String {
    if number.is_nan() || number.is_infinite() {
        return format!("{number}");
    }
    if number == number.trunc() && number.abs() < 1e15 {
        return format!("{}", number.trunc() as i64);
    }
    format!("{number}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_numbers_like_calculator_output() {
        assert_eq!(format_float(1200.0), "1200");
        assert_eq!(format_float(3.25), "3.25");
        assert_eq!(format_float(-7.0), "-7");
    }

    #[test]
    fn renders_headers_and_cells() {
        let rows = vec![
            vec!["Product".to_string(), "Sales".to_string()],
            vec!["Anvil".to_string(), "1200".to_string()],
        ];
        let table = md::table(&rows).unwrap();
        assert!(table.contains("| Product | Sales |"), "{table}");
        assert!(table.contains("| --- | --- |"), "{table}");
        assert!(table.contains("| Anvil | 1200 |"), "{table}");
    }
}
