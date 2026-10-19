//! The onboarding title's ASCII scene, which `/screensaver` shows too: a dim
//! chess-rook watermark hanging off the bottom-right corner, a rain of arrows
//! falling down and to the right across the background, and the running version
//! stamped dim in the top-right corner. The `Shuvarie` wordmark and the caption
//! paint over it, so the scene reads as quiet chrome behind the text.
//!
//! The page sweeps in rather than appearing whole: for its first [`ENTRANCE`] it
//! is wiped down the screen behind an irregular edge, its dark plate with it, so
//! it covers what was underneath rather than cutting to it. That is a
//! `tachyonfx` effect over the finished page, which is why the page is built in
//! a buffer of its own and only the part the edge has reached is copied out.
//! What stands ahead of the edge is the caller's to paint: onboarding leaves the
//! empty frame, and the screensaver puts the session there, so the live screen is
//! what the scene wipes away as it comes up.
//!
//! The caption belongs to the page, not to the scene: it says what a key press
//! does next, and the two pages that show this scene promise different things.
//!
//! The rook is the artwork from the `shuvarie-artwork` repository, embedded in
//! the binary as the pixel grid itself and painted as half blocks in the theme's
//! dim tone. It is a fixed size — one art pixel per cell half, so the terminal
//! never scales it — standing on the bottom-right corner, with what the viewport
//! cannot hold clipped off the silhouette rather than squeezed into it. The rain
//! advances off wall-clock time ([`FrameIndexer`]), so [`view`] paints whichever
//! frame the clock has selected and [`next_wake`] reports when the render loop
//! should come back for the next one.
//!
//! The scene is a pure function of its two clocks — the rain's frame number and
//! the age the page reports — and holds no state of its own. The entrance keeps
//! that property by being **rebuilt on every paint** and advanced straight to
//! the age rather than being kept alive between frames: `sweep_in` draws its
//! jitter from a fixed seed, so a rebuilt effect lays down exactly the edge an
//! effect advanced frame by frame would have.

use std::sync::OnceLock;
use std::time::Duration;

use frameplay::{FrameIndexer, FrameTimeReference, FrameplayOptions};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use tachyonfx::{Effect, EffectTimer, Interpolation, Motion, fx};
use tui_big_text::{BigText, PixelSize};

use crate::tui::components::version_label;
use crate::tui::theme;

/// The rook artwork, vendored from the `shuvarie-artwork` repository's
/// `png/rook.png`: the pixel grid that PNG scales up five times over, cropped to
/// the pixels that carry ink. `#` is ink and `.` is clear, so the silhouette can
/// be read straight off the source.
///
/// Embedded as the grid rather than as the image, so the binary carries the
/// scene, the paint path never touches the filesystem, and no decoder has to run
/// to draw a watermark. The artwork is a hard-edged mask — a pixel is ink or it
/// is clear, never something between — which is what lets the drawn cells be read
/// off it one for one, with no resampling in between.
const ROOK_ART: [&str; ROOK_ART_HEIGHT] = [
    ".###......###......###......###.",
    ".####....#####....#####....####.",
    ".#.###...##.##....##.##...###.#.",
    ".#..##...#...#....#...#...##..#.",
    ".#.########.########.########.#.",
    ".##############################.",
    ".##############################.",
    ".##############################.",
    ".##############################.",
    ".##.########################.##.",
    ".##.########################.##.",
    ".##.########################.##.",
    ".##.########################.##.",
    ".##############################.",
    ".##############################.",
    ".##############################.",
    "..############################..",
    "...##########################...",
    "......####################......",
    "........################........",
    ".........##############.........",
    ".........##############.........",
    "..........############..........",
    "..........############..........",
    "..........############..........",
    "..........############..........",
    "..........############..........",
    ".........##############.........",
    ".........##############.........",
    ".........##############.........",
    ".........##############.........",
    ".........##############.........",
    ".........##############.........",
    "........################........",
    "........################........",
    "........################........",
    ".......##################.......",
    ".......##################.......",
    ".......##################.......",
    ".......##################.......",
    "......####################......",
    "......####################......",
    "....########################....",
    "....########################....",
    "..############################..",
    ".##############################.",
    "################################",
    "################################",
    "################################",
    "################################",
    "################################",
];

/// The grid's width in art pixels, which is the width of the whole rook.
const ROOK_ART_WIDTH: usize = 32;
/// The grid's height in art pixels, which over the half blocks' grain of two art
/// rows per cell is the height of the whole rook.
const ROOK_ART_HEIGHT: usize = 51;

/// The rook as drawn: one art pixel per sub-pixel of a cell, so a row of cells
/// carries two art rows and the silhouette is the artwork's own pixels.
const ROOK_COLUMNS: u16 = ROOK_ART_WIDTH as u16;
const ROOK_ROWS: u16 = (ROOK_ART_HEIGHT as u16).div_ceil(2);
/// How much of the rook's width hangs off the right edge — the crop that pins
/// the silhouette to the corner instead of drawing it whole beside the text.
const ROOK_CROP_PERCENT: u16 = 40;
/// The columns of the rook left inside the right edge: what the corner holds of
/// it. The rook is a fixed size, so a viewport narrower than this clips the rest
/// of the silhouette rather than scaling it down.
const ROOK_VISIBLE_COLUMNS: u16 = ROOK_COLUMNS - ROOK_COLUMNS * ROOK_CROP_PERCENT / 100;

/// The half-block cells the rook paints with: one sub-pixel of the artwork per
/// cell half, so a row of cells carries two rows of the silhouette.
const UPPER_HALF: char = '▀';
const LOWER_HALF: char = '▄';
const FULL_BLOCK: char = '█';

