//! The welcome screen's ASCII scene: a dim chess-rook watermark at the right
//! edge, a half-body knight raising a sword out of the bottom-left corner, and
//! a rain of arrows falling across the background. The `Shuvarie` wordmark and
//! the continue caption paint over it, so the scene reads as quiet chrome
//! behind the text.
//!
//! Every piece is laid out from the viewport alone, so the scene needs no
//! stored geometry, and each silhouette drops out when the terminal is too
//! small to place it without trampling the wordmark. The rain advances off
//! wall-clock time ([`FrameIndexer`]), so [`view`] paints whichever frame the
//! clock has selected and [`next_wake`] reports when the render loop should
//! come back for the next one.

use std::sync::OnceLock;
use std::time::Duration;

use frameplay::{FrameIndexer, FrameTimeReference, FrameplayOptions};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use tui_big_text::{BigText, PixelSize};

use crate::tui::theme;

/// The wordmark's letters, in order.
const WORDMARK_TEXT: &str = "Shuvarie";
/// One glyph's box: `tui-big-text` draws from the 8x8 `font8x8` bitmaps, and
/// [`PixelSize::Full`] spends one terminal cell on each font pixel. The layout
/// reserves this much room per letter, so it is spelled out here rather than
/// measured: `the_wordmark_paints_exactly_its_box` holds the widget to it.
const GLYPH_WIDTH: u16 = 8;
const GLYPH_HEIGHT: u16 = 8;

/// The chess rook, its crenellations on top and its base flared at the bottom.
const ROOK: [&str; 9] = [
    "▛▀▜ ▛▀▜ ▛▀▜",
    "███████████",
    "▌█████████▐",
    "███████████",
    " █████████ ",
    " █████████ ",
    "▐█████████▌",
    "███████████",
    "▀▀▀▀▀▀▀▀▀▀▀",
];

/// A knight's half body, sword raised: a great helm with its visor slit sits
/// above the shoulders, and the blade, crossguard, and grip of the sword the
/// knight has raised stand beside it.
const KNIGHT: [&str; 11] = [
    "     ▄▄▄▄▄▄",
    "   ▄████████▄   ▐█▌",
    "  ▟██████████▙  ▐█▌ ",
    "  ██▀▀▀▀▀▀▀▀██▄▄▄▄▄▄",
    "  ████████████  ▐█▌",
    "  ▀██████████▀  ▐█▌",
    "   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ ",
    " ▟██████████████████",
    "████████████████████",
    "████████▀▀▀▀▀███████",
    "▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀",
];

/// The arrow a falling drop paints.
const DROP: char = '↓';
/// How many cells a drop's tail trails behind its head.
const TRAIL: usize = 4;
/// Frames the rain indexer cycles through. Wide enough that a drop never
/// visibly jumps as the indexer wraps.
const RAIN_FRAMES: usize = 1_000;
/// Drops per second.
const RAIN_RATE: u32 = 12;

/// The caption under the wordmark.
const CAPTION: &str = "Press any key to continue";

/// The `Shuvarie` jumbotron: one line of oversized pixel text, built fresh on
/// each paint (it is eight glyphs of lookup, not worth caching).
fn big_text() -> BigText<'static> {
    BigText::builder()
        .pixel_size(PixelSize::Full)
        .style(
            Style::new()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )
        .lines(vec![Line::from(WORDMARK_TEXT)])
        .build()
}

/// The wordmark's painted size in cells: one glyph box per letter.
fn wordmark_size() -> (u16, u16) {
    let width = GLYPH_WIDTH * WORDMARK_TEXT.chars().count() as u16;
    (width, GLYPH_HEIGHT)
}

/// The rain indexer: a wall-clock clock the scene reads a frame number off.
fn rain() -> &'static FrameIndexer {
    static RAIN: OnceLock<FrameIndexer> = OnceLock::new();
    RAIN.get_or_init(|| {
        FrameIndexer::new(
            RAIN_FRAMES,
            FrameplayOptions {
                frame_time_reference: FrameTimeReference::StartTime,
                frame_rate: RAIN_RATE,
            },
        )
    })
}

/// The rain's current frame, straight off the wall clock.
pub fn rain_frame() -> usize {
    rain().get_cur_index()
}

/// How long until the rain's next frame — the render loop's wake. `None` when
/// the rain is not animating.
pub fn next_wake() -> Option<Duration> {
    rain().time_to_next_frame()
}

/// Paints the scene into `frame`, taking the rain's frame off the wall clock.
pub fn view(frame: &mut Frame<'_>, area: Rect) {
    paint(frame.buffer_mut(), area, rain_frame());
}

