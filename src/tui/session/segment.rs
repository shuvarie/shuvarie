use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};
use shuvarie_llm::{DiffLine, DiffLineKind};
use unicode_width::UnicodeWidthStr;

use crate::tui::theme;

/// Horizontal + vertical inset applied inside a block's background rect: the
/// bg fills the full rows while the text renders one padding row down and the
/// same amount inset on each side.
pub const BLOCK_PADDING: (u16, u16) = (2, 1);

/// Vertical-only inset for a bare text block: one blank row above and below
/// the content, no background, no side inset.
pub const TEXT_PADDING: (u16, u16) = (0, 1);

/// Source-row runs longer than this render as a sliced chunk whose rows
/// materialize only when painted; shorter runs stay fully materialized.
pub(crate) const CHUNK_ROWS: u32 = 48;

/// Address of a block inside the chat: the turn index and the block index
/// within that turn. Turn indices past the committed turns address the
/// in-flight turn.
#[derive(Debug, Clone)]
pub struct BlockAddr {
    pub turn: usize,
    pub block: usize,
}

/// Row range (within a turn) covered by a hit target, with the addressed
/// block. Recorded at render time; translated by the turn's content offset at
/// click time.
#[derive(Debug, Clone)]
pub struct HitRegion {
    pub start: u32,
    pub end: u32,
    pub addr: BlockAddr,
}

/// Rows of a `\n`-separated string addressable by index: byte offsets of the
/// row starts, so a row window materializes without touching the rest.
#[derive(Clone)]
pub struct TextRows {
    text: Rc<str>,
    offsets: Rc<[u32]>,
}

impl TextRows {
    pub fn new(text: impl Into<Rc<str>>) -> Self {
        let text: Rc<str> = text.into();
        let mut offsets = Vec::new();
        let mut start = 0u32;
        for (i, byte) in text.as_bytes().iter().enumerate() {
            if *byte == b'\n' {
                offsets.push(start);
                start = i as u32 + 1;
            }
        }
        if start < text.len() as u32 {
            offsets.push(start);
        }
        Self {
            text,
            offsets: offsets.into(),
        }
    }

    pub fn row_count(&self) -> u32 {
        self.offsets.len() as u32
    }

    /// Row text with its trailing `\r` dropped, mirroring `str::lines()`.
    fn row_str(&self, index: u32) -> &str {
        let start = self.offsets[index as usize] as usize;
        let end = self.offsets.get(index as usize + 1).map_or_else(
            || {
                if self.text.ends_with('\n') {
                    self.text.len() - 1
                } else {
                    self.text.len()
                }
            },
            |off| *off as usize - 1,
        );
        strip_row_break(&self.text[start..end])
    }
}

fn strip_row_break(row: &str) -> &str {
    row.strip_suffix('\r').unwrap_or(row)
}