/// The wordmark's letters, in order.
const WORDMARK_TEXT: &str = "Shuvarie";
/// One glyph's box: `tui-big-text` draws from the 8x8 `font8x8` bitmaps, and
/// [`PixelSize::Full`] spends one terminal cell on each font pixel. The layout
/// reserves this much room per letter, so it is spelled out here rather than
/// measured: `the_wordmark_paints_exactly_its_box` holds the widget to it.
const GLYPH_WIDTH: u16 = 8;
const GLYPH_HEIGHT: u16 = 8;

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

/// How long the scene takes to sweep in. Long enough to read as an arrival,
/// short enough not to hold anyone up on the way to the provider form.
pub const ENTRANCE: Duration = Duration::from_millis(900);
/// How many cells deep each column's own edge fades in. Small on purpose: it is
/// the tear across the columns, not a wide gradient, that should shape the
/// boundary.
const ENTRANCE_GRADIENT: u16 = 3;
/// How much of the page's height the sweep's torn edge spans, as a percentage.
/// This is what makes the sweep irregular rather than a straight wipe, and it is
/// taken from the page so that a tall terminal is not wiped cleanly while a
/// short one is shredded.
const ENTRANCE_TEAR_PERCENT: u16 = 35;
/// The scene's frame rate while the sweep is running, in whole milliseconds a
/// frame. Once the scene has arrived the rain's twelve frames a second are its
/// heartbeat, but a gradient crossing the page in eleven steps reads as dropped
/// frames rather than as animation, so the entrance asks the render loop to come
/// back faster.
const ENTRANCE_FRAME_MS: u64 = 1_000 / 30;
/// The entrance's frame period, as the duration the render loop is handed.
const ENTRANCE_FRAME: Duration = Duration::from_millis(ENTRANCE_FRAME_MS);

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

/// The entrance sweep: the page wiped in from the top of the screen.
///
/// Each column's leading edge is offset from its neighbours over about
/// [`ENTRANCE_TEAR_PERCENT`] of the page, so the edge is torn rather than
/// straight. The faded colour is the plate's own, which does double duty: it is
/// what a cell ahead of the edge is painted, and so what [`paint`] reads the edge
/// back off the built page with.
///
/// Built fresh on every paint rather than kept between frames: the jitter comes
/// from a fixed seed, so rebuilding costs nothing but the edge is identical
/// either way. See the module docs.
fn entrance(area: Rect, plate: Color) -> Effect {
    let tear = (area.height * ENTRANCE_TEAR_PERCENT / 100).max(1);
    fx::sweep_in(
        Motion::UpToDown,
        ENTRANCE_GRADIENT,
        tear,
        plate,
        EffectTimer::from_ms(ENTRANCE.as_millis() as u32, Interpolation::CubicOut),
    )
}

/// How long until the entrance's next frame; `None` once the scene has arrived.
fn entrance_wake(age: Duration) -> Option<Duration> {
    (age < ENTRANCE).then(|| {
        // `Duration` has no remainder, so the phase within the current frame is
        // taken in whole milliseconds — a wake is no finer than the frame it
        // waits for anyway.
        let phase = age.as_millis() as u64 % ENTRANCE_FRAME_MS;
        ENTRANCE_FRAME - Duration::from_millis(phase)
    })
}

/// How long until the scene's next frame — the render loop's wake. The entrance
/// sets the pace while it runs; once the scene has arrived the rain is all that
/// moves.
///
/// `age` is how long the page showing the scene has been up, and it is the
/// caller's to keep: the scene reads no clock of its own beyond the rain's.
pub fn next_wake(age: Duration) -> Option<Duration> {
    let rain = rain().time_to_next_frame();
    match entrance_wake(age) {
        Some(entrance) => Some(rain.map_or(entrance, |rain| rain.min(entrance))),
        None => rain,
    }
}

/// Paints the scene into `frame`, taking the rain's frame off the wall clock.
///
/// `age` is how long the page has been showing the scene: the entrance sweep is
/// drawn from it, and a page reporting an age past [`ENTRANCE`] gets the settled
/// scene.
pub fn view(frame: &mut Frame<'_>, area: Rect, age: Duration, caption: &str) {
    paint(frame.buffer_mut(), area, rain_frame(), age, caption);
}

/// Buffer-level [`view`] with explicit clocks, so tests paint against a bare
/// [`Buffer`] and pick the frame and the age they assert on.
///
/// The page arrives by covering `buf` behind the entrance's edge, and the cells
/// ahead of it are never written to at all: they are left exactly as the caller
/// painted them, which is where the session stands under the screensaver. See
/// the module docs.
pub fn paint(buf: &mut Buffer, area: Rect, rain_frame: usize, age: Duration, caption: &str) {
    if area.is_empty() {
        return;
    }
    if age >= ENTRANCE {
        // Settled: the page goes down whole rather than through the effect, so
        // the steady state is the drawn page exactly and not the effect's last
        // frame of it.
        paint_page(buf, area, rain_frame, caption);
        return;
    }

    // Part way in, the page is built off the buffer and the entrance applied to
    // *that*: `sweep_in` paints the cells its edge has not reached, so running it
    // over the caller's own pixels would bury what the page is covering.
    //
    // The plate is read once and threaded through: the palette is process-global
    // and can be swapped from another thread at any point (a theme preview), and
    // a `theme::bg()` that changes between building the sweep and reading the
    // edge back would make every cell look like ink — burying the screen the
    // scene is supposed to be sweeping over.
    let plate = theme::bg();
    let mut page = Buffer::empty(area);
    paint_page(&mut page, area, rain_frame, caption);
    entrance(area, plate).process(age, &mut page, area);

    for pos in area.positions() {
        let painted = &page[pos];
        // `sweep_in` writes the faded colour to *both* channels of every cell
        // ahead of its edge and leaves the rest of the page alone, so a cell
        // carrying the plate on both is one the edge has not reached. Nothing in
        // the page is ever drawn in the plate's own colour, which is what makes
        // that readable — `the_plate_is_never_painted_as_ink` holds the scene to
        // it.
        if painted.fg == plate && painted.bg == plate {
            continue;
        }
        buf[pos] = painted.clone();
    }
}