/// Buffer-level [`view`] with an explicit rain frame, so tests paint against a
/// bare [`Buffer`] and pick the frame they assert on.
pub fn paint(buf: &mut Buffer, area: Rect, rain_frame: usize) {
    if area.is_empty() {
        return;
    }
    // The page owns the whole screen: fill it with the theme background before
    // anything else, so nothing of the frame before it shows through.
    buf.set_style(area, Style::new().bg(theme::bg()).fg(theme::text()));

    let placement = placement(area);

    render_rain(buf, area, rain_frame);
    if let Some(origin) = placement.rook {
        render_glyphs(buf, area, origin, &ROOK, silhouette_style());
    }
    if let Some(origin) = placement.knight {
        render_glyphs(buf, area, origin, &KNIGHT, silhouette_style());
    }
    render_wordmark(buf, placement.wordmark);
    render_caption(buf, area, placement.wordmark);
}

/// Where the scene's pieces land on a given viewport.
struct Placement {
    /// The wordmark's box: the cells the big text paints into.
    wordmark: Rect,
    /// The knight's origin, `None` when it cannot clear the wordmark.
    knight: Option<(u16, u16)>,
    /// The rook's origin, `None` when it cannot clear the wordmark.
    rook: Option<(u16, u16)>,
}

/// Lays the scene out from the viewport alone: the knight claims the
/// bottom-left corner, the wordmark is centred above it but lifts when it would
/// otherwise trample the knight, and the rook takes the right edge beside the
/// wordmark. Each silhouette gives way when the viewport cannot hold it clear.
fn placement(area: Rect) -> Placement {
    let knight = bottom_left(area, &KNIGHT);
    let wordmark = wordmark_rect(area, knight);
    // The lift the wordmark asked for may not have been enough: on a viewport
    // that short the knight steps aside rather than scribbling over the text.
    let knight = knight.filter(|(_, top)| *top >= wordmark.bottom());
    let rook = rook_rect(area, wordmark);
    Placement {
        wordmark,
        knight,
        rook,
    }
}

/// The origin of a silhouette pinned to the bottom-left corner, or `None` when
/// the viewport cannot hold it clear of the edges.
fn bottom_left(area: Rect, art: &[&str]) -> Option<(u16, u16)> {
    let x = area.x + 1;
    let y = area.bottom().checked_sub(art.len() as u16)?;
    let width = art_width(art);
    (y >= area.y && x + width <= area.right()).then_some((x, y))
}

/// The rect the wordmark paints into: the block text centred on `area`, lifted
/// to leave the caption — and the knight — room underneath.
fn wordmark_rect(area: Rect, knight: Option<(u16, u16)>) -> Rect {
    let (width, height) = wordmark_size();
    let centred = area.y + area.height.saturating_sub(height + 2) / 2;
    let y = match knight {
        // The caption row would land in the knight's lap: lift the block clear.
        Some((_, top)) if centred + height + 1 > top => top.saturating_sub(height + 1).max(area.y),
        _ => centred,
    };
    let x = area.x + area.width.saturating_sub(width) / 2;
    Rect::new(x, y, width.min(area.width), height)
}

/// The widest row of a piece of art: the box the layout has to reserve for
/// it. The rows are stored without trailing padding, so the widest one — not
/// the first — sets the width.
fn art_width(art: &[&str]) -> u16 {
    art.iter().map(|row| row.chars().count()).max().unwrap_or(0) as u16
}

/// The rook's origin, pinned near the right edge and centred vertically.
/// `None` when it would run into the wordmark or off the bottom.
fn rook_rect(area: Rect, wordmark: Rect) -> Option<(u16, u16)> {
    let width = art_width(&ROOK);
    let height = ROOK.len() as u16;
    let x = area.right().checked_sub(width + 2)?;
    let y = area.y + area.height.saturating_sub(height) / 2;
    (x > wordmark.right() && y + height <= area.bottom()).then_some((x, y))
}

/// Paints a block of glyph art at `origin`: its blank cells are skipped, so
/// the rain behind the silhouettes keeps falling.
fn render_glyphs<S: AsRef<str>>(
    buf: &mut Buffer,
    area: Rect,
    origin: (u16, u16),
    rows: &[S],
    style: Style,
) {
    for (row, line) in rows.iter().enumerate() {
        let y = origin.1.saturating_add(row as u16);
        if y >= area.bottom() {
            break;
        }
        for (column, symbol) in line.as_ref().chars().enumerate() {
            if symbol == ' ' {
                continue;
            }
            let x = origin.0.saturating_add(column as u16);
            if x >= area.right() {
                break;
            }
            let cell = &mut buf[(x, y)];
            cell.set_char(symbol);
            cell.set_style(style);
        }
    }
}

