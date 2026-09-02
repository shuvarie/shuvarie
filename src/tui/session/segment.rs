use ratatui::{
    prelude::*,
    widgets::{Paragraph, Wrap},
};
use tui_scrollview::ScrollView;

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

#[derive(Debug, Clone)]
pub struct HitRegion {
    pub start: u16,
    pub end: u16,
    pub addr: BlockAddr,
}

/// One rendered chunk of the chat: its lines, an optional full-width block
/// background, padding applied inside that background, and an optional click
/// hit target spanning the segment's rows. Block models project into segments;
/// the chat engine measures, stamps hit addresses, fills backgrounds, and
/// paints them into the scroll view.
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

    fn text_height(&self, height: u16) -> u16 {
        height.saturating_sub(2 * self.padding.1).max(1)
    }

    pub fn measure(&self, content_width: u16) -> usize {
        let lines = if self.lines.is_empty() {
            usize::from(self.bg.is_some())
        } else {
            Paragraph::new(self.lines.clone())
                .wrap(Wrap { trim: true })
                .line_count(self.text_width(content_width))
                .min(u16::MAX as usize)
        };
        lines
            .saturating_add(2 * self.padding.1 as usize)
            .min(u16::MAX as usize)
    }

    pub fn view(
        &self,
        sv: &mut ScrollView,
        y: u16,
        h: u16,
        content_width: u16,
        regions: &mut Vec<HitRegion>,
    ) {
        if h == 0 {
            return;
        }
        if let Some(addr) = &self.hit {
            regions.push(HitRegion {
                start: y,
                end: y.saturating_add(h),
                addr: addr.clone(),
            });
        }
        if let Some(bg) = self.bg {
            let buf = sv.buf_mut();
            for row in y..y.saturating_add(h) {
                for x in 0..content_width {
                    buf[(x, row)].set_bg(bg);
                }
            }
        }
        if self.lines.is_empty() {
            return;
        }
        let para = Paragraph::new(self.lines.clone()).wrap(Wrap { trim: true });
        sv.render_widget(
            &para,
            Rect::new(
                self.padding.0,
                y + self.padding.1,
                self.text_width(content_width),
                self.text_height(h),
            ),
        );
    }
}