/// The page itself, painted whole into `buf`: the plate, then the scene on it.
fn paint_page(buf: &mut Buffer, area: Rect, rain_frame: usize, caption: &str) {
    // The page owns the whole screen, so every cell it covers is rewritten — the
    // symbol with the style. Restyling alone would leave the glyphs of the screen
    // underneath standing in the page's own ink, which is exactly the screen the
    // screensaver's scene is sweeping away: the plate has to clear it.
    let plate = Style::new().bg(theme::bg()).fg(theme::text());
    for pos in area.positions() {
        let cell = &mut buf[pos];
        cell.reset();
        cell.set_style(plate);
    }

    let placement = placement(area);

    render_rain(buf, area, rain_frame);
    if let Some(rook) = placement.rook {
        render_rook(buf, rook);
    }
    // The version is chrome and the wordmark is the subject, so the stamp goes
    // down first: on a terminal short enough to bring the two onto the same
    // row, `BigText`'s opaque box is what should win.
    render_version(buf, area);
    render_wordmark(buf, placement.wordmark);
    render_caption(buf, area, placement.wordmark, caption);
}

/// Where the scene's pieces land on a given viewport.
struct Placement {
    /// The wordmark's box: the cells the big text paints into.
    wordmark: Rect,
    /// The rook's box, `None` when the viewport is empty.
    rook: Option<Rect>,
}

/// Lays the scene out from the viewport alone: the wordmark takes the centred
/// row, and the rook hangs off the bottom-right corner beside it.
fn placement(area: Rect) -> Placement {
    Placement {
        wordmark: wordmark_rect(area),
        rook: rook_rect(area),
    }
}

/// The rect the wordmark paints into: the block text centred on `area`, with a
/// row left underneath for the caption.
fn wordmark_rect(area: Rect) -> Rect {
    let (width, height) = wordmark_size();
    let y = area.y + area.height.saturating_sub(height + 2) / 2;
    let x = area.x + area.width.saturating_sub(width) / 2;
    Rect::new(x, y, width.min(area.width), height)
}

/// The rook's box: a fixed size — the artwork's own pixels, one per cell half,
/// so the terminal never scales it — standing on the bottom row and flush with
/// the right edge, with [`ROOK_CROP_PERCENT`] of its width running off that
/// edge. `None` when the viewport is empty.
///
/// What a cramped viewport cannot hold is clipped off the silhouette rather than
/// squeezed into it: a viewport shorter than the rook takes the top of it, and
/// one narrower than the part that shows takes the left. Either way the cells
/// that are left stay pinned to the corner, so the artwork still reads as
/// something running off the page's edge.
fn rook_rect(area: Rect) -> Option<Rect> {
    if area.is_empty() {
        return None;
    }
    let width = ROOK_VISIBLE_COLUMNS.min(area.width);
    let height = ROOK_ROWS.min(area.height);
    Some(Rect::new(
        area.right() - width,
        area.bottom() - height,
        width,
        height,
    ))
}

/// Whether the artwork inks the pixel `column` art columns across and `art_row`
/// art rows down the grid. A row of cells is two art rows, so on an artwork of
/// odd height the last cell's lower half falls past the artwork: an art row the
/// grid does not reach is clear.
fn inked(column: usize, art_row: usize) -> bool {
    ROOK_ART
        .get(art_row)
        .and_then(|row| row.as_bytes().get(column))
        .is_some_and(|&pixel| pixel == b'#')
}

/// Paints the rook into `rect` as half blocks in the dim silhouette tone. The
/// cells the silhouette does not reach are left alone, so the rain keeps falling
/// behind it and the rook reads as a watermark rather than a filled rectangle.
///
/// A cell is one art column wide and two art rows tall, read straight off the
/// grid, so the silhouette is the artwork's own pixels at the artwork's own
/// size. The cells a cramped viewport clipped are counted off the top and the
/// left of the rook's box, so what is drawn is the corner of the artwork the box
/// could hold rather than its top-left corner.
fn render_rook(buf: &mut Buffer, rect: Rect) {
    let columns = usize::from(rect.width);
    let rows = usize::from(rect.height);
    let first_column = usize::from(ROOK_VISIBLE_COLUMNS) - columns;
    let first_row = usize::from(ROOK_ROWS) - rows;
    for row in 0..rows {
        let y = rect.y + row as u16;
        let top = 2 * (first_row + row);
        for column in 0..columns {
            let x = rect.x + column as u16;
            let art_column = first_column + column;
            let symbol = match (inked(art_column, top), inked(art_column, top + 1)) {
                (true, true) => FULL_BLOCK,
                (true, false) => UPPER_HALF,
                (false, true) => LOWER_HALF,
                (false, false) => continue,
            };
            let cell = &mut buf[(x, y)];
            cell.set_char(symbol);
            cell.set_style(dim_style());
        }
    }
}

/// Paints the running version into the top-right corner, in the dim watermark
/// tone — chrome in the corner of the page rather than a label competing with
/// the wordmark.
fn render_version(buf: &mut Buffer, area: Rect) {
    let Some(rect) = version_rect(area) else {
        return;
    };
    let label = version_label();
    Paragraph::new(Line::from(Span::styled(label, dim_style())))
        .alignment(Alignment::Right)
        .render(rect, buf);
}

