use pulldown_cmark::Alignment;
use ratatui::prelude::Stylize;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme;

/// Border drawing style for table rules and cell edges.
const BORDER: Style = Style::new().fg(theme::TEXT_MUTED);

/// Longest cell content before it wraps inside the box (the padding spaces
/// excluded): column widths are capped at this plus one padding space per
/// side, so a pathological cell cannot stretch the table past the pane, where
/// the pane's own row wrap would shear the borders apart.
const MAX_CELL_WIDTH: usize = 40;

/// Render a GFM table with box-drawing borders: one shared width per column
/// (the widest cell plus one padding space per side, capped at
/// `MAX_CELL_WIDTH + 2`) and the delimiter row's horizontal alignment applied
/// inside each cell. Cells longer than the cap word-wrap onto further
/// physical rows inside the box, borders and all.
pub fn render(alignments: &[Alignment], rows: &[Vec<Vec<Span<'static>>>]) -> Vec<Line<'static>> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    let mut widths = vec![2usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            let natural = (cell_width(cell) + 2).min(MAX_CELL_WIDTH + 2);
            widths[i] = widths[i].max(natural);
        }
    }
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(edge('┌', '┬', '┐', &widths));
    for (i, row) in rows.iter().enumerate() {
        lines.extend(row_lines(row, &widths, alignments, i == 0));
        if i == 0 && rows.len() > 1 {
            lines.push(edge('├', '┼', '┤', &widths));
        }
    }
    lines.push(edge('└', '┴', '┘', &widths));
    lines
}

/// The physical lines of one logical row: each column's cell wrapped to its
/// column's content width, shorter columns riding along with empty
/// continuation cells, the delimiter row's alignment applied to every
/// physical row.
fn row_lines(
    row: &[Vec<Span<'static>>],
    widths: &[usize],
    alignments: &[Alignment],
    header: bool,
) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Vec<Span<'static>>>> = widths
        .iter()
        .enumerate()
        .map(|(i, &w)| {
            wrap_cell(
                row.get(i).map(Vec::as_slice).unwrap_or_default(),
                w.saturating_sub(2).max(1),
            )
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut lines = Vec::with_capacity(height);
    for r in 0..height {
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw("│").style(BORDER));
        for (i, &w) in widths.iter().enumerate() {
            let cell = wrapped[i].get(r).map(Vec::as_slice).unwrap_or_default();
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
        lines.push(Line::from(spans));
    }
    lines
}

/// Word-wrap one cell into physical rows of at most `width` display columns:
/// breaks ride the space/tab runs between words (a break consumes its
/// separator, so a row never starts with a space the pane's
/// `Wrap { trim: true }` paint would strip) and a token wider than the whole
/// column is hard-split at character widths.
pub(crate) fn wrap_cell(cell: &[Span<'static>], width: usize) -> Vec<Vec<Span<'static>>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    for (text, style, ws) in tokens(cell) {
        let w = text.width();
        if ws {
            if cur_w + w <= width {
                cur_w += w;
                cur.push(Span::styled(text, style));
            } else if cur_w > 0 {
                trim_ws(&mut cur, &mut cur_w);
                if !cur.is_empty() {
                    rows.push(core::mem::take(&mut cur));
                }
                cur_w = 0;
            }
            continue;
        }
        if cur_w + w <= width {
            cur_w += w;
            cur.push(Span::styled(text, style));
            continue;
        }
        if cur_w > 0 {
            trim_ws(&mut cur, &mut cur_w);
            if !cur.is_empty() {
                rows.push(core::mem::take(&mut cur));
            }
            cur_w = 0;
        }
        let mut pieces = split_width(&text, style, width);
        if let Some(last) = pieces.pop() {
            for piece in pieces {
                rows.push(vec![piece]);
            }
            cur_w = last.content.width();
            cur = vec![last];
        }
    }
    if !cur.is_empty() || rows.is_empty() {
        rows.push(cur);
    }
    rows
}

fn trim_ws(cur: &mut Vec<Span<'static>>, cur_w: &mut usize) {
    while let Some(last) = cur.last() {
        if !last.content.chars().all(is_space) {
            break;
        }
        *cur_w -= last.content.width();
        cur.pop();
    }
}

/// A cell's spans split into `(text, style, whitespace)` runs: space/tab runs
/// are the break opportunities, everything else is a word.
fn tokens(cell: &[Span<'static>]) -> Vec<(String, Style, bool)> {
    let mut out = Vec::new();
    for span in cell {
        let mut chunk = String::new();
        let mut ws = false;
        for ch in span.content.chars() {
            let space = is_space(ch);
            if space != ws && !chunk.is_empty() {
                out.push((core::mem::take(&mut chunk), span.style, ws));
            }
            ws = space;
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            out.push((chunk, span.style, ws));
        }
    }
    out
}

/// Split one over-long token at character widths: a zero-width char (combining
/// mark) rides with the piece before it, and a char wider than the whole
/// column forms its own piece so the split always makes progress.
fn split_width(text: &str, style: Style, width: usize) -> Vec<Span<'static>> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut piece_w = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if piece_w + cw > width && !piece.is_empty() {
            pieces.push(Span::styled(core::mem::take(&mut piece), style));
            piece_w = 0;
        }
        piece.push(ch);
        piece_w += cw;
    }
    if !piece.is_empty() {
        pieces.push(Span::styled(piece, style));
    }
    pieces
}

fn is_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t')
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
