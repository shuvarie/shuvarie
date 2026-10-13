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
//! the binary and painted as half blocks in the theme's dim tone. Its placement
//! is derived from the viewport alone, so the scene keeps no geometry of its
//! own: it is pinned to the bottom-right corner and simply drops out when the
//! terminal is too small to hold the part that shows. The rain advances off
//! wall-clock time ([`FrameIndexer`]), so [`view`] paints whichever frame the
//! clock has selected and [`next_wake`] reports when the render loop should
//! come back for the next one.
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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use tachyonfx::{Effect, EffectTimer, Interpolation, Motion, fx};
use tui_big_text::{BigText, PixelSize};

use crate::tui::components::version_label;
use crate::tui::theme;

/// The rook artwork, vendored from the `shuvarie-artwork` repository's
/// `png/rook.png` and embedded, so the binary carries the scene and the paint
/// path never touches the filesystem.
const ROOK_PNG: &[u8] = include_bytes!("../../../assets/rook.png");

/// How tall the rook stands, as a percentage of the viewport's rows.
const ROOK_HEIGHT_PERCENT: u16 = 75;
/// How much of the rook's width hangs off the right edge — the crop that pins
/// the silhouette to the corner instead of drawing it whole beside the text.
const ROOK_CROP_PERCENT: u16 = 40;
/// The shortest the rook is ever drawn. Below this the silhouette reads as a
/// smudge, so the viewport is left with an empty corner instead.
const ROOK_MIN_ROWS: u16 = 6;

