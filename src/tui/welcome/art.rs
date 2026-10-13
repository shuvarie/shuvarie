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

use crate::tui::theme;

/// The wordmark's letters, in order.
const WORDMARK_TEXT: &str = "SHUVARIE";
/// The block font's cell size: every glyph letter is `GLYPH_WIDTH` columns
/// wide and `GLYPH_HEIGHT` rows tall.
const GLYPH_WIDTH: usize = 5;
const GLYPH_HEIGHT: usize = 5;

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

/// The block glyphs of one letter, no trailing padding (the builder pads every
/// row out to [`GLYPH_WIDTH`]). `None` for a letter the font does not cover.
fn glyph(letter: char) -> Option<[&'static str; GLYPH_HEIGHT]> {
    Some(match letter {
        'S' => ["█████", "█", "█████", "    █", "█████"],
        'H' => ["█   █", "█   █", "█████", "█   █", "█   █"],
        'U' => ["█   █", "█   █", "█   █", "█   █", "█████"],
        'V' => ["█   █", "█   █", "█   █", " █ █", "  █"],
        'A' => [" ███", "█   █", "█████", "█   █", "█   █"],
        'R' => ["████", "█   █", "████", "█  █", "█   █"],
        'I' => ["█████", "  █", "  █", "  █", "█████"],
        'E' => ["█████", "█", "████", "█", "█████"],
        _ => return None,
    })
}

