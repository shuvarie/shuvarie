//! The storm the screensaver plays over the page it has settled on: every few
//! seconds the sky glimmers and a bolt cracks down through it.
//!
//! It is painted *over* the finished page rather than into it. The page is the
//! artwork onboarding shares and the storm is the screensaver's own, so
//! onboarding's title page stays the quiet rain it was.
//!
//! What a strike does to the page is a **slight** wash: every cell's background is
//! lifted a little way toward the page's own ink, so the rain's arrows, the rook and
//! the wordmark keep their colours and read as silhouettes against a sky that has
//! only just stirred — never against a glare that repaints them. The bolt is the one
//! thing that writes glyphs. Neither touches a cell's symbol or foreground, so the
//! storm adds light and a channel to the page rather than drawing a new one.
//!
//! The one thing the bolt keeps out of is the page's text — [`art::text_block`], the
//! wordmark's box and the caption's row — so a strike breaks around the title
//! rather than eating it.
//!
//! Like the scene, the storm is a pure function of one clock: the age the
//! screensaver reports for the page, so raising the scene again restarts the storm
//! with it and nothing has to be kept between frames. [`next_wake`] hands the
//! render loop the next frame the storm needs; while the sky is quiet that is the
//! start of the next strike, which comes later than the rain's own frame, so a
//! quiet storm costs the loop nothing.
//!
//! The rhythm is a slot per strike, each held back by a jitter taken from its
//! slot's hash: irregular to watch, while the slots keep two strikes from ever
//! overlapping, so there is one bolt on the page at a time. Within its slot a strike
//! flickers rather than fades — [`STROKE`] says frame by frame whether the bolt is on
//! the page, as the return stroke, the channel going dark while it discharges and a
//! weaker second stroke follow one another — because lightning is seen in blows and
//! not in a fade.
//!
//! The bolt itself is drawn from the strike's seed rather than stored: it enters on
//! the top row, walks down a row at a time, steps a column now and then and forks
//! once on its way down, so every frame of a strike carries the same bolt the way
//! every frame of the rain carries the same arrows.

use std::time::Duration;

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};

use crate::tui::theme;
use crate::tui::welcome::art;

/// A strike's brightness, one entry per frame, as a percentage of the flash's full
/// strength: the return stroke, the sky dimming while the channel discharges, a
/// weaker second stroke following it, then the afterglow decaying to nothing. The
/// dips between the strokes are what make a strike read as lightning rather than as
/// a fade.
const STROKE: [u8; 9] = [100, 74, 15, 58, 92, 38, 20, 9, 3];

/// How long each of the strike's frames is held. Thirty frames a second: the
/// strokes land close enough together to read as one blow, and anything slower
/// reads as a fade rather than as lightning.
const STROKE_FRAME_MS: u64 = 33;

/// How long a strike occupies, from its first stroke to the last of its afterglow.
const STRIKE: Duration = Duration::from_millis(STROKE_FRAME_MS * STROKE.len() as u64);

/// The storm's rhythm: at most one strike per slot, the slot being this long.
const STRIKE_PERIOD_MS: u64 = 5_000;

/// How far into its slot a strike may land. The floor holds the opening strike
/// clear of the page's arrival — the scene comes up in the rain and is struck a
/// beat later — and the spread to the ceiling is what keeps the rhythm from
/// ticking like a metronome.
const STRIKE_EARLIEST_MS: u64 = 1_500;
const STRIKE_LATEST_MS: u64 = 3_500;

/// The share of the way from the plate to the page's ink that the flash lifts the sky
/// at its strongest. Deliberately low — roughly an eighth of the way — because this is
/// chrome behind a scene somebody is watching, not a light source: the strike should be
/// felt as a glimmer across the page, and the bolt, not the wash, is what is meant to be
/// seen. `the_flash_is_slight_and_leaves_the_pages_ink_alone` holds it to that.
const WASH: f32 = 0.12;

/// The dimmest frame of a strike that still shows the bolt. The channel is on the page
/// for the strokes and gone in the dips between them, so what flickers is the bolt.
const BOLT: u8 = 40;

