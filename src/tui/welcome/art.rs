//! The welcome screen's ASCII scene: a dim chess-rook watermark at the right
//! edge, a half-body knight raising a sword out of the bottom-left corner, and
//! a rain of arrows falling down and to the right across the background. The
//! `Shuvarie` wordmark and the continue caption paint over it, so the scene
//! reads as quiet chrome behind the text.
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
use ratatui::layout::{Alignment, Position, Rect};
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

/// The arrow a falling drop paints: the shaft of an arrow shot from a bow. A
/// streak lies along the diagonal its drop falls down, so the shaft points the
/// way it travels.
const SHAFT: char = '\\';
/// How many cells of shaft trail behind a drop's tip.
const TRAIL: usize = 4;
/// How far a drop drifts downwind for every row it falls — the slope of the
/// rain, and so the lean of every shaft it paints. [`lanes`] starts one drop on
/// the top row and the left column for each diagonal of the page, which between
/// them cover every diagonal only while this is one.
const DRIFT: i32 = 1;
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

/// Paints the falling arrows: one drop per diagonal of the page, each on its
/// own period and phase so the rain looks scattered without any stored state. A
/// streak lies along the diagonal its drop falls down, so the shaft points the
/// way it travels and the tail is a step of the fall itself.
fn render_rain(buf: &mut Buffer, area: Rect, frame: usize) {
    for head in streak_heads(area, frame) {
        // The streak, from its bright tip back along the shaft.
        for step in 0..TRAIL as i32 {
            let (x, y) = tail_cell(head, step);
            paint_arrow(buf, area, x, y, arrow_style(step as usize));
        }
    }
}

/// The lanes of the rain: the cells a drop enters the page on — the whole top
/// row, then the left column below the corner. Between them these head every
/// diagonal the page has, so the rain covers it edge to edge, bottom-left corner
/// included, and no two streaks can ever fight over a cell.
fn lanes(area: Rect) -> Vec<(i32, i32)> {
    let (left, top) = (i32::from(area.x), i32::from(area.y));
    let (width, height) = (i32::from(area.width), i32::from(area.height));
    let across = (0..width).map(|column| (left + column, top));
    let down = (1..height).map(|row| (left, top + row));
    across.chain(down).collect()
}

/// Where each lane's drop has fallen to in `frame`: the cell at the tip of its
/// streak, in screen coordinates. Lanes whose drop is off the page — falling,
/// off the bottom-right, or waiting up and to the left to re-enter — are
/// skipped.
fn streak_heads(area: Rect, frame: usize) -> Vec<(i32, i32)> {
    let (right, bottom) = (i32::from(area.right()) - 1, i32::from(area.bottom()) - 1);
    // A lane waits at least the page's longest diagonal before it repeats, so a
    // short lane — a corner — rains no harder than the middle does.
    let floor = usize::from(area.width.min(area.height)) + 2 * TRAIL;
    let mut heads = Vec::new();
    for (x, y) in lanes(area) {
        // The steps the drop takes to cross from its entry cell to the far edge:
        // the diagonal the page shows of its lane.
        let span = (right - x).min(bottom - y) + 1;
        let seed = hash(u32::from(x as u16) ^ (u32::from(y as u16) << 16));
        // The period carries the drop clear off the page — streak and all —
        // before it re-enters at the top-left; the phase staggers the lanes.
        let period = (span as usize + 2 * TRAIL + seed as usize % 8).max(floor);
        let step = (frame + (seed >> 8) as usize % period) % period;
        let head = step as i32 - TRAIL as i32;
        if (0..span).contains(&head) {
            heads.push((x + DRIFT * head, y + head));
        }
    }
    heads
}

/// The cell a streak occupies `step` steps behind its tip: back up one row and,
/// with it, [`DRIFT`] columns to the left. Step 0 is the tip itself.
fn tail_cell(head: (i32, i32), step: i32) -> (i32, i32) {
    (head.0 - DRIFT * step, head.1 - step)
}

/// Paints one cell of a streak. A shaft runs off the top-left corner while its
/// drop waits to re-enter, and off the bottom-right as the drop falls clear of
/// the page: anything outside it is simply dropped.
fn paint_arrow(buf: &mut Buffer, area: Rect, x: i32, y: i32, style: Style) {
    let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) else {
        return;
    };
    if !area.contains(Position::new(x, y)) {
        return;
    }
    let cell = &mut buf[(x, y)];
    cell.set_char(SHAFT);
    cell.set_style(style);
}

/// A shaft fades towards its tail: the tip is the brightest cell of the streak.
fn arrow_style(fade: usize) -> Style {
    if fade == 0 {
        Style::new().fg(theme::text_muted())
    } else {
        Style::new()
            .fg(theme::text_muted())
            .add_modifier(Modifier::DIM)
    }
}