/// A row-addressable projection of block content: how many source rows it has
/// and how one row renders. Styling happens at materialization time so only
/// painted rows build lines.
#[derive(Clone)]
pub enum BodySource {
    /// One styled span per row behind an optional fixed prefix (tool output
    /// rows, reasoning bodies).
    Text {
        rows: TextRows,
        prefix: &'static str,
        style: Style,
    },
    /// Numbered file-content rows (`    12 code`).
    Numbered { rows: TextRows },
    /// Diff rows: gutter numbers, a kind marker, and the row text.
    Diff { lines: Rc<[DiffLine]> },
    /// Pre-rendered styled lines (markdown output) shared through an `Rc`;
    /// a row materializes by cloning only the painted window's lines.
    Lines { lines: Rc<[Line<'static>]> },
}

impl BodySource {
    pub fn row_count(&self) -> u32 {
        match self {
            BodySource::Text { rows, .. } | BodySource::Numbered { rows } => rows.row_count(),
            BodySource::Diff { lines } => lines.len() as u32,
            BodySource::Lines { lines } => lines.len() as u32,
        }
    }

    /// The plain text of one rendered row: the concatenation of the styled
    /// line's span contents. Wrap counting runs against this text.
    pub(crate) fn row_text(&self, index: u32) -> String {
        match self {
            BodySource::Text { rows, prefix, .. } => {
                format!("{prefix}{}", rows.row_str(index))
            }
            BodySource::Numbered { rows } => {
                format!("    {:>4} {}", index + 1, rows.row_str(index))
            }
            BodySource::Diff { lines } => diff_row_text(&lines[index as usize]),
            BodySource::Lines { lines } => lines[index as usize]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect(),
        }
    }

    /// The copy text of one source row: the logical content without layout
    /// decoration — code without the number gutter, diffs without the
    /// gutter columns, everything else as shown.
    pub(crate) fn row_copy_text(&self, index: u32) -> String {
        match self {
            BodySource::Text { rows, prefix, .. } => {
                format!("{prefix}{}", rows.row_str(index))
            }
            BodySource::Numbered { rows } => rows.row_str(index).to_string(),
            BodySource::Diff { lines } => diff_row_copy_text(&lines[index as usize]),
            BodySource::Lines { lines } => lines[index as usize]
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect(),
        }
    }

    pub(crate) fn row(&self, index: u32) -> Line<'static> {
        match self {
            BodySource::Text {
                rows,
                prefix,
                style,
            } => Line::from(Span::styled(
                format!("{prefix}{}", rows.row_str(index)),
                *style,
            )),
            BodySource::Numbered { rows } => Line::from(vec![
                Span::raw(format!("    {:>4} ", index + 1)).fg(theme::TEXT_MUTED),
                Span::raw(rows.row_str(index).to_string()).fg(theme::TEXT_DIM),
            ]),
            BodySource::Diff { lines } => diff_row_line(&lines[index as usize]),
            BodySource::Lines { lines } => lines[index as usize].clone(),
        }
    }
}

fn diff_row_text(line: &DiffLine) -> String {
    if line.kind == DiffLineKind::Ellipsis {
        return "    …".to_string();
    }
    let marker = match line.kind {
        DiffLineKind::Add => "+",
        DiffLineKind::Remove => "-",
        _ => " ",
    };
    let old_num = gutter_number(line.old_line);
    let new_num = gutter_number(line.new_line);
    let text = line.text.trim_end_matches(['\r', '\n']);
    format!("  {old_num} {new_num} {marker}{text}")
}

fn diff_row_copy_text(line: &DiffLine) -> String {
    if line.kind == DiffLineKind::Ellipsis {
        return String::new();
    }
    let marker = match line.kind {
        DiffLineKind::Add => "+",
        DiffLineKind::Remove => "-",
        DiffLineKind::Context => " ",
        DiffLineKind::Ellipsis => unreachable!(),
    };
    let text = line.text.trim_end_matches(['\r', '\n']);
    format!("{marker}{text}")
}

fn diff_row_line(line: &DiffLine) -> Line<'static> {
    if line.kind == DiffLineKind::Ellipsis {
        return Line::from(Span::raw("    …").fg(theme::TEXT_MUTED));
    }
    let (marker, fg) = match line.kind {
        DiffLineKind::Add => ("+", theme::SUCCESS),
        DiffLineKind::Remove => ("-", theme::ERROR),
        DiffLineKind::Context => (" ", theme::TEXT_DIM),
        DiffLineKind::Ellipsis => unreachable!(),
    };
    let old_num = gutter_number(line.old_line);
    let new_num = gutter_number(line.new_line);
    let text = line.text.trim_end_matches(['\r', '\n']);
    Line::from(vec![
        Span::raw(format!("  {old_num} {new_num} ")).fg(theme::TEXT_MUTED),
        Span::raw(marker).fg(fg).bold(),
        Span::raw(text.to_string()).fg(fg),
    ])
}

fn gutter_number(value: Option<u64>) -> String {
    value
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_string())
}

/// One chunk of a segment body. Fixed chunks keep their lines resident (they
/// are small: headers, labels, collapsed previews); sliced chunks project
/// their rows from a shared source and materialize only the rows intersecting
/// the viewport at paint time.
#[derive(Clone)]
pub enum BodyChunk {
    Fixed(FixedChunk),
    Sliced(SlicedChunk),
}

#[derive(Clone)]
pub struct FixedChunk {
    pub lines: Vec<Line<'static>>,
    counts: RefCell<Option<(u16, Rc<[u32]>)>>,
}

#[derive(Clone)]
pub struct SlicedChunk {
    pub source: BodySource,
    /// Index of the chunk's first source row within the projection.
    pub start: u32,
    /// Wrapped-row count per source row at the build width; the build width
    /// always matches the width the segment is measured and painted at. The
    /// chunk covers `[start..start + counts.len())` — below the source's full
    /// row range when the chunk's display range is trimmed (e.g. markdown
    /// trailing blank lines).
    counts: Rc<[u32]>,
}