/// The shortest a bolt can be, as a percentage of the page's height: the run from
/// the top row to where it lands. A storm whose every bolt reached the bottom row
/// would read as wallpaper; a short strike dying in the sky is what makes a long
/// one land.
const BOLT_DEPTH_MIN_PERCENT: u16 = 40;

/// The fork a bolt throws off on its way down: how far along the run it splits, as
/// a percentage of the run's length, and how many rows the fork itself covers.
const BOLT_FORK_PERCENT: u16 = 62;
const BOLT_FORK_ROWS: u16 = 6;

/// The channel's glyphs: the vertical it holds and the two diagonals it climbs
/// when it steps sideways. Box drawing rather than the rain's `\`, so the bolt is
/// never mistaken for a shaft of rain.
const VERTICAL: char = '│';
const LEANS_LEFT: char = '╱';
const LEANS_RIGHT: char = '╲';

/// Paints the storm over the page in `frame`, taking its clock from the page's
/// `age`.
pub fn view(frame: &mut Frame<'_>, area: Rect, age: Duration) {
    paint(frame.buffer_mut(), area, age);
}

/// Buffer-level [`view`] with the clock explicit, so tests paint against a bare
/// [`Buffer`] and pick the frame they assert on.
pub fn paint(buf: &mut Buffer, area: Rect, age: Duration) {
    if area.is_empty() {
        return;
    }
    let Some((seed, elapsed)) = strike(age) else {
        return;
    };
    let strength = stroke(elapsed);
    wash(buf, area, strength);
    if strength >= BOLT {
        bolt(buf, area, seed);
    }
}

/// How long until the storm's next frame. While a strike burns that is its next
/// stroke, a frame of thirty a second; a quiet sky waits for the next strike to
/// land, which is further off than the rain's own frame — so the storm adds
/// nothing to the loop's work between strikes.
pub fn next_wake(age: Duration) -> Option<Duration> {
    match strike(age) {
        Some((_, elapsed)) => {
            // `Duration` has no remainder, so the phase within the stroke is taken
            // in whole milliseconds — the same way the entrance's frames are.
            let phase = elapsed.as_millis() as u64 % STROKE_FRAME_MS;
            Some(Duration::from_millis(STROKE_FRAME_MS - phase))
        }
        // A wake of zero would spin the loop rather than wait, so a storm that
        // cannot say how long it has left says nothing and leaves the loop to the
        // rain.
        None => next_strike(age)
            .checked_sub(age)
            .filter(|wake| *wake > Duration::ZERO),
    }
}

/// When the storm next strikes after `age`: this slot's strike if it is still to
/// come, the next slot's otherwise.
fn next_strike(age: Duration) -> Duration {
    let now = slot(age);
    let this = strike_at(now);
    if this > age { this } else { strike_at(now + 1) }
}

/// The strike alive `age` into the page, if there is one: the seed its bolt is
/// drawn from, and how far into it the strike has burned.
fn strike(age: Duration) -> Option<(u32, Duration)> {
    // A strike outlives the start of its own slot by less than the jitter can hold
    // one back, so the only two slots that can be burning at `age` are this one and
    // the one before it.
    let now = slot(age);
    std::iter::once(now)
        .chain(now.checked_sub(1))
        .find_map(|index| {
            let start = strike_at(index);
            let elapsed = age.checked_sub(start)?;
            (elapsed < STRIKE).then(|| (seed(start), elapsed))
        })
}

/// Which slot of the storm's rhythm `age` falls in.
fn slot(age: Duration) -> u64 {
    (age.as_millis() as u64) / STRIKE_PERIOD_MS
}

/// When the strike sharing `index`'s slot lands: its slot, held back by a jitter of
/// its own so that no two strikes are spaced the same way.
pub(crate) fn strike_at(index: u64) -> Duration {
    let span = (STRIKE_LATEST_MS - STRIKE_EARLIEST_MS) as u32;
    let jitter = u64::from(hash(index as u32) % span);
    Duration::from_millis(index.saturating_mul(STRIKE_PERIOD_MS) + STRIKE_EARLIEST_MS + jitter)
}

/// The seed a strike's bolt is drawn from: the hash of the moment it lands, so no
/// two strikes throw the same bolt.
fn seed(start: Duration) -> u32 {
    hash(start.as_millis() as u32)
}