/// Paints the jumbotron into the box the layout reserved for it. `BigText`
/// writes every cell of that box — blank pixels included — so the rain stops at
/// the wordmark rather than showing through the counters.
fn render_wordmark(buf: &mut Buffer, rect: Rect) {
    big_text().render(rect, buf);
}

fn render_caption(buf: &mut Buffer, area: Rect, wordmark: Rect) {
    let y = wordmark.bottom() + 1;
    if y >= area.bottom() {
        return;
    }
    let line = Rect::new(area.x, y, area.width, 1);
    Paragraph::new(Line::from(Span::styled(
        CAPTION,
        Style::new().fg(theme::text_muted()),
    )))
    .alignment(Alignment::Center)
    .render(line, buf);
}

/// The dim grey the two silhouettes paint in — present, but never competing
/// with the wordmark above them.
fn silhouette_style() -> Style {
    Style::new().fg(theme::text_dim())
}

/// Paints the falling arrows: one drop per screen column, each on its own
/// period and phase so the rain looks scattered without any stored state.
fn render_rain(buf: &mut Buffer, area: Rect, frame: usize) {
    let rows = area.height as usize;
    for column in 0..area.width {
        let seed = hash(u32::from(area.x.saturating_add(column)));
        // The period carries the drop past the bottom edge (and back round to
        // the top) before it repeats; the phase staggers the columns.
        let period = rows + 8 + (seed % 8) as usize;
        let head = (frame + (seed >> 8) as usize % period) % period;
        if head >= rows {
            continue;
        }
        for row in head.saturating_sub(TRAIL - 1)..=head {
            let style = drop_style(head - row);
            let cell = &mut buf[(
                area.x.saturating_add(column),
                area.y.saturating_add(row as u16),
            )];
            cell.set_char(DROP);
            cell.set_style(style);
        }
    }
}

/// A drop fades over its tail: the head is the brightest cell of the streak.
fn drop_style(fade: usize) -> Style {
    if fade == 0 {
        Style::new().fg(theme::text_muted())
    } else {
        Style::new()
            .fg(theme::text_muted())
            .add_modifier(Modifier::DIM)
    }
}