impl BodyChunk {
    /// Chunk a run of source rows: runs at or under [`CHUNK_ROWS`] render
    /// fully materialized, longer runs render sliced.
    pub fn rows(source: BodySource, start: u32, width: u16, trim: bool) -> Self {
        let end = source.row_count();
        if end.saturating_sub(start) <= CHUNK_ROWS {
            let lines = (start..end).map(|i| source.row(i)).collect();
            BodyChunk::Fixed(FixedChunk {
                lines,
                counts: RefCell::new(None),
            })
        } else {
            let counts: Rc<[u32]> = (start..end)
                .map(|i| source_row_count(&source, i, width, trim))
                .collect();
            BodyChunk::Sliced(SlicedChunk {
                source,
                start,
                counts,
            })
        }
    }

    /// A sliced chunk over `counts`-precomputed rows of `source`, covering
    /// `[start..start + counts.len())` at the width the counts were built
    /// with. The projection must have that many rows.
    pub fn counted(source: BodySource, start: u32, counts: Rc<[u32]>) -> Self {
        debug_assert!(
            usize::try_from(source.row_count().saturating_sub(start)).unwrap_or(usize::MAX)
                >= counts.len(),
        );
        BodyChunk::Sliced(SlicedChunk {
            source,
            start,
            counts,
        })
    }

    pub fn fixed(lines: Vec<Line<'static>>) -> Self {
        BodyChunk::Fixed(FixedChunk {
            lines,
            counts: RefCell::new(None),
        })
    }

    /// Wrapped rows the chunk produces at `width`.
    fn row_count(&self, width: u16, trim: bool) -> u32 {
        match self {
            BodyChunk::Fixed(chunk) => chunk.counts(width, trim).iter().sum(),
            BodyChunk::Sliced(chunk) => chunk.counts.iter().sum(),
        }
    }

    /// Paint the wrapped rows `band` (chunk-local) at `rect`.
    fn paint_band(&self, band: Range<u32>, width: u16, trim: bool, rect: Rect, buf: &mut Buffer) {
        let window = match self {
            BodyChunk::Fixed(chunk) => {
                let counts = chunk.counts(width, trim);
                count_window(&counts, band.start, band.end)
                    .map(|(first, last, offset)| (chunk.lines[first..=last].to_vec(), offset))
            }
            BodyChunk::Sliced(chunk) => {
                count_window(&chunk.counts, band.start, band.end).map(|(first, last, offset)| {
                    (
                        (first..=last)
                            .map(|i| chunk.source.row(chunk.start + i as u32))
                            .collect::<Vec<Line<'static>>>(),
                        offset,
                    )
                })
            }
        };
        if let Some((lines, offset)) = window {
            render_window(lines, offset, trim, rect, buf);
        }
    }
}

/// Wrapped-row count of one source row at `width`: 1 when the row fits, the
/// exact wrapped count otherwise.
fn source_row_count(source: &BodySource, index: u32, width: u16, trim: bool) -> u32 {
    let text = source.row_text(index);
    if UnicodeWidthStr::width(text.as_str()) <= usize::from(width) {
        return 1;
    }
    wrapped_line_count(&source.row(index), width, trim)
}

/// Render `lines` with word wrapping, skipping `offset` wrapped rows from the
/// top. Ratatui wraps each source line independently, so a slice beginning
/// mid-body reproduces exactly the rows the whole-body paragraph shows at
/// that offset.
fn render_window(lines: Vec<Line<'static>>, offset: u32, trim: bool, rect: Rect, buf: &mut Buffer) {
    if lines.is_empty() {
        return;
    }
    Paragraph::new(lines)
        .wrap(Wrap { trim })
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0))
        .render(rect, buf);
}

/// Map a wrapped-row band `[lo..hi)` onto the source rows covering it:
/// `(first row index, last row index, offset within the first row)`.
fn count_window(counts: &[u32], lo: u32, hi: u32) -> Option<(usize, usize, u32)> {
    debug_assert!(lo < hi);
    let mut cum = 0u32;
    let mut first: Option<(usize, u32)> = None;
    for (i, count) in counts.iter().enumerate() {
        let next = cum + count;
        if first.is_none() && next > lo {
            first = Some((i, lo - cum));
        }
        if next >= hi {
            return first.map(|(first, offset)| (first, i, offset));
        }
        cum = next;
    }
    None
}

/// Wrapped-row count of one rendered line at `width` — exactly what the line
/// contributes inside a larger paragraph, since ratatui wraps each source
/// line independently. Lines that fit take the O(width) fast path.
pub(crate) fn wrapped_line_count(line: &Line<'_>, width: u16, trim: bool) -> u32 {
    if line.width() <= usize::from(width) {
        return 1;
    }
    Paragraph::new(vec![line.clone()])
        .wrap(Wrap { trim })
        .line_count(width) as u32
}