/// The strike's brightness `elapsed` into it: [`STROKE`] read a frame at a time. Only
/// called while the strike burns, so the frame is always one of the table's.
fn stroke(elapsed: Duration) -> u8 {
    let frame = elapsed.as_millis() as u64 / STROKE_FRAME_MS;
    STROKE[(frame as usize).min(STROKE.len() - 1)]
}

/// Lifts the page toward the flash: every cell in `area` brightened by `strength` of
/// the way to the glare, its symbol and ink untouched. A slight lift at the strongest
/// frame ([`WASH`]) and less on every other, so the sky stirs rather than blowing out.
fn wash(buf: &mut Buffer, area: Rect, strength: u8) {
    let Some(ink) = glare() else {
        return;
    };
    let share = f32::from(strength) / 100.0 * WASH;
    for pos in area.positions() {
        let cell = &mut buf[pos];
        // From whatever the page has there rather than from the plate alone: the storm
        // lifts the page it is painted over, whatever that page turns out to be.
        cell.set_bg(theme::mix(cell.bg, ink, share));
    }
}

/// The colour the flash lifts the page toward: the palette's own ink, so the glimmer
/// carries the theme's hue — the warm parchment of Faerun, the blue of Tokyo Night.
///
/// A palette whose ink is darker than its plate — a light theme — leaves the page no
/// brighter to go: there the sky stays as it is and the bolt alone carries the strike.
fn glare() -> Option<Color> {
    (brightness(theme::text()) > brightness(theme::bg())).then(theme::text)
}

/// How bright a palette colour is, by the sum of its channels: enough to tell ink from
/// plate, on a colour the palette accessors painted.
fn brightness(color: Color) -> u32 {
    match color {
        Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b),
        _ => 0,
    }
}

/// Paints the strike's bolt: the channel down the page and the fork off it.
fn bolt(buf: &mut Buffer, area: Rect, seed: u32) {
    let channel = channel(area, seed);
    let fork = branch(area, seed, &channel);
    let text = art::text_block(area);
    for (pos, glyph) in channel.into_iter().chain(fork) {
        if text.contains(pos) {
            continue;
        }
        let cell = &mut buf[pos];
        // The background is the wash's, not the bolt's: the channel is a bright glyph
        // on a lit sky rather than a hole punched in it.
        cell.set_char(glyph);
        cell.set_fg(theme::text());
        cell.modifier.insert(Modifier::BOLD);
    }
}

/// The bolt's channel, cell by cell: every row from the top of the page down to
/// where the strike lands, each cell leaning the way the channel steps into it.
///
/// Pure in the strike's `seed`, the way the rain is pure in its lanes: the same
/// strike draws the same bolt on every one of its frames.
fn channel(area: Rect, seed: u32) -> Vec<(Position, char)> {
    let mut cells = Vec::new();
    if area.is_empty() {
        return cells;
    }
    let (left, right) = (i32::from(area.x), i32::from(area.right()) - 1);
    let rows = depth(area, seed);
    let mut x = left + (seed % u32::from(area.width)) as i32;
    for row in 0..rows {
        let glyph = if row == 0 {
            // The channel enters from above the page, so its first cell is the
            // straight it arrives on.
            VERTICAL
        } else {
            let next = (x + step(seed, row)).clamp(left, right);
            let glyph = lean(next - x);
            x = next;
            glyph
        };
        cells.push((Position::new(x as u16, area.y + row), glyph));
    }
    cells
}

/// Which way the channel leans through `row`: a column to the left, none at all,
/// or one to the right — weighted to hold straight, so the run reads as lightning
/// rather than as a staircase.
fn step(seed: u32, row: u16) -> i32 {
    match hash(seed ^ u32::from(row)) % 8 {
        0 => -1,
        5 | 6 => 1,
        _ => 0,
    }
}

/// The glyph a channel paints where it moves a column as it goes down: the
/// diagonal it climbs, or the vertical it holds.
fn lean(step: i32) -> char {
    match step {
        1 => LEANS_RIGHT,
        -1 => LEANS_LEFT,
        _ => VERTICAL,
    }
}

