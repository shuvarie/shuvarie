use pulldown_cmark::Alignment;
use ratatui::prelude::Stylize;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

/// Border drawing style for table rules and cell edges.
const BORDER: Style = Style::new().fg(theme::TEXT_MUTED);

/// Render a GFM table with box-drawing borders: one shared width per column
/// (the widest cell plus one padding space per side) and the delimiter row's
/// horizontal alignment applied inside each cell.
pub fn render(alignments: &[Alignment], rows: &[Vec<Vec<Span<'static>>>]) -> Vec<Line<'static>> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![2usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell_width(cell) + 2);
        }
    }
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(edge('┌', '┬', '┐', &widths));
    for (i, row) in rows.iter().enumerate() {
        lines.push(row_line(row, &widths, alignments, i == 0));
        if i == 0 && rows.len() > 1 {
            lines.push(edge('├', '┼', '┤', &widths));
        }
    }
    lines.push(edge('└', '┴', '┘', &widths));
    lines
}

fn row_line(
    row: &[Vec<Span<'static>>],
    widths: &[usize],
    alignments: &[Alignment],
    header: bool,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::raw("│").style(BORDER));
    for (i, &w) in widths.iter().enumerate() {
        let cell = row.get(i).map(Vec::as_slice).unwrap_or_default();
        let pad = w.saturating_sub(cell_width(cell) + 2);
        let (left, right) = match alignments.get(i).copied().unwrap_or(Alignment::None) {
            Alignment::Right => (1 + pad, 1),
            Alignment::Center => (1 + pad / 2, 1 + pad - pad / 2),
            _ => (1, 1 + pad),
        };
        if left > 0 {
            spans.push(Span::raw(" ".repeat(left)));
        }
        for span in cell {
            spans.push(if header {
                span.clone().bold()
            } else {
                span.clone()
            });
        }
        if right > 0 {
            spans.push(Span::raw(" ".repeat(right)));
        }
        spans.push(Span::raw("│").style(BORDER));
    }
    Line::from(spans)
}

fn edge(left: char, mid: char, right: char, widths: &[usize]) -> Line<'static> {
    let mut text = String::new();
    text.push(left);
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            text.push(mid);
        }
        text.extend(core::iter::repeat_n('─', w));
    }
    text.push(right);
    Line::from(Span::raw(text).style(BORDER))
}

fn cell_width(cell: &[Span<'static>]) -> usize {
    cell.iter().map(|s| s.content.width()).sum()
}
