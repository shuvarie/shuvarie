use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};

/// Horizontal + vertical inset applied inside a block's background rect: the
/// bg fills the full rows while the text renders one padding row down and the
/// same amount inset on each side.
pub const BLOCK_PADDING: (u16, u16) = (2, 1);

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
}

impl Segment {
    pub fn plain(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            bg: None,
            padding: (0, 0),
            hit: None,
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
                .wrap(Wrap { trim: true })
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
            .wrap(Wrap { trim: true })
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