/// The cells the version stamp takes: the right end of the top row, or `None`
/// when the viewport is too narrow to hold the label whole, which loses the
/// corner rather than showing a clipped version. The rain tests read this so
/// they can tell the stamp's chrome from their own shafts.
fn version_rect(area: Rect) -> Option<Rect> {
    let width = u16::try_from(version_label().chars().count()).ok()?;
    (width <= area.width).then(|| Rect::new(area.right() - width, area.y, width, 1))
}

/// Paints the jumbotron into the box the layout reserved for it. `BigText`
/// writes every cell of that box — blank pixels included — so the rain stops at
/// the wordmark rather than showing through the counters.
fn render_wordmark(buf: &mut Buffer, rect: Rect) {
    big_text().render(rect, buf);
}

/// The page's caption, centred one row under the wordmark.
fn render_caption(buf: &mut Buffer, area: Rect, wordmark: Rect, caption: &str) {
    let y = wordmark.bottom() + 1;
    if y >= area.bottom() {
        return;
    }
    let line = Rect::new(area.x, y, area.width, 1);
    Paragraph::new(Line::from(Span::styled(
        caption,
        Style::new().fg(theme::text_muted()),
    )))
    .alignment(Alignment::Center)
    .render(line, buf);
}

/// The dim grey the watermark chrome paints in — the rook's silhouette and the
/// version stamp: present, but never competing with the wordmark above them.
fn dim_style() -> Style {
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

    use ratatui::style::Color;

    /// A caption the scene knows nothing about: the page owns the string, so
    /// the tests pass one in and read it back out.
    const CAPTION: &str = "Press the any key";

    /// What the tests stand the entrance on: a marker on every cell, painted the
    /// way `App::view` paints the session under the screensaver's scene before the
    /// sweep runs. A cell still carrying it is a cell the page has not covered.
    const UNDERLAY: &str = "~";
    const UNDERLAY_STYLE: Style = Style::new().fg(Color::Red).bg(Color::Blue);

    /// A marked page: an empty one carrying the underlay the scene sweeps over.
    fn underlay(area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        for pos in area.positions() {
            buf[pos].set_symbol(UNDERLAY).set_style(UNDERLAY_STYLE);
        }
        buf
    }

    /// A scene long since arrived: the entrance sweep is over, so these tests
    /// read the page it settles on rather than the entrance itself.
    fn scene(width: u16, height: u16, frame: usize) -> Buffer {
        sweep(width, height, frame, ENTRANCE)
    }

    /// The scene `age` into its entrance sweep, over the underlay.
    fn sweep(width: u16, height: u16, frame: usize, age: Duration) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = underlay(area);
        paint(&mut buf, area, frame, age, CAPTION);
        buf
    }

    /// Whether the entrance's edge has reached `(x, y)`. Ahead of it the page has
    /// not painted at all, so the underlay is still standing there untouched.
    fn swept(buf: &Buffer, x: u16, y: u16) -> bool {
        buf[(x, y)].symbol() != UNDERLAY
    }

    /// How far down the page the entrance has carried: the last row it has
    /// reached, as a count of rows from the top.
    fn depth(buf: &Buffer, area: Rect) -> u16 {
        (area.y..area.bottom())
            .filter(|&y| (area.x..area.right()).any(|x| swept(buf, x, y)))
            .count() as u16
    }

    fn row_of(buf: &Buffer, y: u16, width: u16) -> String {
        (0..width).map(|x| buf[(x, y)].symbol()).collect()
    }

    fn is_arrow(buf: &Buffer, x: u16, y: u16) -> bool {
        buf[(x, y)].symbol().starts_with(SHAFT)
    }

    /// Whether `(x, y)` carries the rook: a half block in the dim tone. The
    /// rain paints other cells in other tones, so this reads the silhouette out
    /// of the finished scene.
    fn is_rook(buf: &Buffer, x: u16, y: u16) -> bool {
        let cell = &buf[(x, y)];
        matches!(cell.symbol(), "▀" | "▄" | "█") && cell.style().fg == Some(theme::text_dim())
    }

    /// The rook on a buffer of its own, with nothing else of the scene on it: the
    /// rain and the wordmark paint over and beside the silhouette, and the tests
    /// below are about which of the artwork's pixels land where.
    fn rook_only(width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        if let Some(rook) = rook_rect(area) {
            render_rook(&mut buf, rook);
        }
        buf
    }

    /// Every cell of a rook's box, in no particular order.
    fn rook_cells(rook: Rect) -> impl Iterator<Item = (u16, u16)> {
        (rook.x..rook.right()).flat_map(move |x| (rook.y..rook.bottom()).map(move |y| (x, y)))
    }

    /// The embedded grid is the artwork trimmed to its ink: the shape reaches all
    /// four of its edges, so the corner anchors the silhouette rather than the
    /// clear margin the artwork frames it in.
    #[test]
    fn the_embedded_grid_is_the_artworks_ink_bounds() {
        assert_eq!(ROOK_ART.len(), ROOK_ART_HEIGHT, "the grid's own height");
        for (row, line) in ROOK_ART.iter().enumerate() {
            assert_eq!(line.len(), ROOK_ART_WIDTH, "row {row} is the grid's width");
            assert!(
                line.bytes().all(|pixel| pixel == b'#' || pixel == b'.'),
                "row {row} carries ink or clear pixels: {line:?}"
            );
        }

        // A rook stands taller than it is wide, and every row of the grid
        // carries ink: the grid is the silhouette's bounds rather than a canvas
        // the shape is drawn on.
        let inked_rows = ROOK_ART.iter().filter(|line| line.contains('#')).count();
        assert_eq!(inked_rows, ROOK_ART_HEIGHT, "no row is left clear");
        assert!(
            inked_rows > ROOK_ART_WIDTH,
            "a rook stands taller than it is wide: {ROOK_ART_WIDTH}x{inked_rows}"
        );

        for (edge, inked_edge) in [
            ("top", (0..ROOK_ART_WIDTH).any(|column| inked(column, 0))),
            (
                "bottom",
                (0..ROOK_ART_WIDTH).any(|column| inked(column, ROOK_ART_HEIGHT - 1)),
            ),
            ("left", (0..ROOK_ART_HEIGHT).any(|row| inked(0, row))),
            (
                "right",
                (0..ROOK_ART_HEIGHT).any(|row| inked(ROOK_ART_WIDTH - 1, row)),
            ),
        ] {
            assert!(inked_edge, "the {edge} edge of the trimmed grid is inked");
        }
    }

    /// The rook is a fixed size — the artwork's own pixels, one per cell half —
    /// so a roomier terminal draws the same silhouette in the same corner rather
    /// than a bigger one.
    #[test]
    fn the_rook_is_the_artworks_own_size_whatever_the_viewport() {
        let small = rook_rect(Rect::new(0, 0, 100, 30)).expect("a rook fits a 100x30 terminal");
        let large = rook_rect(Rect::new(0, 0, 200, 50)).expect("a rook fits a 200x50 terminal");

        assert_eq!(
            (small.width, small.height),
            (ROOK_VISIBLE_COLUMNS, ROOK_ROWS),
            "the box is the artwork's own size"
        );
        assert_eq!(
            (large.width, large.height),
            (small.width, small.height),
            "the viewport does not scale it"
        );
        assert_eq!(small, Rect::new(80, 4, ROOK_VISIBLE_COLUMNS, ROOK_ROWS));
        assert_eq!(large, Rect::new(180, 24, ROOK_VISIBLE_COLUMNS, ROOK_ROWS));

        // And its grain is the artwork's aspect: a cell carries two art rows.
        let artwork = f64::from(ROOK_ART_WIDTH as u16) / f64::from(ROOK_ART_HEIGHT as u16);
        let drawn = f64::from(ROOK_COLUMNS) / f64::from(ROOK_ROWS * 2);
        assert!(
            (drawn - artwork).abs() < 0.1,
            "the box draws {drawn} where the artwork is {artwork}"
        );
    }

    /// The rook is pinned to the bottom-right corner with part of its width off
    /// the page: it stands on the bottom row, ends flush with the right edge, and
    /// the columns past that edge are the crop, never painted.
    #[test]
    fn the_rook_hangs_off_the_bottom_right_corner() {
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area).expect("a rook fits a 100x30 terminal");

        assert_eq!(rook.bottom(), area.bottom(), "standing on the bottom row");
        assert_eq!(rook.right(), area.right(), "flush with the right edge");
        assert!(rook.x >= area.x && rook.y >= area.y, "inside the viewport");
        assert!(
            rook.width < ROOK_COLUMNS,
            "part of the rook is off the page"
        );
        assert_eq!(
            rook.width,
            ROOK_COLUMNS - ROOK_COLUMNS * ROOK_CROP_PERCENT / 100,
            "the whole rook less the crop is what shows"
        );
    }

    /// A cramped viewport clips the rook rather than scaling it or dropping it:
    /// the box keeps its footing in the corner and shows the corner of the
    /// artwork it could hold — the base of a short terminal, the right-hand
    /// columns of a narrow one — so the cells it does paint are the very cells a
    /// roomier viewport paints there.
    #[test]
    fn a_cramped_viewport_clips_the_rook_rather_than_scaling_it() {
        let roomy = Rect::new(0, 0, 100, 30);
        let whole = rook_only(roomy.width, roomy.height);
        let rook = rook_rect(roomy).expect("a rook fits a 100x30 terminal");

        // Shorter than the rook: the crown is what goes, so the bottom row of the
        // clipped box is still the artwork's last art row.
        let short = Rect::new(0, 0, 100, 10);
        let clipped = rook_only(short.width, short.height);
        let kept = rook_rect(short).expect("a short viewport keeps the rook's corner");
        assert_eq!(kept.height, short.height, "clipped to the viewport");
        assert_eq!(kept.bottom(), short.bottom(), "and still standing on it");
        assert_eq!(kept.width, rook.width, "with the width the corner holds");
        for row in 0..kept.height {
            for column in 0..kept.width {
                assert_eq!(
                    clipped[(kept.x + column, kept.y + row)].symbol(),
                    whole[(rook.x + column, rook.bottom() - kept.height + row)].symbol(),
                    "({column}, {row}) of the clipped rook"
                );
            }
        }

        // Narrower than the part that shows: the box takes the artwork off the
        // left, so what is drawn is the columns against the right edge.
        let narrow = Rect::new(0, 0, 8, 30);
        let clipped = rook_only(narrow.width, narrow.height);
        let kept = rook_rect(narrow).expect("a narrow viewport keeps the rook's corner");
        assert_eq!(kept.width, narrow.width, "clipped to the viewport");
        assert_eq!(kept.height, ROOK_ROWS, "the crop keeps the height");
        assert_eq!(kept.right(), narrow.right(), "and the right edge");
        for row in 0..kept.height {
            for column in 0..kept.width {
                assert_eq!(
                    clipped[(kept.x + column, kept.y + row)].symbol(),
                    whole[(rook.right() - kept.width + column, rook.y + row)].symbol(),
                    "({column}, {row}) of the clipped rook"
                );
            }
        }

        // Only an empty viewport is left without a corner to stand in.
        assert!(
            rook_rect(Rect::new(0, 0, 0, 30)).is_none(),
            "no columns to stand in"
        );
        assert!(
            rook_rect(Rect::new(0, 0, 80, 0)).is_none(),
            "no rows to stand on"
        );
    }

    /// The rook paints half blocks in the dim tone, and only where the
    /// silhouette reaches: the base spans the box's whole width and the crown
    /// above it leaves the box's left column clear. A filled rectangle would ink
    /// both.
    #[test]
    fn the_rook_paints_a_silhouette_not_a_rectangle() {
        let rook = rook_rect(Rect::new(0, 0, 100, 30)).expect("a rook fits a 100x30 terminal");
        let buf = rook_only(100, 30);

        let edge: Vec<bool> = (rook.y..rook.bottom())
            .map(|y| is_rook(&buf, rook.x, y))
            .collect();
        assert!(
            edge.iter().any(|&inked| inked),
            "the base reaches the box's left edge"
        );
        assert!(
            edge.iter().any(|&inked| !inked),
            "the crown leaves the box's left edge clear"
        );
        assert!(
            inked(0, ROOK_ART_HEIGHT - 1),
            "the artwork inks the grid's left edge at the base"
        );

        // The half blocks are the vertical resolution the rook gets: the
        // artwork's last art row is the upper half of a cell whose lower half
        // falls past the artwork, while the body of the silhouette is full
        // blocks and the cells it leaves go unpainted.
        assert!(
            rook_cells(rook).any(|(x, y)| {
                let symbol = buf[(x, y)].symbol();
                symbol.starts_with(UPPER_HALF) || symbol.starts_with(LOWER_HALF)
            }),
            "the silhouette's edges round through the half blocks"
        );
        assert!(
            rook_cells(rook)
                .any(|(x, y)| is_rook(&buf, x, y) && buf[(x, y)].symbol().starts_with(FULL_BLOCK)),
            "the solid body of the silhouette is painted"
        );
        assert!(
            rook_cells(rook).any(|(x, y)| buf[(x, y)].symbol() == " "),
            "the cells off the silhouette are left to the rain"
        );
    }

    /// The crop cuts the artwork off mid-silhouette rather than squeezing the
    /// whole of it into the room the corner has: the box's right edge lands
    /// inside the artwork — at the shaft's waist, where the artwork is still
    /// inked — so the viewport cuts the rook rather than showing the artwork's
    /// own right edge, which is clear there.
    #[test]
    fn the_crop_cuts_the_artwork_mid_silhouette() {
        let rook = rook_rect(Rect::new(0, 0, 100, 30)).expect("a rook fits a 100x30 terminal");
        let buf = rook_only(100, 30);

        // The waist: the art row carrying the least ink below the crown — the
        // shaft rather than the crown above it or the base below.
        let ink = |row: usize| {
            (0..ROOK_ART_WIDTH)
                .filter(|&column| inked(column, row))
                .count()
        };
        let waist = (ROOK_ART_HEIGHT / 2..ROOK_ART_HEIGHT)
            .min_by_key(|&row| ink(row))
            .expect("rows to weigh");
        let last = ROOK_VISIBLE_COLUMNS as usize - 1;

        assert!(
            inked(last, waist),
            "the artwork is still inked where the viewport cuts it"
        );
        assert!(
            !inked(ROOK_ART_WIDTH - 1, waist),
            "the artwork's own right edge is clear at the waist, so the edge cell \
             would have shown that instead"
        );
        assert!(
            is_rook(&buf, rook.right() - 1, rook.y + (waist / 2) as u16),
            "the edge cell is inked: the crop cuts the silhouette, not the artwork"
        );
    }

    /// The crop is the artwork running off the corner, not a smaller rook: the
    /// cells the viewport holds stay flush to the edge, and the columns past it
    /// are never painted.
    #[test]
    fn the_crop_is_painted_off_the_page() {
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area).expect("a rook fits a 100x30 terminal");
        let buf = rook_only(area.width, area.height);

        let inked_in_last_column = (rook.y..rook.bottom())
            .filter(|&y| is_rook(&buf, rook.right() - 1, y))
            .count();
        assert!(
            inked_in_last_column > 0,
            "the silhouette is cut off by the viewport, not ended short of it"
        );
        assert_eq!(rook.right(), area.right(), "and lands flush with the edge");
        assert_eq!(
            rook.width, ROOK_VISIBLE_COLUMNS,
            "none of the crop reaches the page"
        );
        assert!(
            (ROOK_VISIBLE_COLUMNS as usize..ROOK_ART_WIDTH)
                .any(|column| inked(column, ROOK_ART_HEIGHT - 1)),
            "the artwork carries the base past the edge, so the crop cuts it"
        );
    }

    /// The wordmark paints exactly the box the layout reserves for it — no
    /// wider, or it would run over the rook; no narrower, or `BigText` and the
    /// placement maths would disagree about where the text ends.
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

    /// The wordmark is centred on the viewport, with the caption's row left
    /// under it.
    #[test]
    fn the_wordmark_is_centred_above_its_caption() {
        let area = Rect::new(0, 0, 100, 30);
        let wordmark = placement(area).wordmark;
        assert_eq!(wordmark.x, (area.width - wordmark.width) / 2);
        assert!(
            wordmark.bottom() + 1 < area.bottom(),
            "the caption has a row"
        );
        assert!(
            wordmark.y > area.y,
            "and the block is not pinned to the top"
        );
    }

    /// The running version is stamped in the top-right corner: flush with the
    /// right edge, on the top row alone, in the dim watermark tone rather than
    /// the wordmark's accent.
    #[test]
    fn the_version_stamp_sits_dimmed_in_the_top_right_corner() {
        let area = Rect::new(0, 0, 100, 30);
        let buf = scene(area.width, area.height, 0);
        let label = version_label();
        let stamp = version_rect(area).expect("a 100-wide terminal holds the stamp");

        let top = row_of(&buf, area.y, area.width);
        assert!(
            top.ends_with(&label),
            "the stamp ends flush with the right edge: {top:?}"
        );
        assert_eq!(stamp.right(), area.right(), "flush with the right edge");
        assert_eq!(
            buf[(stamp.x, stamp.y)].style().fg,
            Some(theme::text_dim()),
            "the stamp paints in the dim watermark tone"
        );
        // One row of chrome, not a watermark across the page: nothing below the
        // top row carries the version.
        for y in area.y + 1..area.bottom() {
            let row = row_of(&buf, y, area.width);
            assert!(!row.contains(&label), "row {y} carries the stamp: {row:?}");
        }
    }

    /// A viewport too narrow to hold the stamp whole loses the corner rather
    /// than showing a clipped version, and exactly the stamp's own width is
    /// where it comes back.
    #[test]
    fn a_cramped_terminal_drops_the_version_stamp() {
        let label = version_label();
        let width = label.chars().count() as u16;
        assert!(width > 1, "the label is worth a width boundary");

        let buf = scene(width - 1, 24, 0);
        for y in 0..24 {
            let row = row_of(&buf, y, width - 1);
            assert!(!row.contains(&label), "row {y} carries the stamp: {row:?}");
        }
        assert!(
            row_of(&scene(width, 24, 0), 0, width).ends_with(&label),
            "a viewport exactly the stamp's width still has it"
        );
    }

    /// Where a short terminal brings the stamp and the wordmark onto the same
    /// row, the wordmark's opaque box is what wins: the stamp is chrome, the
    /// title is the subject.
    #[test]
    fn the_wordmark_outranks_the_version_stamp() {
        let area = Rect::new(0, 0, 60, 10);
        let buf = scene(area.width, area.height, 0);
        let wordmark = placement(area).wordmark;
        let stamp = version_label().chars().count() as u16;

        assert_eq!(
            wordmark.y, area.y,
            "the terminal is short enough to collide"
        );
        assert!(
            wordmark.right() + stamp > area.right(),
            "and the two want the same corner"
        );
        assert_eq!(
            buf[(wordmark.right() - 1, area.y)].style().fg,
            Some(theme::accent()),
            "the wordmark owns the cell the stamp would have used"
        );
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
        assert!(
            rook_cells(rook).any(|(x, y)| is_rook(&buf, x, y)),
            "the rook is painted into its corner"
        );

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

    /// The page arrives by sweeping in down the screen rather than appearing
    /// whole, the top of it first.
    #[test]
    fn the_scene_sweeps_in_from_the_top() {
        let area = Rect::new(0, 0, 100, 30);
        let total = area.width as usize * area.height as usize;
        let at = |step: u32| sweep(area.width, area.height, 0, ENTRANCE * step / 10);

        assert_eq!(depth(&at(0), area), 0, "nothing has been painted yet");
        assert_eq!(
            depth(&at(10), area),
            area.height,
            "a finished sweep leaves no row behind"
        );

        // The page fills in as the edge carries down it, never back up.
        let counts: Vec<usize> = (0..=10)
            .map(|step| {
                let buf = at(step);
                (area.y..area.bottom())
                    .flat_map(|y| (area.x..area.right()).map(move |x| (x, y)))
                    .filter(|(x, y)| swept(&buf, *x, *y))
                    .count()
            })
            .collect();
        assert!(
            counts.windows(2).all(|step| step[0] <= step[1]),
            "the reveal never retreats: {counts:?}"
        );
        assert_eq!(counts[0], 0, "nothing has arrived yet: {counts:?}");
        assert_eq!(total, counts[10], "the page has all arrived: {counts:?}");
        assert!(
            counts[1] > 0 && counts[1] < total,
            "the reveal is progressive rather than all at once: {counts:?}"
        );

        // And it is the top of the page that goes first.
        let early = at(1);
        assert!(
            (area.x..area.right()).any(|x| swept(&early, x, area.y)),
            "the sweep has reached the top of the page"
        );
        assert!(
            !(area.x..area.right()).any(|x| swept(&early, x, area.bottom() - 1)),
            "and not the bottom of it"
        );
    }

    /// The whole page sweeps in, its dark plate with it: ahead of the edge nothing
    /// has been painted, so the screen the page is covering still stands there —
    /// rather than the plate landing at once and the scene materialising on it.
    #[test]
    fn the_plate_sweeps_in_with_the_page() {
        let area = Rect::new(0, 0, 100, 30);

        assert_eq!(
            depth(&sweep(area.width, area.height, 0, Duration::ZERO), area),
            0,
            "an entrance that has just started leaves the screen it covers alone"
        );

        // A tenth of the way in: the page has taken the top of the screen, and the
        // screen it is covering still has the bottom of it, untouched.
        let mid = sweep(area.width, area.height, 0, ENTRANCE / 10);
        assert!(depth(&mid, area) > 0, "the page has covered the top row");
        assert!(
            depth(&mid, area) < area.height,
            "and has not reached the bottom"
        );
        assert!(
            (area.x..area.right()).all(|x| !swept(&mid, x, area.bottom() - 1)),
            "leaving the bottom row to the screen underneath"
        );
        assert_eq!(
            mid[(area.x, area.y)].bg,
            theme::bg(),
            "what arrives over it is the page's own plate"
        );

        // Once it has landed there is no underlay left anywhere: the page is
        // opaque, which is what lets the screensaver stop painting the session.
        let arrived = sweep(area.width, area.height, 0, ENTRANCE);
        assert!(
            area.positions().all(|pos| swept(&arrived, pos.x, pos.y)),
            "a settled page covers the screen whole"
        );
    }

    /// Nothing in the page is drawn in the plate's own colour. That is what lets
    /// [`paint`] read the entrance's edge back off the built page: a cell carrying
    /// the plate on both channels is a cell the edge has not reached.
    #[test]
    fn the_plate_is_never_painted_as_ink() {
        for (width, height) in [(80, 24), (100, 30), (40, 12)] {
            let area = Rect::new(0, 0, width, height);
            for frame in 0..8 {
                let buf = scene(width, height, frame);
                for pos in area.positions() {
                    let cell = &buf[pos];
                    assert!(
                        cell.fg != theme::bg() || cell.bg != theme::bg(),
                        "frame {frame} on {width}x{height} draws {:?} at {pos:?} in the plate's own colour",
                        cell.symbol()
                    );
                }
            }
        }
    }

    /// The entrance is a pure function of the scene's clocks, which is what lets
    /// it be rebuilt on every paint instead of being kept alive between frames:
    /// the same clock draws the same page, sweep and all. What makes that work is
    /// `sweep_in` drawing its jitter from a fixed seed.
    #[test]
    fn the_entrance_is_a_pure_function_of_its_clock() {
        for age in [
            Duration::ZERO,
            ENTRANCE / 3,
            ENTRANCE / 2,
            ENTRANCE - Duration::from_millis(1),
        ] {
            assert_eq!(
                sweep(80, 24, 7, age).content,
                sweep(80, 24, 7, age).content,
                "the scene {age:?} into its entrance redraws identically"
            );
        }
        assert_ne!(
            sweep(80, 24, 7, Duration::ZERO).content,
            sweep(80, 24, 7, ENTRANCE / 2).content,
            "the sweep is what moves the page between the two"
        );
    }

    /// While the sweep runs the render loop is paced by the entrance rather than
    /// by the rain, so the gradient crosses the page in more than the eleven
    /// steps the rain's twelve frames a second would give it.
    #[test]
    fn the_entrance_sets_the_pace_until_the_scene_has_arrived() {
        assert!(
            ENTRANCE_FRAME < Duration::from_millis(1_000 / RAIN_RATE as u64),
            "the entrance has to be the faster of the two clocks to pace anything"
        );

        for age in [Duration::ZERO, Duration::from_millis(1), ENTRANCE / 2] {
            let wake = entrance_wake(age).expect("the sweep is still running");
            assert!(
                wake > Duration::ZERO && wake <= ENTRANCE_FRAME,
                "{age:?} into the sweep waits {wake:?}"
            );
        }
        assert!(
            entrance_wake(ENTRANCE).is_none(),
            "an arrived scene has no entrance frame left to wait for"
        );

        // The loop is handed the entrance's pace, and the rain still drives it
        // once the scene has arrived.
        let wake = next_wake(Duration::ZERO).expect("the scene animates");
        assert!(wake <= ENTRANCE_FRAME, "woken in {wake:?}");
        assert!(
            next_wake(ENTRANCE).is_some(),
            "the rain keeps animating behind a settled scene"
        );
    }

    /// The rain animates across the whole background: every column is rained on
    /// in the sky, where nothing else paints over it.
    #[test]
    fn the_rain_sweeps_every_column() {
        let area = Rect::new(0, 0, 80, 30);
        let above = sky(area);
        let raining: Vec<Vec<bool>> = (0..60)
            .map(|frame| {
                let buf = scene(area.width, area.height, frame);
                (0..area.width)
                    .map(|column| (0..above).any(|row| is_arrow(&buf, column, row)))
                    .collect()
            })
            .collect();
        for column in 0..area.width {
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
        let top = sky(area);
        for frame in 0..40 {
            // Tips are followed only while their successor — a step down — stays
            // inside the sky, the band above the rook where the rain is the only
            // thing painting.
            let before = tips(&scene(area.width, area.height, frame), top - 2);
            let after = tips(&scene(area.width, area.height, frame + 1), top);
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

        // Read the streaks off the sky — the band above the rook, where the
        // rain is the only thing painting.
        let area = Rect::new(0, 0, 80, 30);
        let buf = scene(area.width, area.height, 0);
        let stamp = version_rect(area);
        let mut tails = 0;
        for (x, y) in tips(&buf, sky(area)) {
            for step in 1..TRAIL as i32 {
                let (tail_x, tail_y) = tail_cell((x, y), step);
                let (Ok(tail_x), Ok(tail_y)) = (u16::try_from(tail_x), u16::try_from(tail_y))
                else {
                    break; // the rest of the shaft has run off the page
                };
                if stamp.is_some_and(|stamp| stamp.contains(Position::new(tail_x, tail_y))) {
                    continue; // the version stamp is chrome over the rain
                }
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

    /// The rows above the rook: the band of the page where the rain is the only
    /// thing painting. The rook is a fixed size, so a viewport that leaves little
    /// room above it leaves the rain little sky to be read in.
    fn sky(area: Rect) -> u16 {
        rook_rect(area).map_or(area.height, |rook| rook.y)
    }

    /// The tips of the streaks — the bright end of every shaft — among the
    /// `rows` at the top of the page.
    fn tips(buf: &Buffer, rows: u16) -> Vec<(i32, i32)> {
        (0..rows)
            .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                is_arrow(buf, x, y) && !buf[(x, y)].style().add_modifier.contains(Modifier::DIM)
            })
            .map(|(x, y)| (i32::from(x), i32::from(y)))
            .collect()
    }
}