/// A small integer hash: seeds each column's drop so the rain scatters
/// deterministically, with no state to keep in step with the frame counter.
fn hash(value: u32) -> u32 {
    let mut x = value.wrapping_mul(0x9E37_79B9);
    x ^= x >> 15;
    x = x.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 13;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(width: u16, height: u16, frame: usize) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        paint(&mut buf, area, frame);
        buf
    }

    fn row_of(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn is_drop(buf: &Buffer, x: u16, y: u16) -> bool {
        buf[(x, y)].symbol().starts_with(DROP)
    }

    /// The wordmark paints exactly the box the layout reserves for it — no
    /// wider, or it would run into the silhouettes; no narrower, or `BigText`
    /// and the placement maths would disagree about where the text ends.
    #[test]
    fn the_wordmark_paints_exactly_its_box() {
        let (width, height) = wordmark_size();
        let area = Rect::new(0, 0, 120, 20);
        let wordmark = Rect::new(10, 5, width, height);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, Style::new().bg(theme::bg()).fg(theme::text()));
        render_wordmark(&mut buf, wordmark);

        let mut letters = 0;
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                let cell = &buf[(x, y)];
                let inside = (wordmark.x..wordmark.right()).contains(&x)
                    && (wordmark.y..wordmark.bottom()).contains(&y);
                if !inside {
                    assert_eq!(
                        cell.symbol(),
                        " ",
                        "({x}, {y}) paints outside the {width}x{height} box"
                    );
                    continue;
                }
                // The widget styles every cell of a glyph's box, blank pixels
                // included, so the box — and nothing but the box — is the
                // wordmark's, and the page's background survives under it.
                assert_eq!(cell.style().fg, Some(theme::accent()), "({x}, {y})");
                assert_eq!(cell.style().bg, Some(theme::bg()), "({x}, {y})");
                letters += usize::from(cell.symbol() != " ");
            }
        }
        assert!(letters > 0, "the wordmark spells something");
    }

    /// The silhouettes' boxes are the widths the placement maths reserves, so
    /// a row can never spill past the room the layout left it.
    #[test]
    fn silhouettes_fill_their_boxes() {
        assert_eq!(art_width(&ROOK), 11);
        assert_eq!(art_width(&KNIGHT), 20);
        for (name, art) in [("rook", &ROOK[..]), ("knight", &KNIGHT[..])] {
            let width = art_width(art);
            for row in art {
                assert!(
                    row.chars().count() <= width as usize,
                    "{name} row {row:?} overflows the {width}-cell box"
                );
            }
        }
    }

    /// A roomy terminal shows the whole scene: the rook clears the wordmark to
    /// the right, the knight takes the corner below it.
    #[test]
    fn a_roomy_terminal_places_every_piece() {
        let area = Rect::new(0, 0, 100, 30);
        let placement = placement(area);

        let rook = placement.rook.expect("rook fits a 100x30 terminal");
        let knight = placement.knight.expect("knight fits a 100x30 terminal");
        assert!(
            rook.0 >= placement.wordmark.right(),
            "rook starts right of the wordmark"
        );
        assert!(
            knight.1 >= placement.wordmark.bottom(),
            "knight starts below the wordmark"
        );
    }

    /// The wordmark is wide enough that a standard 80x24 terminal cannot fit
    /// the rook beside it, but the knight still takes the corner: the wordmark
    /// gives up the centred row rather than trampling the helmet.
    #[test]
    fn a_standard_terminal_lifts_the_wordmark_over_the_knight() {
        let area = Rect::new(0, 0, 80, 24);
        let placement = placement(area);
        let knight = placement.knight.expect("knight fits an 80x24 terminal");

        assert!(
            placement.rook.is_none(),
            "no room for the rook at 80 columns"
        );
        assert!(knight.1 >= placement.wordmark.bottom(), "knight is clear");
        assert!(
            placement.wordmark.bottom() < knight.1,
            "the caption row sits above the knight"
        );
        assert!(
            placement.wordmark.y < area.height / 2,
            "the block is lifted off the centred row to make room"
        );
    }

    /// A terminal too narrow or too short drops the silhouettes rather than
    /// letting them collide with the wordmark.
    #[test]
    fn a_cramped_terminal_drops_the_silhouettes() {
        let narrow = placement(Rect::new(0, 0, 20, 24));
        assert!(narrow.rook.is_none());
        assert!(narrow.knight.is_none());

        let short = placement(Rect::new(0, 0, 80, 12));
        assert!(short.rook.is_none());
        assert!(short.knight.is_none());
    }

    /// The intro paints the wordmark, the caption, and the rook on a terminal
    /// with room for them.
    #[test]
    fn the_scene_paints_the_wordmark_caption_and_rook() {
        let area = Rect::new(0, 0, 100, 30);
        let buf = scene(area.width, area.height, 0);
        let placement = placement(area);
        let wordmark = placement.wordmark;

        assert_eq!(
            buf[(wordmark.x, wordmark.y)].style().fg,
            Some(theme::accent()),
            "the wordmark owns its box's first cell"
        );
        assert!(
            (wordmark.y..wordmark.bottom())
                .flat_map(|y| (wordmark.x..wordmark.right()).map(move |x| (x, y)))
                .any(|(x, y)| buf[(x, y)].symbol() == "█"),
            "the wordmark spells something"
        );

        let caption = row_of(&buf, wordmark.bottom() + 1, area.width);
        assert!(caption.contains(CAPTION), "{caption:?}");
        assert!(
            caption.starts_with(' '),
            "the caption is centred: {caption:?}"
        );

        let rook = placement.rook.expect("rook fits");
        assert_eq!(buf[(rook.0, rook.1)].symbol(), "▛", "the rook's crown");
        assert_eq!(buf[(rook.0, rook.1)].style().fg, Some(theme::text_dim()));

        // The rain never buries the wordmark, whatever frame is showing.
        for frame in 0..8 {
            let buf = scene(area.width, area.height, frame);
            assert_eq!(
                buf[(wordmark.x, wordmark.y)].style().fg,
                Some(theme::accent()),
                "frame {frame} keeps the wordmark"
            );
        }
    }

    /// The rain animates across the whole background: every column's drop
    /// passes through the rows above the wordmark, where nothing else paints
    /// over it.
    #[test]
    fn the_rain_sweeps_every_column() {
        const ABOVE: u16 = 10;
        let raining: Vec<Vec<bool>> = (0..60)
            .map(|frame| {
                let buf = scene(80, 30, frame);
                (0..80)
                    .map(|column| (0..ABOVE).any(|row| is_drop(&buf, column, row)))
                    .collect()
            })
            .collect();
        for column in 0..80 {
            assert!(
                raining.iter().any(|frame| frame[column as usize]),
                "column {column} never rains"
            );
        }
    }

    /// The rain is deterministic: the same frame paints the same arrows, so the
    /// animation reads as motion rather than noise.
    #[test]
    fn the_rain_is_a_pure_function_of_its_frame() {
        assert_eq!(scene(60, 20, 7).content, scene(60, 20, 7).content);
        assert_ne!(scene(60, 20, 7).content, scene(60, 20, 8).content);
    }
}