/// How many rows a bolt runs, counting the row it enters on: at least
/// [`BOLT_DEPTH_MIN_PERCENT`] of the page and at most all of it.
fn depth(area: Rect, seed: u32) -> u16 {
    if area.is_empty() {
        return 0;
    }
    let span = 100 - BOLT_DEPTH_MIN_PERCENT;
    let percent = BOLT_DEPTH_MIN_PERCENT + (seed % u32::from(span)) as u16;
    let rows = (u32::from(area.height) * u32::from(percent) / 100) as u16;
    rows.clamp(1, area.height)
}

/// The fork a bolt throws off on its way down: a short run leaving the channel
/// [`BOLT_FORK_PERCENT`] of the way along it, heading to one side of it so the
/// split reads as a branch rather than as a second channel.
fn branch(area: Rect, seed: u32, channel: &[(Position, char)]) -> Vec<(Position, char)> {
    let mut cells = Vec::new();
    let Some(&(from, _)) = channel.get(channel.len() * usize::from(BOLT_FORK_PERCENT) / 100) else {
        return cells;
    };
    let (left, right) = (i32::from(area.x), i32::from(area.right()) - 1);
    let towards = if seed & 1 == 0 { 1 } else { -1 };
    let mut x = i32::from(from.x);
    for row in 1..=BOLT_FORK_ROWS {
        let y = from.y.saturating_add(row);
        if y >= area.bottom() {
            break;
        }
        // The fork mostly steps away from the channel and holds now and then, so
        // it tapers off rather than running parallel to it.
        let holds = hash(seed.wrapping_add(u32::from(row))).is_multiple_of(3);
        let next = (x + if holds { 0 } else { towards }).clamp(left, right);
        cells.push((Position::new(next as u16, y), lean(next - x)));
        x = next;
    }
    cells
}