/// The text one wrapped row of `line` shows at `width`: rendered into a
/// scratch buffer with the same paragraph settings so copy output matches
/// the pixels exactly. The fast path returns the spans as-is when the line
/// fits.
pub(crate) fn visual_row_text(line: &Line<'_>, width: u16, trim: bool, index: u32) -> String {
    if index == 0 && line.width() <= usize::from(width) {
        return line.spans.iter().map(|s| s.content.clone()).collect();
    }
    let total = wrapped_line_count(line, width, trim);
    let offset = index.min(total.saturating_sub(1));
    let area = Rect::new(0, 0, width.max(1), 1);
    let mut buf = Buffer::empty(area);
    Paragraph::new(vec![line.clone()])
        .wrap(Wrap { trim })
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0))
        .render(area, &mut buf);
    (0..area.width)
        .map(|x| buf[(x, 0)].symbol().to_string())
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// The substring of a rendered row between content columns `[from, to)`,
/// keeping any wide glyph straddling a boundary. `to` past the row's end
/// takes the rest of the row.
pub(crate) fn slice_visual(text: &str, from: usize, to: usize) -> String {
    let mut out = String::new();
    let mut x = 0usize;
    for c in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if x + w <= from {
            x += w;
            continue;
        }
        if x >= to {
            break;
        }
        out.push(c);
        x += w;
    }
    out
}

/// One rendered chunk of the chat: its body chunks, an optional full-width
/// block background, padding applied inside that background, and an optional
/// click hit target spanning the segment's rows. Block models project into
/// segments; the chat engine measures, stamps hit addresses, and paints the
/// visible window of them into the frame.
pub struct Segment {
    pub chunks: Vec<BodyChunk>,
    pub bg: Option<Color>,
    pub padding: (u16, u16),
    pub hit: Option<BlockAddr>,
    pub trim: bool,
}

impl Segment {
    pub fn plain(lines: Vec<Line<'static>>) -> Self {
        Self::materialized(lines, None, (0, 0), true)
    }

    pub fn materialized(
        lines: Vec<Line<'static>>,
        bg: Option<Color>,
        padding: (u16, u16),
        trim: bool,
    ) -> Self {
        Self::chunked(BodyChunk::fixed(lines), bg, padding, trim)
    }

    /// A segment around one prebuilt chunk (e.g. a cached markdown render).
    pub fn chunked(chunk: BodyChunk, bg: Option<Color>, padding: (u16, u16), trim: bool) -> Self {
        Self {
            chunks: vec![chunk],
            bg,
            padding,
            hit: None,
            trim,
        }
    }

    pub fn spacer() -> Self {
        Self::plain(vec![Line::from("")])
    }

    fn text_width(&self, content_width: u16) -> u16 {
        content_width.saturating_sub(2 * self.padding.0).max(1)
    }

    /// Wrapped rows the body produces at `width` (excludes padding).
    fn body_rows(&self, width: u16) -> u32 {
        self.chunks
            .iter()
            .map(|chunk| chunk.row_count(width, self.trim))
            .sum()
    }

    fn is_empty(&self, width: u16) -> bool {
        self.body_rows(width) == 0
    }

