use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

/// Horizontal + vertical inset applied inside a block's background rect: the
/// bg fills the full rows while the text renders one padding row down and the
/// same amount inset on each side.
pub const BLOCK_PADDING: (u16, u16) = (2, 1);

/// Vertical-only inset for a bare text block: one blank row above and below
/// the content, no background, no side inset.
pub const TEXT_PADDING: (u16, u16) = (0, 1);

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

/// One rendered chunk of the chat: its lines, an optional full-width block
/// background, padding applied inside that background, and an optional click
/// hit target spanning the segment's rows. Block models project into segments;
/// the chat engine measures, stamps hit addresses, and paints the visible
/// window of them into the frame.
pub struct Segment {
    pub lines: Vec<Line<'static>>,
    pub bg: Option<Color>,
    pub padding: (u16, u16),
    pub hit: Option<BlockAddr>,
    pub trim: bool,
}

impl Segment {
    pub fn plain(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            bg: None,
            padding: (0, 0),
            hit: None,
            trim: true,
        }
    }

    pub fn spacer() -> Self {
        Self::plain(vec![Line::from("")])
    }

    fn text_width(&self, content_width: u16) -> u16 {
        content_width.saturating_sub(2 * self.padding.0).max(1)
    }

    pub fn measure(&self, content_width: u16) -> u32 {
        let lines = if self.lines.is_empty() {
            usize::from(self.bg.is_some())
        } else {
            Paragraph::new(self.lines.clone())
                .wrap(Wrap { trim: self.trim })
                .line_count(self.text_width(content_width))
        };
        (lines.saturating_add(2 * self.padding.1 as usize)) as u32
    }

    /// Paint the segment given its row range in content space (`top`..`top +
    /// h`), clipped to the viewport `[scroll_y, scroll_y + clip.height)`.
    /// Rows outside the clip are skipped; a segment whose top rows fall above
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
        if self.lines.is_empty() {
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
        let skip = u16::try_from(vt - text_top).unwrap_or(u16::MAX);
        let para = Paragraph::new(self.lines.clone())
            .wrap(Wrap { trim: self.trim })
            .scroll((skip, 0));
        para.render(
            Rect {
                x: clip.x.saturating_add(self.padding.0),
                y: clip.y + u16::try_from(vt - scroll_y).unwrap_or(u16::MAX),
                width: self.text_width(content_width),
                height: u16::try_from(vb - vt).unwrap_or(u16::MAX),
            },
            buf,
        );
    }
}

#[cfg(test)]
mod tests {
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
            lines: vec![del],
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
}