/// A small integer hash: seeds each strike's bolt and the jitter in the storm's
/// rhythm, so the storm scatters deterministically with no state to keep in step
/// with the clock. The same mix the rain's lanes are seeded from.
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

    use ratatui::style::Style;

    /// What a test page paints on every cell: a symbol to stand in for the scene's
    /// own, so the wash and the bolt are read off the page rather than assumed.
    const MARKER: &str = "·";

    /// The size the storm is drawn at through most of these tests.
    fn area() -> Rect {
        Rect::new(0, 0, 80, 24)
    }

    /// A settled page: the plate behind every cell and the page's ink on it, which
    /// is what `art::paint` leaves and what the storm is painted over.
    fn page(area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        for pos in area.positions() {
            let cell = &mut buf[pos];
            cell.set_symbol(MARKER);
            cell.set_style(Style::new().bg(theme::bg()).fg(theme::text()));
        }
        buf
    }

    /// The storm `age` into the page, painted over a settled page.
    fn storm(area: Rect, age: Duration) -> Buffer {
        let mut buf = page(area);
        paint(&mut buf, area, age);
        buf
    }

    /// A palette colour's channels, for measuring how far the wash moved it.
    fn channels(color: Color) -> [i32; 3] {
        match color {
            Color::Rgb(r, g, b) => [i32::from(r), i32::from(g), i32::from(b)],
            _ => [0, 0, 0],
        }
    }

    /// The storm paints what its clock says and nothing else: the same age draws
    /// the same page, which is what lets nothing at all be kept between frames.
    #[test]
    fn the_storm_is_a_pure_function_of_its_clock() {
        // The two pages are compared cell for cell, palette included.
        let _guard = theme::lock_for_tests();
        let area = area();
        let striking = strike_at(3);
        assert_eq!(
            storm(area, striking).content,
            storm(area, striking).content,
            "the strike redraws identically"
        );
        assert_ne!(
            storm(area, striking).content,
            storm(area, striking + STRIKE).content,
            "and what moves between the two is the strike"
        );
    }

    /// The storm strikes on a rhythm rather than a metronome, and never twice at
    /// once: a strike is spent before the next one can land, so one bolt is on the
    /// page at a time.
    #[test]
    fn the_storm_strikes_irregularly_but_never_twice_at_once() {
        let starts: Vec<Duration> = (0..24).map(strike_at).collect();
        assert!(
            starts.windows(2).all(|pair| pair[0] < pair[1]),
            "strikes land in order: {starts:?}"
        );

        for (index, &start) in starts.iter().enumerate() {
            let slot = Duration::from_millis(index as u64 * STRIKE_PERIOD_MS);
            let into = start - slot;
            assert!(
                into >= Duration::from_millis(STRIKE_EARLIEST_MS)
                    && into < Duration::from_millis(STRIKE_LATEST_MS),
                "strike {index} lands {into:?} into its slot"
            );
        }

        let gaps: Vec<Duration> = starts.windows(2).map(|pair| pair[1] - pair[0]).collect();
        assert!(
            gaps.iter().any(|&gap| gap != gaps[0]),
            "the rhythm is irregular: {gaps:?}"
        );
        assert!(
            gaps.iter().all(|&gap| gap > STRIKE),
            "a strike is over before the next lands: {gaps:?}"
        );
    }

    /// The page comes up in the rain and is struck a beat later: the storm's
    /// opening strike is held clear of the sweep that carries the page in, so the
    /// scene never arrives through a flash.
    #[test]
    fn the_opening_strike_lands_after_the_page_has_arrived() {
        assert!(strike(Duration::ZERO).is_none(), "the storm opens quiet");
        assert!(
            strike_at(0) > art::ENTRANCE,
            "the first strike lands {:?} into the page, past the entrance",
            strike_at(0)
        );
    }

    /// A strike is seen in blows rather than in a fade: the sky is lit for the return
    /// stroke, dims while the channel discharges, lifts again for the weaker second
    /// stroke, and then decays into the afterglow.
    #[test]
    fn a_strike_flickers_rather_than_fading() {
        for (number, &strength) in STROKE.iter().enumerate() {
            let at = Duration::from_millis(number as u64 * STROKE_FRAME_MS);
            assert_eq!(stroke(at), strength, "frame {number} of the strike");
        }

        assert!(
            STROKE[2] < STROKE[1] && STROKE[2] < STROKE[3],
            "the sky dims between the strokes: {STROKE:?}"
        );
        assert!(
            STROKE[3] > STROKE[2] && STROKE[4] > STROKE[3] && STROKE[4] < STROKE[0],
            "a second, weaker stroke follows the first: {STROKE:?}"
        );
        assert!(
            STROKE[4] > STROKE[5] && STROKE[5] > STROKE[6],
            "and decays into the afterglow: {STROKE:?}"
        );
        assert!(
            STROKE.iter().filter(|&&strength| strength >= BOLT).count() > 1,
            "the bolt shows on more than one frame: {STROKE:?}"
        );
        assert!(
            STROKE.iter().any(|&strength| strength < BOLT),
            "and is off the page on at least one, which is what makes it flicker: {STROKE:?}"
        );
    }

    /// The bolt is one jagged run: a cell on every row from the top of the page,
    /// each one no more than a column from the row above it, leaning the way it
    /// steps.
    #[test]
    fn the_bolt_runs_down_the_page_a_row_at_a_time() {
        let area = area();
        for index in 0..16 {
            let channel = channel(area, seed(strike_at(index)));
            assert!(
                channel.len()
                    >= usize::from(area.height) * usize::from(BOLT_DEPTH_MIN_PERCENT) / 100,
                "strike {index} throws a bolt of {} rows",
                channel.len()
            );
            for (row, &(pos, glyph)) in channel.iter().enumerate() {
                assert_eq!(
                    pos.y,
                    area.y + row as u16,
                    "a cell on every row from the top"
                );
                assert!(area.contains(pos), "row {row} stays on the page: {pos:?}");
                if row == 0 {
                    assert_eq!(glyph, VERTICAL, "the channel enters straight");
                    continue;
                }
                let step = i32::from(pos.x) - i32::from(channel[row - 1].0.x);
                assert!((-1..=1).contains(&step), "row {row} jumps {step} columns");
                assert_eq!(glyph, lean(step), "row {row} leans the way it steps");
            }
        }
    }

    /// A bolt forks once on its way down: a short run leaving the channel two
    /// thirds of the way along it, one row below the channel's own cell, staying on
    /// the page and heading to one side of it.
    #[test]
    fn a_bolt_forks_on_its_way_down() {
        let area = area();
        let seed = seed(strike_at(5));
        let channel = channel(area, seed);
        let fork = branch(area, seed, &channel);

        assert!(!fork.is_empty(), "the bolt forks");
        assert!(
            fork.len() <= usize::from(BOLT_FORK_ROWS),
            "the fork is short"
        );
        let (from, _) = channel[channel.len() * usize::from(BOLT_FORK_PERCENT) / 100];
        assert_eq!(
            fork[0].0.y,
            from.y + 1,
            "the fork leaves the channel below its own cell"
        );
        for (row, &(pos, glyph)) in fork.iter().enumerate() {
            assert_eq!(pos.y, from.y + 1 + row as u16, "the fork runs on down");
            assert!(area.contains(pos), "the fork stays on the page: {pos:?}");
            assert!(
                matches!(glyph, VERTICAL | LEANS_LEFT | LEANS_RIGHT),
                "the fork paints the channel's own glyphs"
            );
        }
        assert!(
            fork.iter().any(|&(_, glyph)| glyph != VERTICAL),
            "and it steps away from the channel rather than running parallel to it"
        );
    }

    /// The flash is slight: at its strongest it moves the plate a small share of the
    /// way toward the page's ink, and every dimmer frame moves it less. Both channels
    /// of the wash read this as light rather than as a blow.
    #[test]
    fn the_flash_is_slight_and_leaves_the_pages_ink_alone() {
        struct Restore(shuvarie_core::ThemeColors);
        impl Drop for Restore {
            fn drop(&mut self) {
                theme::set(self.0);
            }
        }
        let _guard = theme::lock_for_tests();
        let _restore = Restore(theme::current());
        theme::set(shuvarie_core::ThemeColors::faerun());

        let area = area();
        let mut lit = page(area);
        wash(&mut lit, area, 100);
        for pos in area.positions() {
            assert_eq!(
                lit[pos].symbol(),
                MARKER,
                "the wash moves no symbol: {pos:?}"
            );
            assert_eq!(lit[pos].fg, theme::text(), "and writes no ink: {pos:?}");
        }

        // The lift is a share of the way from plate to ink, and a slight one: a
        // quarter of that span is the ceiling, and every channel stays well inside it.
        let plate = channels(theme::bg());
        let flashed = channels(theme::mix(theme::bg(), theme::text(), WASH));
        let ink = channels(theme::text());
        for channel in 0..3 {
            let lifted = flashed[channel] - plate[channel];
            let span = ink[channel] - plate[channel];
            assert!(lifted > 0, "channel {channel} is lifted at all");
            assert!(
                lifted * 4 <= span,
                "channel {channel} moves {lifted} of its {span}: the flash is a lift, not a blow"
            );
        }
        assert_ne!(
            theme::mix(theme::bg(), theme::text(), WASH),
            theme::text(),
            "the plate never becomes the ink"
        );

        // And the dimmer frames are dimmer: the afterglow lifts the sky less than the
        // return stroke does.
        let corner = Position::new(0, 0);
        let mut faint = page(area);
        wash(&mut faint, area, STROKE[STROKE.len() - 1]);
        assert!(
            brightness(faint[corner].bg) < brightness(lit[corner].bg),
            "the afterglow is dimmer than the blow"
        );
        assert_ne!(
            faint[corner].bg,
            theme::bg(),
            "but the sky is still lifted rather than untouched"
        );
    }

    /// A palette with no room to be lit — a light theme, whose ink is darker than its
    /// plate — is lit by the bolt alone: the sky is left as the page painted it and the
    /// channel still stands out against it.
    #[test]
    fn a_light_palette_is_lit_by_the_bolt_alone() {
        struct Restore(shuvarie_core::ThemeColors);
        impl Drop for Restore {
            fn drop(&mut self) {
                theme::set(self.0);
            }
        }
        let _guard = theme::lock_for_tests();
        let _restore = Restore(theme::current());
        theme::set(shuvarie_core::ThemeColors::faerun_light());

        let area = area();
        assert_eq!(glare(), None, "a light page has nowhere brighter to go");
        let mut buf = page(area);
        wash(&mut buf, area, 100);
        for pos in area.positions() {
            assert_eq!(
                buf[pos].bg,
                theme::bg(),
                "the sky is left as it is: {pos:?}"
            );
        }

        // The strike is not lost, only its flash: the channel is still drawn.
        bolt(&mut buf, area, seed(strike_at(4)));
        assert!(
            area.positions().any(|pos| buf[pos].symbol() != MARKER),
            "the bolt paints over a light page"
        );
    }

    /// A strike writes the channel and nothing else: every cell whose symbol differs
    /// from the page's carries one of the bolt's own glyphs, in the page's ink, so the
    /// storm adds a channel to the scene rather than redrawing it.
    #[test]
    fn a_strike_writes_only_the_channel_over_the_page() {
        let _guard = theme::lock_for_tests();
        let area = area();
        let base = page(area);

        let mut bolted = 0;
        for index in 0..16 {
            let struck = storm(area, strike_at(index));
            for pos in area.positions() {
                let (before, after) = (&base[pos], &struck[pos]);
                if after.symbol() == before.symbol() {
                    assert_eq!(
                        (after.fg, after.modifier),
                        (before.fg, before.modifier),
                        "strike {index} restyles an unbolted cell at {pos:?}"
                    );
                    continue;
                }
                assert!(
                    matches!(after.symbol(), "│" | "╱" | "╲"),
                    "strike {index} paints {:?} at {pos:?}",
                    after.symbol()
                );
                assert_eq!(after.fg, before.fg, "the bolt writes the page's ink");
                bolted += 1;
            }
        }
        assert!(bolted > 0, "and the bolt does paint");
    }

    /// The bolt breaks around the page's text: no cell of the wordmark's box or of
    /// the caption's row is painted, whatever the strike, so the title and the line
    /// that says how to leave the scene survive every flash.
    #[test]
    fn the_bolt_keeps_out_of_the_pages_text() {
        let area = area();
        let text = art::text_block(area);
        assert!(!text.is_empty(), "the page has text to keep clear of");

        let mut painted = 0;
        for index in 0..24 {
            let mut buf = page(area);
            bolt(&mut buf, area, seed(strike_at(index)));
            for pos in text.positions() {
                assert_eq!(
                    buf[pos].symbol(),
                    MARKER,
                    "strike {index} paints the page's text at {pos:?}"
                );
            }
            painted += area
                .positions()
                .filter(|&pos| !text.contains(pos) && buf[pos].symbol() != MARKER)
                .count();
        }
        assert!(painted > 0, "and the bolt does paint, beside the text");
    }

    /// The render loop is handed the storm's next frame: a stroke's frame while a
    /// strike burns, and otherwise the next strike — later than the rain's own
    /// frame, so a quiet storm adds nothing to the loop's work.
    #[test]
    fn the_storm_wakes_the_loop_for_its_own_frames() {
        let start = strike_at(1);
        assert_eq!(
            next_wake(start),
            Some(Duration::from_millis(STROKE_FRAME_MS)),
            "a strike that has just landed waits a whole frame for its next"
        );
        let part_way = start + Duration::from_millis(STROKE_FRAME_MS / 2);
        let wake = next_wake(part_way).expect("the strike is still burning");
        assert!(
            wake > Duration::ZERO && wake <= Duration::from_millis(STROKE_FRAME_MS),
            "{part_way:?} into the storm waits {wake:?}"
        );

        assert_eq!(
            next_wake(Duration::ZERO),
            Some(strike_at(0)),
            "a quiet sky waits for the next strike"
        );
        assert!(
            strike_at(0) > Duration::from_millis(1_000 / 12),
            "which is further off than the rain's own frame"
        );
    }

    /// A quiet sky is left exactly as the page painted it: the storm only ever
    /// touches the page while a strike is burning.
    #[test]
    fn a_quiet_sky_is_left_as_the_page_painted_it() {
        let _guard = theme::lock_for_tests();
        let area = area();
        assert_eq!(storm(area, Duration::ZERO).content, page(area).content);
        assert_eq!(
            storm(area, art::ENTRANCE).content,
            page(area).content,
            "the page arrives with the storm quiet"
        );
    }
}