    /// Every row materialized: resident lines plus sliced source rows. For
    /// tests and one-off text extraction only.
    #[cfg(test)]
    pub fn flattened(&self) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for chunk in &self.chunks {
            match chunk {
                BodyChunk::Fixed(chunk) => lines.extend(chunk.lines.iter().cloned()),
                BodyChunk::Sliced(chunk) => {
                    lines.extend(chunk.rows().map(|i| chunk.source.row(i)));
                }
            }
        }
        lines
    }

    pub fn measure(&self, content_width: u16) -> u32 {
        let width = self.text_width(content_width);
        let lines = if self.is_empty(width) {
            usize::from(self.bg.is_some())
        } else {
            self.body_rows(width) as usize
        };
        (lines.saturating_add(2 * self.padding.1 as usize)) as u32
    }

    /// Paint the segment given its row range in content space (`top`..`top +
    /// h`), clipped to the viewport `[scroll_y, scroll_y + clip.height)`.
    /// Rows outside the clip are skipped; only the chunks and source rows
    /// intersecting the clip materialize. A segment whose top rows fall above
    /// the viewport scrolls its paragraph so the visible rows stay aligned
    /// with the measured layout.
    pub fn paint(
        &self,
        clip: Rect,
        scroll_y: u32,
        top: u32,
        h: u32,
        content_width: u16,
        buf: &mut Buffer,
    ) {
        if h == 0 || clip.height == 0 {
            return;
        }
        let viewport_bottom = scroll_y + u32::from(clip.height);
        let vis_top = top.max(scroll_y);
        let vis_bottom = (top + h).min(viewport_bottom);
        if vis_top >= vis_bottom {
            return;
        }
        if let Some(bg) = self.bg {
            for row in vis_top..vis_bottom {
                let y = clip.y + u16::try_from(row - scroll_y).unwrap_or(u16::MAX);
                for x in clip.x..clip.x.saturating_add(content_width) {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_bg(bg);
                    }
                }
            }
        }
        let width = self.text_width(content_width);
        if self.is_empty(width) {
            return;
        }
        let pad_y = u32::from(self.padding.1.min((h.min(u32::from(u16::MAX)) as u16) / 2));
        let text_top = top + pad_y;
        let text_bottom = (top + h).saturating_sub(pad_y);
        let vt = text_top.max(vis_top);
        let vb = text_bottom.min(vis_bottom);
        if vt >= vb {
            return;
        }
        let lo = vt - text_top;
        let hi = vb - text_top;
        let mut cursor = 0u32;
        for chunk in &self.chunks {
            let rows = chunk.row_count(width, self.trim);
            if cursor >= hi {
                break;
            }
            if cursor + rows <= lo {
                cursor += rows;
                continue;
            }
            let a = lo.max(cursor) - cursor;
            let b = hi.min(cursor + rows) - cursor;
            let rect = Rect {
                x: clip.x.saturating_add(self.padding.0),
                y: clip.y + u16::try_from(text_top + cursor + a - scroll_y).unwrap_or(u16::MAX),
                width,
                height: u16::try_from(b - a).unwrap_or(u16::MAX),
            };
            chunk.paint_band(a..b, width, self.trim, rect, buf);
            cursor += rows;
        }
    }

    /// Resolve a segment-local wrapped row back to the source row it
    /// projects, mirroring `paint`'s layout. `None` on padding rows, empty
    /// segments, and rows past the body.
    pub fn locate_row(&self, row_in_seg: u32, content_width: u16) -> Option<ResolvedRow> {
        let width = self.text_width(content_width);
        if self.is_empty(width) {
            return None;
        }
        let pad_y = u32::from(self.padding.1);
        let body_row = row_in_seg.checked_sub(pad_y)?;
        let mut cursor = 0u32;
        for chunk in &self.chunks {
            let rows = chunk.row_count(width, self.trim);
            if body_row < cursor + rows {
                let local = body_row - cursor;
                return match chunk {
                    BodyChunk::Fixed(chunk) => {
                        let counts = chunk.counts(width, self.trim);
                        let (first, _last, offset) = count_window(&counts, local, local + 1)?;
                        let source = BodySource::Lines {
                            lines: Rc::from(chunk.lines.clone()),
                        };
                        Some(ResolvedRow {
                            source,
                            source_row: first as u32,
                            wrap_index: offset,
                            wrap_total: counts[first],
                            text_width: width,
                            pad_x: self.padding.0,
                            trim: self.trim,
                        })
                    }
                    BodyChunk::Sliced(chunk) => {
                        let (first, _last, offset) = count_window(&chunk.counts, local, local + 1)?;
                        Some(ResolvedRow {
                            source: chunk.source.clone(),
                            source_row: chunk.start + first as u32,
                            wrap_index: offset,
                            wrap_total: chunk.counts[first],
                            text_width: width,
                            pad_x: self.padding.0,
                            trim: self.trim,
                        })
                    }
                };
            }
            cursor += rows;
        }
        None
    }
}

/// A segment-local wrapped row resolved back to its source: which body row
/// produced it, which wrapped slice of that row it is, and the source handle
/// for text extraction. Copy-only; paint never touches it.
pub struct ResolvedRow {
    pub source: BodySource,
    pub source_row: u32,
    /// Wrapped-row offset within the source row.
    pub wrap_index: u32,
    pub wrap_total: u32,
    /// Text width the row was resolved at (content width minus the segment
    /// padding), for visual extraction.
    pub text_width: u16,
    /// Segment padding columns the text starts at, in content coordinates.
    pub pad_x: u16,
    pub trim: bool,
}

impl ResolvedRow {
    /// The row's logical copy text, without layout decoration.
    pub fn copy_text(&self) -> String {
        self.source.row_copy_text(self.source_row)
    }

    /// The styled line of the source row.
    pub fn line(&self) -> Line<'static> {
        self.source.row(self.source_row)
    }
}

impl SlicedChunk {
    /// The chunk's covered source-row range: `[start..start +
    /// counts.len())`.
    #[cfg(test)]
    pub fn rows(&self) -> Range<u32> {
        self.start..self.start + self.counts.len() as u32
    }
}