/// The half-block cells the rook paints with: one sub-pixel of the artwork per
/// cell half, so a row of cells carries two rows of the silhouette.
const UPPER_HALF: char = '▀';
const LOWER_HALF: char = '▄';
const FULL_BLOCK: char = '█';
/// How much of a sub-pixel the silhouette has to cover to be inked. The
/// artwork is a hard-edged mask, so this only rounds the box filter's partial
/// edges.
const INK: u8 = 128;

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
fn entrance(area: Rect) -> Effect {
    let tear = (area.height * ENTRANCE_TEAR_PERCENT / 100).max(1);
    fx::sweep_in(
        Motion::UpToDown,
        ENTRANCE_GRADIENT,
        tear,
        theme::bg(),
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
    let mut page = Buffer::empty(area);
    paint_page(&mut page, area, rain_frame, caption);
    entrance(area).process(age, &mut page, area);

    for pos in area.positions() {
        let painted = &page[pos];
        // `sweep_in` writes the faded colour to *both* channels of every cell
        // ahead of its edge and leaves the rest of the page alone, so a cell
        // carrying the plate on both is one the edge has not reached. Nothing in
        // the page is ever drawn in the plate's own colour, which is what makes
        // that readable — `the_plate_is_never_painted_as_ink` holds the scene to
        // it.
        if painted.fg == theme::bg() && painted.bg == theme::bg() {
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

    let mask = rook_mask();
    let placement = placement(area, mask);

    render_rain(buf, area, rain_frame);
    if let (Some(rook), Some(mask)) = (placement.rook, mask) {
        render_rook(buf, area, &rook, mask);
    }
    // The version is chrome and the wordmark is the subject, so the stamp goes
    // down first: on a terminal short enough to bring the two onto the same
    // row, `BigText`'s opaque box is what should win.
    render_version(buf, area);
    render_wordmark(buf, placement.wordmark);
    render_caption(buf, area, placement.wordmark, caption);
}

/// The rook's silhouette: the embedded artwork's alpha, cropped to the pixels
/// that carry ink and kept as per-pixel coverage.
struct Mask {
    width: usize,
    height: usize,
    /// One coverage byte per art pixel, row-major — 0 clear, 255 solid.
    coverage: Vec<u8>,
}

impl Mask {
    /// Decodes [`ROOK_PNG`] and trims the transparent margin away from the
    /// shape. `None` when the artwork will not decode or carries no ink at all,
    /// which drops the rook rather than failing the paint.
    fn decode() -> Option<Self> {
        let image = image::ImageReader::new(std::io::Cursor::new(ROOK_PNG))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?
            .into_rgba8();

        // The shape's own bounds: the artwork frames the rook in clear pixels,
        // and it is the silhouette — not the frame — the corner anchors.
        let (mut left, mut top) = (image.width(), image.height());
        let (mut right, mut bottom) = (0, 0);
        for (x, y, pixel) in image.enumerate_pixels() {
            if pixel.0[3] < INK {
                continue;
            }
            left = left.min(x);
            top = top.min(y);
            right = right.max(x);
            bottom = bottom.max(y);
        }
        if left > right || top > bottom {
            return None;
        }

        let (width, height) = (right - left + 1, bottom - top + 1);
        let mut coverage = Vec::with_capacity((width * height) as usize);
        for y in top..=bottom {
            for x in left..=right {
                coverage.push(image.get_pixel(x, y).0[3]);
            }
        }
        Some(Self {
            width: width as usize,
            height: height as usize,
            coverage,
        })
    }

    /// The coverage a sub-pixel of a `columns` x `rows` box of cells stands for,
    /// box-filtered down from the artwork's own resolution. A cell is two
    /// sub-pixels tall, so the sub-pixel grid is `columns` x `2 * rows`.
    fn sample(&self, column: usize, sub_row: usize, columns: usize, rows: usize) -> u8 {
        let (x0, x1) = part_range(column, columns, self.width);
        let (y0, y1) = part_range(sub_row, rows * 2, self.height);
        let mut total = 0u32;
        let mut count = 0u32;
        for y in y0..y1 {
            for x in x0..x1 {
                total += u32::from(self.coverage[y * self.width + x]);
                count += 1;
            }
        }
        u8::try_from(total / count.max(1)).unwrap_or(u8::MAX)
    }
}

/// The artwork pixels one part of a `parts`-long axis stands for: its share of
/// `source`, never empty, so a sub-pixel always has something to average.
fn part_range(part: usize, parts: usize, source: usize) -> (usize, usize) {
    let start = part * source / parts;
    let end = ((part + 1) * source / parts).max(start + 1).min(source);
    (start, end)
}

/// The rook's silhouette, decoded from the embedded artwork on first use — and
/// only once, failures included.
fn rook_mask() -> Option<&'static Mask> {
    static MASK: OnceLock<Option<Mask>> = OnceLock::new();
    MASK.get_or_init(Mask::decode).as_ref()
}

/// Where the scene's pieces land on a given viewport.
struct Placement {
    /// The wordmark's box: the cells the big text paints into.
    wordmark: Rect,
    /// The rook's box, `None` when the viewport cannot hold it.
    rook: Option<Rook>,
}

/// The rook's landing: the cells it paints, and how much of its own width is
/// off the page behind them.
struct Rook {
    /// The cells inside the viewport, flush with the bottom-right corner.
    rect: Rect,
    /// The artwork columns hidden past the right edge, never painted: the crop
    /// that reads as the silhouette running off the corner.
    hidden: u16,
}

/// Lays the scene out from the viewport alone: the wordmark takes the centred
/// row, and the rook hangs off the bottom-right corner beside it.
fn placement(area: Rect, mask: Option<&Mask>) -> Placement {
    Placement {
        wordmark: wordmark_rect(area),
        rook: mask.and_then(|mask| rook_rect(area, mask)),
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

/// The rook's width in cells at a given height: the artwork's aspect, carried
/// over the half blocks' grain of two sub-pixel rows per cell. Without it the
/// silhouette would be squashed to whatever shape the terminal happens to have.
fn rook_columns(mask: &Mask, rows: u16) -> u16 {
    let sub_rows = u64::from(rows) * 2;
    let columns = (sub_rows * mask.width as u64).div_ceil(mask.height as u64);
    u16::try_from(columns).unwrap_or(u16::MAX)
}

/// The rook's box: [`ROOK_HEIGHT_PERCENT`] of the viewport tall, standing on
/// the bottom row and flush with the right edge, with
/// [`ROOK_CROP_PERCENT`] of its width running off that edge. `None` when the
/// viewport cannot hold the part that shows.
fn rook_rect(area: Rect, mask: &Mask) -> Option<Rook> {
    if area.height < ROOK_MIN_ROWS {
        return None;
    }
    let rows = u16::try_from(usize::from(area.height) * usize::from(ROOK_HEIGHT_PERCENT) / 100)
        .ok()?
        .max(ROOK_MIN_ROWS);
    let columns = rook_columns(mask, rows);
    let hidden = columns * ROOK_CROP_PERCENT / 100;
    let visible = columns - hidden;
    // The silhouette needs its footing on screen: a viewport too narrow for
    // what would show gets no rook rather than a strip of one.
    if visible > area.width {
        return None;
    }
    let y = area.bottom().checked_sub(rows)?;
    Some(Rook {
        rect: Rect::new(area.x + area.width - visible, y, visible, rows),
        hidden,
    })
}

/// Paints the rook into `rook.rect` as half blocks in the dim silhouette tone.
/// The cells the silhouette does not reach are left alone, so the rain keeps
/// falling behind it and the rook reads as a watermark rather than a filled
/// rectangle.
///
/// The artwork is sampled across its *whole* width — the visible cells plus the
/// crop — so the columns past the right edge are what goes unseen, leaving the
/// silhouette cut off by the viewport rather than squashed into it.
fn render_rook(buf: &mut Buffer, area: Rect, rook: &Rook, mask: &Mask) {
    let visible = usize::from(rook.rect.width);
    let rows = usize::from(rook.rect.height);
    let columns = visible + usize::from(rook.hidden);
    if visible == 0 || rows == 0 || columns == 0 {
        return;
    }
    for row in 0..rows {
        let y = rook.rect.y + row as u16;
        if y >= area.bottom() {
            break;
        }
        for column in 0..visible {
            let x = rook.rect.x + column as u16;
            if x >= area.right() {
                break;
            }
            let top = mask.sample(column, row * 2, columns, rows);
            let bottom = mask.sample(column, row * 2 + 1, columns, rows);
            let symbol = match (top >= INK, bottom >= INK) {
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

    /// The embedded artwork decodes into a mask trimmed to its ink: the shape
    /// reaches all four of its edges, so the corner anchors the silhouette
    /// rather than the clear margin the artwork frames it in.
    #[test]
    fn the_embedded_artwork_decodes_into_a_trimmed_mask() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        assert!(mask.width > 0 && mask.height > 0);
        assert!(
            mask.height > mask.width,
            "a rook stands taller than it is wide: {}x{}",
            mask.width,
            mask.height
        );
        assert_eq!(
            mask.coverage.len(),
            mask.width * mask.height,
            "one coverage byte per art pixel"
        );

        let at = |x: usize, y: usize| mask.coverage[y * mask.width + x];
        for (edge, inked) in [
            ("top", (0..mask.width).any(|x| at(x, 0) >= INK)),
            (
                "bottom",
                (0..mask.width).any(|x| at(x, mask.height - 1) >= INK),
            ),
            ("left", (0..mask.height).any(|y| at(0, y) >= INK)),
            (
                "right",
                (0..mask.height).any(|y| at(mask.width - 1, y) >= INK),
            ),
        ] {
            assert!(inked, "the {edge} edge of the trimmed mask is inked");
        }
    }

    /// The half blocks carry two sub-pixel rows per cell, so the box the layout
    /// reserves reproduces the artwork's aspect instead of squashing the
    /// silhouette into whatever shape the terminal happens to have.
    #[test]
    fn the_rook_box_keeps_the_artworks_aspect() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        let artwork = mask.width as f64 / mask.height as f64;
        for rows in [ROOK_MIN_ROWS, 12, 22] {
            let columns = rook_columns(mask, rows);
            let drawn = f64::from(columns) / f64::from(rows * 2);
            assert!(
                (drawn - artwork).abs() < 0.1,
                "at {rows} rows the box is {drawn} where the artwork is {artwork}"
            );
        }
    }

    /// The rook is pinned to the bottom-right corner with part of its width off
    /// the page: it stands on the bottom row, ends flush with the right edge,
    /// and the columns past that edge are the crop, never painted.
    #[test]
    fn the_rook_hangs_off_the_bottom_right_corner() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area, mask).expect("a rook fits a 100x30 terminal");

        assert_eq!(
            rook.rect.bottom(),
            area.bottom(),
            "standing on the bottom row"
        );
        assert_eq!(rook.rect.right(), area.right(), "flush with the right edge");
        assert_eq!(rook.rect.height, 30 * ROOK_HEIGHT_PERCENT / 100);
        assert!(rook.hidden > 0, "part of the rook is off the page");
        assert_eq!(
            rook.rect.width + rook.hidden,
            rook_columns(mask, rook.rect.height),
            "the visible cells plus the crop are the whole rook"
        );
        assert_eq!(
            rook.hidden,
            rook_columns(mask, rook.rect.height) * ROOK_CROP_PERCENT / 100
        );
    }

    /// A viewport too short or too narrow for the part that shows drops the rook
    /// rather than painting a smudge or a sliver of it over the text.
    #[test]
    fn a_cramped_terminal_drops_the_rook() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        assert!(
            rook_rect(Rect::new(0, 0, 80, ROOK_MIN_ROWS - 1), mask).is_none(),
            "too few rows to read"
        );
        assert!(
            rook_rect(Rect::new(0, 0, 8, 24), mask).is_none(),
            "the footing would not fit the width"
        );
        assert!(
            rook_rect(Rect::new(0, 0, 100, 30), mask).is_some(),
            "a roomy terminal keeps the rook"
        );
    }

    /// The rook paints half blocks in the dim tone, and only where the
    /// silhouette reaches: the crown is wide and the shaft below it narrow, so
    /// the box's own left edge is inked beside the crown and clear beside the
    /// shaft. A filled rectangle would ink both.
    #[test]
    fn the_rook_paints_a_silhouette_not_a_rectangle() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area, mask).expect("a rook fits a 100x30 terminal");
        let buf = scene(area.width, area.height, 0);

        let edge: Vec<bool> = (rook.rect.y..rook.rect.bottom())
            .map(|y| is_rook(&buf, rook.rect.x, y))
            .collect();
        assert!(
            edge.iter().any(|&inked| inked),
            "the crown reaches the box's left edge"
        );
        assert!(
            edge.iter().any(|&inked| !inked),
            "the shaft leaves the box's left edge clear"
        );

        // Both halves of the grain are in use somewhere on the silhouette: an
        // edge landing on a cell boundary leaves a cell with only one of its
        // halves inked, which is the resolution the half blocks buy.
        assert!(
            (rook.rect.x..rook.rect.right())
                .flat_map(|x| (rook.rect.y..rook.rect.bottom()).map(move |y| (x, y)))
                .any(|(x, y)| {
                    let symbol = buf[(x, y)].symbol();
                    is_rook(&buf, x, y)
                        && (symbol.starts_with(UPPER_HALF) || symbol.starts_with(LOWER_HALF))
                }),
            "the silhouette's edges round through the half blocks"
        );
        assert!(
            (rook.rect.x..rook.rect.right())
                .flat_map(|x| (rook.rect.y..rook.rect.bottom()).map(move |y| (x, y)))
                .any(|(x, y)| is_rook(&buf, x, y) && buf[(x, y)].symbol().starts_with(FULL_BLOCK)),
            "the solid body of the silhouette is painted"
        );
    }

    /// The crop cuts the artwork off mid-silhouette rather than squeezing the
    /// whole of it into the room left over: the box's right edge lands where the
    /// rook is at its narrowest — inside the shaft — and the silhouette is still
    /// inked against the viewport edge there. A render that squeezed the artwork
    /// into the box would show the artwork's own right edge in that cell, which
    /// is clear at the waist.
    #[test]
    fn the_crop_cuts_the_artwork_mid_silhouette() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area, mask).expect("a rook fits a 100x30 terminal");
        let buf = scene(area.width, area.height, 0);

        // The waist: the artwork row carrying the least ink — the shaft rather
        // than the crown above it or the base below.
        let ink = |y: usize| {
            (0..mask.width)
                .filter(|&x| mask.coverage[y * mask.width + x] >= INK)
                .count()
        };
        let waist = (0..mask.height)
            .min_by_key(|&y| ink(y))
            .expect("rows to weigh");
        let row = rook.rect.y + (waist * usize::from(rook.rect.height) / mask.height) as u16;

        assert!(
            is_rook(&buf, rook.rect.right() - 1, row),
            "the viewport cuts the rook at its waist, so the edge cell is inked"
        );
        assert!(
            mask.coverage[waist * mask.width + mask.width - 1] < INK,
            "the artwork's own right edge is clear at the waist, so the cell above \
             would have shown that instead"
        );
    }

    /// The crop is the artwork running off the corner, not a smaller rook: the
    /// cells inside the viewport stay flush to the edge, and nothing is painted
    /// past it.
    #[test]
    fn the_crop_is_painted_off_the_page() {
        let mask = rook_mask().expect("the embedded artwork decodes");
        let area = Rect::new(0, 0, 100, 30);
        let rook = rook_rect(area, mask).expect("a rook fits a 100x30 terminal");
        let buf = scene(area.width, area.height, 0);

        let inked_in_last_column = (rook.rect.y..rook.rect.bottom())
            .filter(|&y| is_rook(&buf, rook.rect.right() - 1, y))
            .count();
        assert!(
            inked_in_last_column > 0,
            "the silhouette is cut off by the viewport, not ended short of it"
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
        let wordmark = placement(area, rook_mask()).wordmark;
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
        let wordmark = placement(area, rook_mask()).wordmark;
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
        let placement = placement(area, rook_mask());
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
            (rook.rect.x..rook.rect.right())
                .flat_map(|x| (rook.rect.y..rook.rect.bottom()).map(move |y| (x, y)))
                .any(|(x, y)| is_rook(&buf, x, y)),
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
    /// thing painting, whatever size the artwork takes on a given viewport.
    fn sky(area: Rect) -> u16 {
        placement(area, rook_mask())
            .rook
            .map_or(area.height, |rook| rook.rect.y)
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