/// The wordmark's rows, rendered once from the per-letter glyphs: one blank
/// column between letters and every row padded to the full width, so the
/// layout maths below stays in step with what is painted.
fn wordmark() -> &'static [String] {
    static WORDMARK: OnceLock<Vec<String>> = OnceLock::new();
    WORDMARK.get_or_init(|| {
        let letters: Vec<_> = WORDMARK_TEXT.chars().filter_map(glyph).collect();
        (0..GLYPH_HEIGHT)
            .map(|row| {
                letters
                    .iter()
                    .map(|letter| {
                        let mut text = String::from(letter[row]);
                        let pad = GLYPH_WIDTH.saturating_sub(text.chars().count());
                        text.extend(std::iter::repeat_n(' ', pad));
                        text
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect()
    })
}

/// The wordmark's painted size in cells.
fn wordmark_size() -> (u16, u16) {
    let letters = WORDMARK_TEXT.chars().count();
    let width = GLYPH_WIDTH * letters + letters.saturating_sub(1);
    (width as u16, GLYPH_HEIGHT as u16)
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

    let wordmark = wordmark_rect(area);
    let rook = rook_rect(area, wordmark);
    let knight = knight_rect(area, wordmark);

    render_rain(buf, area, rain_frame);
    if let Some(origin) = rook {
        render_glyphs(buf, area, origin, &ROOK, silhouette_style());
    }
    if let Some(origin) = knight {
        render_glyphs(buf, area, origin, &KNIGHT, silhouette_style());
    }
    render_wordmark(buf, area, wordmark);
    render_caption(buf, area, wordmark);
}

/// The widest row of a piece of art: the box the layout has to reserve for
/// it. The rows are stored without trailing padding, so the widest one — not
/// the first — sets the width.
fn art_width(art: &[&str]) -> u16 {
    art.iter().map(|row| row.chars().count()).max().unwrap_or(0) as u16
}

/// The rect the wordmark paints into: the block text centred on `area`, lifted
/// half a row to leave the caption — and the knight — room underneath.
fn wordmark_rect(area: Rect) -> Rect {
    let (width, height) = wordmark_size();
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height + 2) / 2;
    Rect::new(x, y, width.min(area.width), height)
}

/// The knight's origin, pinned to the bottom-left corner. `None` when the
/// terminal is too short for the silhouette to clear the wordmark.
fn knight_rect(area: Rect, wordmark: Rect) -> Option<(u16, u16)> {
    let width = art_width(&KNIGHT);
    let height = KNIGHT.len() as u16;
    let x = area.x + 1;
    let y = area.bottom().checked_sub(height)?;
    (y >= wordmark.bottom() && x + width <= area.right()).then_some((x, y))
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

fn render_wordmark(buf: &mut Buffer, area: Rect, rect: Rect) {
    let style = Style::new()
        .fg(theme::accent())
        .add_modifier(Modifier::BOLD);
    render_glyphs(buf, area, (rect.x, rect.y), wordmark(), style);
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

    /// Every glyph stays inside the font's cell box, so the builder's padding
    /// can never under- or over-run a letter.
    #[test]
    fn glyphs_fit_the_block_font() {
        for letter in WORDMARK_TEXT.chars() {
            let glyph = glyph(letter).unwrap_or_else(|| panic!("no glyph for {letter}"));
            for row in glyph {
                assert!(
                    row.chars().count() <= GLYPH_WIDTH,
                    "{letter} row {row:?} is wider than {GLYPH_WIDTH} cells"
                );
            }
        }
    }

    /// The wordmark keeps one fixed width, one blank column between letters:
    /// a short or long row would shear the block text.
    #[test]
    fn wordmark_rows_share_one_width_and_spell_the_name() {
        let (width, height) = wordmark_size();
        assert_eq!(height, GLYPH_HEIGHT as u16);
        assert_eq!(width as usize, GLYPH_WIDTH * 8 + 7, "eight letters, joined");

        let rows = wordmark();
        assert_eq!(rows.len(), GLYPH_HEIGHT);
        for row in rows {
            assert_eq!(row.chars().count(), width as usize, "row {row:?}");
        }
        // Every letter is exactly `GLYPH_WIDTH` wide with one blank column
        // between them, so the block text neither shears nor runs together.
        let letters = WORDMARK_TEXT.chars().count();
        for join in 1..letters {
            let at = join * GLYPH_WIDTH + join - 1;
            assert_eq!(
                rows[2].chars().nth(at),
                Some(' '),
                "blank column before letter {join}"
            );
        }
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

    /// The scene's pieces never overlap on a normal terminal: the rook clears
    /// the wordmark, and the knight sits below it.
    #[test]
    fn a_regular_terminal_places_every_piece() {
        let area = Rect::new(0, 0, 80, 24);
        let wordmark = wordmark_rect(area);
        let rook = rook_rect(area, wordmark).expect("rook fits an 80x24 terminal");
        let knight = knight_rect(area, wordmark).expect("knight fits an 80x24 terminal");

        assert_eq!(wordmark, Rect::new(16, 8, 47, 5));
        assert!(
            rook.0 >= wordmark.right(),
            "rook starts right of the wordmark"
        );
        assert!(
            knight.1 >= wordmark.bottom(),
            "knight starts below the wordmark"
        );
    }

    /// A terminal too narrow or too short drops the silhouettes rather than
    /// letting them collide with the wordmark.
    #[test]
    fn a_cramped_terminal_drops_the_silhouettes() {
        let narrow = Rect::new(0, 0, 20, 24);
        let wordmark = wordmark_rect(narrow);
        assert!(rook_rect(narrow, wordmark).is_none());
        assert!(knight_rect(narrow, wordmark).is_none());

        let short = Rect::new(0, 0, 80, 12);
        let wordmark = wordmark_rect(short);
        assert!(knight_rect(short, wordmark).is_none());
    }

    /// The intro paints the wordmark, the caption, and the rook on a terminal
    /// with room for them.
    #[test]
    fn the_scene_paints_the_wordmark_caption_and_rook() {
        let buf = scene(80, 24, 0);
        let area = Rect::new(0, 0, 80, 24);
        let wordmark = wordmark_rect(area);

        assert_eq!(
            buf[(wordmark.x, wordmark.y)].symbol(),
            "█",
            "the wordmark's first cell"
        );
        assert_eq!(
            buf[(wordmark.x, wordmark.y)].style().fg,
            Some(theme::accent())
        );

        let caption = row_of(&buf, wordmark.bottom() + 1, 80);
        assert!(caption.contains(CAPTION), "{caption:?}");
        assert!(
            caption.starts_with(' '),
            "the caption is centred: {caption:?}"
        );

        let rook = rook_rect(area, wordmark).expect("rook fits");
        assert_eq!(buf[(rook.0, rook.1)].symbol(), "▛", "the rook's crown");
        assert_eq!(buf[(rook.0, rook.1)].style().fg, Some(theme::text_dim()));

        // The rain never buries the wordmark, whatever frame is showing.
        for frame in 0..8 {
            let buf = scene(80, 24, frame);
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