/// A small integer hash: seeds each lane's drop so the rain scatters
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

    fn is_arrow(buf: &Buffer, x: u16, y: u16) -> bool {
        buf[(x, y)].symbol().starts_with(SHAFT)
    }

    /// The tips of the streaks — the bright end of every shaft — among the
    /// `rows` above the wordmark, the one strip of the page where the rain is
    /// the only thing painting.
    fn tips(buf: &Buffer, rows: u16) -> Vec<(i32, i32)> {
        (0..rows)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                is_arrow(buf, x, y) && !buf[(x, y)].style().add_modifier.contains(Modifier::DIM)
            })
            .map(|(x, y)| (i32::from(x), i32::from(y)))
            .collect()
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

    /// The rain animates across the whole background: every column is rained on
    /// above the wordmark, where nothing else paints over it.
    #[test]
    fn the_rain_sweeps_every_column() {
        const ABOVE: u16 = 10;
        let raining: Vec<Vec<bool>> = (0..60)
            .map(|frame| {
                let buf = scene(80, 30, frame);
                (0..80)
                    .map(|column| (0..ABOVE).any(|row| is_arrow(&buf, column, row)))
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

    /// The lanes between them head every diagonal the page has, so the rain
    /// reaches the bottom-left corner — which drops entering on the top row
    /// alone never would — and no two streaks can fight over a cell.
    #[test]
    fn the_lanes_cover_every_diagonal() {
        let area = Rect::new(3, 7, 80, 24);
        let mut diagonals: Vec<i32> = lanes(area).iter().map(|&(x, y)| x - DRIFT * y).collect();
        assert_eq!(diagonals.len(), 80 + 24 - 1, "one lane per diagonal");

        diagonals.sort_unstable();
        diagonals.dedup();
        // Every diagonal the page has: from its bottom-left corner's to its
        // top-right's, with no gaps between the lanes.
        let first = i32::from(area.x) - i32::from(area.bottom()) + 1;
        let last = i32::from(area.right()) - 1 - i32::from(area.y);
        assert_eq!(diagonals, (first..=last).collect::<Vec<_>>());
    }

    /// The rain travels on the diagonal, not merely painted on one: a tip one
    /// frame later is a step down and a step right of where it was, so the
    /// arrows fall rather than slide sideways.
    #[test]
    fn the_rain_travels_on_the_diagonal() {
        let area = Rect::new(0, 0, 80, 30);
        let mut checked = 0;
        for frame in 0..40 {
            // Tips from row 8 down are left out, so every successor — a step
            // down — stays in the rain-only band the `after` frame is read from.
            let before = tips(&scene(area.width, area.height, frame), 8);
            let after = tips(&scene(area.width, area.height, frame + 1), 10);
            for (x, y) in before {
                let (next_x, next_y) = (x + 1, y + 1);
                if next_x >= i32::from(area.width) {
                    continue; // a tip at the right edge falls off the page instead
                }
                assert!(
                    after.contains(&(next_x, next_y)),
                    "the shaft tipped at ({x}, {y}) was elsewhere than ({next_x}, {next_y})"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "a tip was on the page to follow");
    }

    /// A shaft trails back up and to the left of its tip, along the diagonal its
    /// drop falls down, so the arrow leans into its fall.
    #[test]
    fn a_shaft_trails_up_and_to_the_left_of_its_tip() {
        assert_eq!(DRIFT, 1);
        assert_eq!(tail_cell((10, 20), 0), (10, 20));
        assert_eq!(tail_cell((10, 20), 1), (9, 19));
        assert_eq!(tail_cell((10, 20), TRAIL as i32 - 1), (7, 17));

        // Read the streaks off the band above the wordmark — the one strip of
        // the page where the rain is the only thing painting.
        const ABOVE: u16 = 10;
        let buf = scene(80, 30, 0);
        let mut tails = 0;
        for (x, y) in tips(&buf, ABOVE) {
            for step in 1..TRAIL as i32 {
                let (tail_x, tail_y) = tail_cell((x, y), step);
                let (Ok(tail_x), Ok(tail_y)) = (u16::try_from(tail_x), u16::try_from(tail_y))
                else {
                    break; // the rest of the shaft has run off the page
                };
                assert!(
                    is_arrow(&buf, tail_x, tail_y),
                    "the shaft tipped at ({x}, {y}) trails to ({tail_x}, {tail_y})"
                );
                tails += 1;
            }
        }
        assert!(tails > 0, "a shaft's tail was on screen to check");
    }

    /// The rain is deterministic: the same frame paints the same arrows, so the
    /// animation reads as motion rather than noise.
    #[test]
    fn the_rain_is_a_pure_function_of_its_frame() {
        assert_eq!(scene(60, 20, 7).content, scene(60, 20, 7).content);
        assert_ne!(scene(60, 20, 7).content, scene(60, 20, 8).content);
    }
}