impl FixedChunk {
    /// Wrapped-row count per resident line at `width`, memoized per width.
    fn counts(&self, width: u16, trim: bool) -> Rc<[u32]> {
        let mut memo = self.counts.borrow_mut();
        if let Some((memo_width, counts)) = memo.as_ref()
            && *memo_width == width
        {
            return counts.clone();
        }
        let counts: Rc<[u32]> = self
            .lines
            .iter()
            .map(|line| wrapped_line_count(line, width, trim))
            .collect();
        *memo = Some((width, counts.clone()));
        counts
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn buffer_rows(buf: &Buffer, w: u16, h: u16) -> Vec<String> {
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn plain_segment_trims_leading_whitespace() {
        let seg = Segment::plain(vec![Line::from("    indented prose")]);
        let w = 20;
        assert!(seg.trim);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, 1));
        seg.paint(buf.area, 0, 0, seg.measure(w), w, &mut buf);
        assert_eq!(buffer_rows(&buf, w, 1)[0], "indented prose");
    }

    #[test]
    fn untrimmed_segment_keeps_leading_whitespace_when_wrapping() {
        let long = "        let value = compute_something(with_a_long_argument, more_args);";
        let del = Line::from(vec![
            Span::raw("     3      "),
            Span::raw("-"),
            Span::raw(long.to_string()),
        ]);
        let seg = Segment {
            chunks: vec![BodyChunk::fixed(vec![del])],
            bg: None,
            padding: (0, 0),
            hit: None,
            trim: false,
        };
        let w = 44;
        let h = seg.measure(w);
        assert!(h >= 2, "row must wrap");
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h as u16));
        seg.paint(buf.area, 0, 0, h, w, &mut buf);
        let rows = buffer_rows(&buf, w, h as u16);
        assert!(
            rows[0].starts_with("     3      -"),
            "gutter: {:?}",
            rows[0]
        );
        assert!(
            rows[0].contains("        let value"),
            "code indent: {:?}",
            rows[0]
        );
        assert!(
            rows[1..]
                .iter()
                .all(|r| r.is_empty() || r.starts_with(|c: char| !c.is_whitespace())),
            "continuation rows must not be dedented: {rows:?}"
        );
    }

    fn tricky_lines() -> Vec<Line<'static>> {
        vec![
            Line::from("short line"),
            Line::from(""),
            Line::from("a long word with no break opportunities aaaaaaaaaaaaaaaaaaaaaaaa end"),
            Line::from("日本語のテキストは幅が二倍になります so it wraps often"),
            Line::from("trailing spaces   "),
            Line::from("word word word word word word word word"),
            Line::from("exact-width-line-abcdefghij"),
        ]
    }

    #[test]
    fn per_line_counts_match_full_paragraph_count() {
        let lines = tricky_lines();
        for width in [10u16, 20, 27, 28, 40, 80] {
            for trim in [true, false] {
                let full = Paragraph::new(lines.clone())
                    .wrap(Wrap { trim })
                    .line_count(width) as u32;
                let sum: u32 = lines
                    .iter()
                    .map(|line| wrapped_line_count(line, width, trim))
                    .sum();
                assert_eq!(sum, full, "width {width} trim {trim}");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn full_render(seg: &Segment, width: u16) -> Buffer {
        let h = seg.measure(width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, h as u16));
        seg.paint(buf.area, 0, 0, h, width, &mut buf);
        buf
    }

    #[cfg(test)]
    pub(crate) fn assert_windows_match(seg: &Segment, width: u16, full: &Buffer, scrolls: &[u32]) {
        let height = full.area().height;
        for &scroll_y in scrolls {
            let viewport = height.min(6);
            let clip = Rect::new(0, 0, width, viewport);
            let mut buf = Buffer::empty(clip);
            seg.paint(clip, scroll_y, 0, u32::from(height), width, &mut buf);
            for row in 0..viewport as u32 {
                let content_row = scroll_y + row;
                if content_row >= height as u32 {
                    break;
                }
                for x in 0..width {
                    let expected = &full[(x, content_row as u16)];
                    let actual = &buf[(x, row as u16)];
                    assert_eq!(
                        actual.symbol(),
                        expected.symbol(),
                        "symbol row {content_row} col {x} scroll {scroll_y}"
                    );
                    assert_eq!(
                        actual.fg, expected.fg,
                        "fg row {content_row} col {x} scroll {scroll_y}"
                    );
                    assert_eq!(
                        actual.bg, expected.bg,
                        "bg row {content_row} col {x} scroll {scroll_y}"
                    );
                }
            }
        }
    }

    #[test]
    fn windowed_paint_matches_full_paint_for_fixed_chunks() {
        let seg = Segment::plain(tricky_lines());
        let width = 28;
        let full = full_render(&seg, width);
        assert_windows_match(
            &seg,
            width,
            &full,
            &[0, 1, 3, 5, 7, u32::from(full.area().height) - 1],
        );
    }

    fn big_text(count: u32) -> String {
        (0..count)
            .map(|i| {
                if i % 7 == 0 {
                    format!("out {i}: a long tail that surely wraps beyond the pane edge {i}")
                } else {
                    format!("out {i}: value {i} with some padding text to fill width")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn materialized_text_lines(count: u32) -> Vec<Line<'static>> {
        big_text(count)
            .lines()
            .map(|row| Line::from(Span::raw(row.to_string()).fg(theme::TEXT_DIM)))
            .collect()
    }

    fn sliced_text_segment(width: u16) -> Segment {
        let source = BodySource::Text {
            rows: TextRows::new(big_text(220)),
            prefix: "",
            style: Style::new().fg(theme::TEXT_DIM),
        };
        Segment {
            chunks: vec![BodyChunk::rows(source, 0, width, false)],
            bg: None,
            padding: (0, 0),
            hit: None,
            trim: false,
        }
    }

    #[test]
    fn sliced_chunk_paints_identically_to_materialized() {
        let width = 40;
        let sliced = sliced_text_segment(width);
        let materialized = Segment {
            chunks: vec![BodyChunk::fixed(materialized_text_lines(220))],
            bg: None,
            padding: (0, 0),
            hit: None,
            trim: false,
        };
        assert_eq!(
            sliced.flattened().len(),
            materialized.flattened().len(),
            "row count mismatch"
        );
        assert_eq!(sliced.measure(width), materialized.measure(width));
        let full = full_render(&materialized, width);
        assert_windows_match(
            &sliced,
            width,
            &full,
            &[0, 7, 100, 200, u32::from(full.area().height) - 1],
        );
    }

    fn text_source(count: u32) -> BodySource {
        BodySource::Text {
            rows: TextRows::new(big_text(count)),
            prefix: "",
            style: Style::new().fg(theme::TEXT_DIM),
        }
    }

    #[test]
    fn windowed_paint_matches_full_paint_across_chunk_boundaries() {
        let width = 30u16;
        let header = vec![Line::from("header"), Line::from("")];
        let seg = Segment {
            chunks: vec![
                BodyChunk::fixed(header.clone()),
                BodyChunk::rows(
                    text_source(220),
                    0,
                    width.saturating_sub(2 * BLOCK_PADDING.0),
                    false,
                ),
                BodyChunk::fixed(vec![Line::from("trailer")]),
            ],
            bg: Some(theme::SUCCESS_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: false,
        };
        let mut flat = header.clone();
        flat.extend(
            big_text(220)
                .lines()
                .map(|row| Line::from(Span::raw(row.to_string()).fg(theme::TEXT_DIM))),
        );
        flat.push(Line::from("trailer"));
        let fixed_equivalent = Segment {
            chunks: vec![BodyChunk::fixed(flat)],
            bg: Some(theme::SUCCESS_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: false,
        };
        assert_eq!(seg.measure(width), fixed_equivalent.measure(width));
        let full = full_render(&fixed_equivalent, width);
        assert_windows_match(
            &seg,
            width,
            &full,
            &[
                0,
                2,
                9,
                50,
                100,
                150,
                200,
                230,
                u32::from(full.area().height) - 1,
            ],
        );
    }

    #[test]
    fn short_runs_stay_fixed() {
        let source = BodySource::Text {
            rows: TextRows::new("a\nb\nc"),
            prefix: "",
            style: Style::new(),
        };
        match BodyChunk::rows(source, 0, 40, false) {
            BodyChunk::Fixed(chunk) => assert_eq!(chunk.lines.len(), 3),
            BodyChunk::Sliced(_) => panic!("short run must stay materialized"),
        }
    }

    #[test]
    fn text_rows_offsets_match_str_lines() {
        for text in [
            "",
            "a",
            "a\n",
            "a\nb",
            "a\r\nb",
            "\n\n",
            "a\n\nb\n",
            "one\r\ntwo\r\nthree",
        ] {
            let rows = TextRows::new(text);
            let expected: Vec<&str> = text.lines().collect();
            assert_eq!(rows.row_count() as usize, expected.len(), "text {text:?}");
            for (i, want) in expected.iter().enumerate() {
                assert_eq!(rows.row_str(i as u32), *want, "text {text:?} row {i}");
            }
        }
    }

    #[test]
    fn empty_body_segment_measures_as_bg_row() {
        let seg = Segment {
            chunks: Vec::new(),
            bg: Some(theme::PROMPT_BG),
            padding: BLOCK_PADDING,
            hit: None,
            trim: true,
        };
        assert_eq!(seg.measure(40), 1 + 2 * u32::from(BLOCK_PADDING.1));
    }

    fn sample_text_source() -> BodySource {
        BodySource::Text {
            rows: TextRows::new("short\na very long tool output row that wraps around the\nthird"),
            prefix: "",
            style: Style::new(),
        }
    }

    #[test]
    fn locate_row_resolves_fixed_chunks() {
        let seg = Segment::chunked(
            BodyChunk::rows(sample_text_source(), 0, 40, false),
            None,
            (2, 1),
            false,
        );
        assert_eq!(seg.measure(40), 6);
        assert!(seg.locate_row(0, 40).is_none(), "padding row");
        let head = seg.locate_row(1, 40).expect("first body row");
        assert_eq!(head.copy_text(), "short");
        assert_eq!((head.wrap_index, head.wrap_total), (0, 1));
        let wrapped_head = seg.locate_row(2, 40).expect("wrapped row");
        assert_eq!(wrapped_head.source_row, 1);
        assert_eq!(wrapped_head.wrap_index, 0);
        let wrapped_tail = seg.locate_row(3, 40).expect("wrapped tail");
        assert_eq!(wrapped_tail.source_row, 1);
        assert_eq!(wrapped_tail.wrap_index, 1);
        let tail = seg.locate_row(4, 40).expect("third source row");
        assert_eq!(tail.copy_text(), "third");
        assert!(seg.locate_row(5, 40).is_none(), "past the body");
    }

    #[test]
    fn locate_row_resolves_sliced_chunks() {
        let source = sample_text_source();
        let counts: Rc<[u32]> = (0..3)
            .map(|i| source_row_count(&source, i, 40, false))
            .collect();
        let seg = Segment::chunked(BodyChunk::counted(source, 0, counts), None, (2, 1), false);
        let mid = seg.locate_row(3, 40).expect("second wrapped row");
        assert_eq!(mid.source_row, 1);
        assert_eq!(mid.wrap_index, 1);
        assert_eq!(mid.wrap_total, 2);
        let tail = seg.locate_row(4, 40).expect("third source row");
        assert_eq!(tail.copy_text(), "third");
        assert!(seg.locate_row(5, 40).is_none(), "trailing padding");
    }

    #[test]
    fn visual_row_text_matches_painted_window() {
        for (trim, line) in [
            (false, Line::from("short line")),
            (
                false,
                Line::from("a much longer line that will definitely wrap at the given width"),
            ),
            (
                false,
                Line::from("        let value = compute_something(with_a_long_argument, args);"),
            ),
            (
                true,
                Line::from("        let value = compute_something(with_a_long_argument, args);"),
            ),
        ] {
            let w = 30;
            let total = wrapped_line_count(&line, w, trim);
            let mut buf = Buffer::empty(Rect::new(
                0,
                0,
                w,
                u16::try_from(2 * total.max(1)).unwrap_or(u16::MAX),
            ));
            Paragraph::new(vec![line.clone()])
                .wrap(Wrap { trim })
                .render(buf.area, &mut buf);
            for i in 0..total {
                let painted: String = (0..w)
                    .map(|x| buf[(x, i as u16)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string();
                assert_eq!(visual_row_text(&line, w, trim, i), painted);
            }
        }
    }

    #[test]
    fn visual_row_text_fast_path_matches_spans() {
        let line = Line::from("no wrap here");
        assert_eq!(visual_row_text(&line, 40, true, 0), "no wrap here");
    }

    #[test]
    fn copy_text_strips_layout_decoration() {
        let numbered = BodySource::Numbered {
            rows: TextRows::new("let x = 1;\nlet y = 2;"),
        };
        assert_eq!(numbered.row_copy_text(0), "let x = 1;");
        assert_eq!(numbered.row_text(0), "       1 let x = 1;");
        let diff = BodySource::Diff {
            lines: Rc::from(vec![
                DiffLine {
                    kind: DiffLineKind::Add,
                    old_line: None,
                    new_line: Some(3),
                    text: "let z = 3;".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Context,
                    old_line: Some(3),
                    new_line: Some(3),
                    text: "kept".to_string(),
                },
            ]),
        };
        assert_eq!(diff.row_copy_text(0), "+let z = 3;");
        assert_eq!(diff.row_copy_text(1), " kept");
    }
}
