//! TUI-side attachment media: raw blob bytes shipped from core, decoded once,
//! and rendered through `ratatui-image`'s halfblocks protocol into cached
//! `Line`s the chat virtualizer paints like any other body chunk.
//!
//! The chat pane knows attachment *metadata* (from `ChatMsg`/`TreeNode`); the
//! blob bytes live in the session store. When a user turn with images
//! materializes, the missing hashes are requested from core
//! ([`SessionEffect::LoadMedia`] → `Command::LoadAttachmentMedia`) and arrive
//! as [`MediaBytes`] — inserted here, capped by an LRU so an image-heavy
//! session cannot balloon the TUI's memory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use image::{DynamicImage, ImageReader};
use ratatui::prelude::*;
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;

/// Total raw media bytes the store keeps. Oldest-inserted items are evicted
/// first; an evicted image falls back to its unloaded chip until the next
/// load-shaped event re-requests it.
const MAX_MEDIA_BYTES: usize = 48 * 1024 * 1024;

/// Rows an unloaded image region reserves: its chip line. A real render
/// replaces the estimate once the turn materializes with media present.
pub const CHIP_ROWS: u32 = 1;

/// Debug-friendly byte payload (a raw `Vec<u8>` would blow up event traces).
pub struct MediaBytes(pub Arc<[u8]>);

impl fmt::Debug for MediaBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaBytes({} bytes)", self.0.len())
    }
}

/// One attachment's media, decoded lazily on first render.
pub struct MediaImage {
    bytes: Rc<[u8]>,
    decoded: RefCell<Option<Rc<DynamicImage>>>,
}

impl MediaImage {
    /// The decoded pixels, decoding on first use. A broken image yields
    /// `None` every time (the block shows its unloaded chip instead).
    pub fn decoded(&self) -> Option<Rc<DynamicImage>> {
        if let Some(decoded) = &*self.decoded.borrow() {
            return Some(Rc::clone(decoded));
        }
        let decoded = ImageReader::new(std::io::Cursor::new(&self.bytes[..]))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?;
        let decoded = Rc::new(decoded);
        *self.decoded.borrow_mut() = Some(Rc::clone(&decoded));
        Some(decoded)
    }
}

/// The chat pane's received media, keyed by content hash (shas arrive with
/// attachment metadata, so presence is testable without decoding).
pub struct MediaStore {
    cell: FontSize,
    /// Bumped whenever the configured cell size changes: image caches keyed
    /// on it re-render (halfblock row counts come from the font metrics).
    cell_gen: u64,
    images: HashMap<String, Rc<MediaImage>>,
    order: Vec<String>,
    total: usize,
}

impl MediaStore {
    pub fn new(cell: FontSize) -> Self {
        Self {
            cell,
            cell_gen: 0,
            images: HashMap::new(),
            order: Vec::new(),
            total: 0,
        }
    }

    pub fn cell_size(&self) -> FontSize {
        self.cell
    }

    pub fn cell_gen(&self) -> u64 {
        self.cell_gen
    }

    pub fn set_cell_size(&mut self, cell: FontSize) {
        if cell.width != self.cell.width || cell.height != self.cell.height {
            self.cell = cell;
            self.cell_gen += 1;
        }
    }

    /// Whether the store serves this hash (the attachment metadata's sha256).
    pub fn has(&self, sha256: &str) -> bool {
        self.images.contains_key(sha256)
    }

    pub fn get(&self, sha256: &str) -> Option<Rc<MediaImage>> {
        self.images.get(sha256).cloned()
    }

    /// Shas from the given list the store cannot serve: the next media
    /// request batch.
    pub fn missing<'a, I>(&self, shas: I) -> Vec<String>
    where
        I: IntoIterator<Item = &'a str>,
    {
        shas.into_iter()
            .filter(|sha| !self.has(sha))
            .map(str::to_string)
            .collect()
    }

    /// Record an arrival. Inserting bumps no rebuild counter: block caches
    /// key on which shas are present, and content-addressing means an entry
    /// for an sha is immutable once written.
    pub fn insert(&mut self, sha256: String, bytes: Vec<u8>) {
        if self.has(&sha256) {
            return;
        }
        self.total += bytes.len();
        self.images.insert(
            sha256.clone(),
            Rc::new(MediaImage {
                bytes: bytes.into(),
                decoded: RefCell::new(None),
            }),
        );
        self.order.push(sha256);
        while self.total > MAX_MEDIA_BYTES && self.order.len() > 1 {
            let evicted = self.order.remove(0);
            if let Some(item) = self.images.remove(&evicted) {
                self.total -= item.bytes.len();
            }
        }
    }
}

/// A halfblocks picker for the given font metrics (deprecated constructor —
/// the only one that takes a font size without querying stdio).
fn picker_for(cell: FontSize) -> Picker {
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(cell);
    picker.set_protocol_type(ProtocolType::Halfblocks);
    picker
}

/// Render one image attachment into content-width lines through the
/// halfblocks protocol (tricolor cells: ordinary diff-able cells that survive
/// the virtualizer's clipped paint windows, unlike graphical payload
/// protocols). `None` when the media is absent/undecodable or the width is
/// degenerate.
pub fn image_lines(store: &MediaStore, sha256: &str, width: u16) -> Option<Vec<Line<'static>>> {
    if width == 0 {
        return None;
    }
    let item = store.get(sha256)?;
    let decoded = Rc::try_unwrap(item.decoded()?).unwrap_or_else(|arc| (*arc).clone());
    let picker = picker_for(store.cell_size());
    let proto = picker
        .new_protocol(
            decoded,
            Size::new(width, u16::MAX),
            ratatui_image::Resize::Fit(None),
        )
        .ok()?;
    Some(lift_lines(&proto))
}

/// Render the protocol into a scratch buffer of exactly its own size and lift
/// every cell to an owned styled span (the segment painter fills the rest of
/// the row with the block background).
fn lift_lines(proto: &Protocol) -> Vec<Line<'static>> {
    let size = proto.size();
    let mut buf = Buffer::empty(Rect::new(0, 0, size.width.max(1), size.height.max(1)));
    ratatui_image::Image::new(proto).render(buf.area, &mut buf);
    (0..buf.area.height)
        .map(|y| {
            let spans = (0..buf.area.width)
                .map(|x| {
                    let cell = &buf[(x, y)];
                    Span::styled(
                        cell.symbol().to_string(),
                        Style::new().fg(cell.fg).bg(cell.bg),
                    )
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageEncoder;
    use image::codecs::png::PngEncoder;

    pub(super) fn gradient_png() -> Vec<u8> {
        let img = image::ImageBuffer::from_fn(64, 32, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 8) as u8, 40])
        });
        let mut out = Vec::new();
        PngEncoder::new(std::io::Cursor::new(&mut out))
            .write_image(img.as_raw(), 64, 32, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn image_lines_renders_and_lifts_styled_cells() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("sha1".into(), gradient_png());
        let lines = image_lines(&store, "sha1", 40).expect("renders");
        assert!(!lines.is_empty());
        let colored = lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| matches!(span.style.bg, Some(bg) if bg != Color::Reset));
        assert!(colored, "halfblock rows must carry image colors");
    }

    #[test]
    fn image_lines_without_media_is_none_and_missing_reports() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("present".into(), gradient_png());
        assert!(image_lines(&store, "absent", 40).is_none());
        assert_eq!(
            store.missing(["absent", "present"]),
            vec!["absent".to_string()]
        );
    }

    #[test]
    fn lru_eviction_drops_the_oldest() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        let big = vec![0u8; MAX_MEDIA_BYTES / 2];
        store.insert("a".into(), big.clone());
        store.insert("b".into(), big.clone());
        store.insert("c".into(), big);
        assert!(!store.has("a"), "oldest must evict");
        assert!(store.has("b") && store.has("c"));
    }

    #[test]
    fn cell_size_change_bumps_generation() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        assert_eq!(store.cell_gen(), 0);
        store.set_cell_size(FontSize::new(8, 16));
        assert_eq!(store.cell_gen(), 0, "same size is a no-op");
        store.set_cell_size(FontSize::new(10, 20));
        assert_eq!(store.cell_gen(), 1);
    }

    #[test]
    fn empty_image_does_not_decode_never_lies() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("broken".into(), b"not a png".to_vec());
        assert!(
            image_lines(&store, "broken", 40).is_none(),
            "undecodable media stays unloaded"
        );
    }

    #[test]
    fn decode_failure_stays_none_across_repeated_renders() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("broken".into(), b"\x89PNG\r\n\x1a\ngarbage".to_vec());
        assert!(image_lines(&store, "broken", 40).is_none());
        assert!(image_lines(&store, "broken", 80).is_none());
    }
}

/// A long-lived empty media store for tests that build `ChatEnv`s and never
/// touch media. One leaked store per test thread; tests that mutate a store
/// build their own.
#[cfg(test)]
pub(crate) fn shared_test_store() -> &'static MediaStore {
    thread_local! {
        static STORE: std::cell::Cell<Option<&'static MediaStore>> = const { std::cell::Cell::new(None) };
    }
    STORE.with(|cell| {
        cell.get().unwrap_or_else(|| {
            #[allow(deprecated)]
            let leaked: &'static MediaStore =
                Box::leak(Box::new(MediaStore::new(FontSize::new(8, 16))));
            cell.set(Some(leaked));
            leaked
        })
    })
}

/// The escape-passthrough spike: the full rendering chain ratatui-image's
/// graphical protocols rely on (payload planted on a buffer cell, diffed by
/// ratatui, flushed through the Termina backend) exercised over a captured
/// in-memory termina terminal. Halfblocks — the protocol the chat pane uses
/// — plants plain styled cells and never touches this; the spike pins
/// whether kitty/sixel payload injection could pass unmodified today.
#[cfg(test)]
mod passthrough_spike {
    use std::rc::Rc;

    use ratatui::Terminal;
    use ratatui::backend::TerminaBackend;
    use ratatui::prelude::*;
    use ratatui_image::FontSize;
    use ratatui_image::Resize;
    use ratatui_image::picker::{Picker, ProtocolType};
    use ratatui_image::protocol::Protocol;

    use super::tests::gradient_png;

    struct CaptureTerminal {
        output: Rc<std::cell::RefCell<Vec<u8>>>,
    }

    impl std::io::Write for CaptureTerminal {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl termina::Terminal for CaptureTerminal {
        fn enter_raw_mode(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn enter_cooked_mode(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn get_dimensions(&self) -> std::io::Result<termina::WindowSize> {
            Ok(termina::WindowSize {
                cols: 80,
                rows: 24,
                pixel_width: Some(640),
                pixel_height: Some(384),
            })
        }

        fn event_reader(&self) -> termina::EventReader {
            panic!("the capture fixture never reads events")
        }

        fn poll<F: Fn(&termina::Event) -> bool>(
            &self,
            _filter: F,
            _timeout: Option<std::time::Duration>,
        ) -> std::io::Result<bool> {
            panic!("the capture fixture never polls")
        }

        fn read<F: Fn(&termina::Event) -> bool>(
            &self,
            _filter: F,
        ) -> std::io::Result<termina::Event> {
            panic!("the capture fixture never reads")
        }

        fn set_panic_hook(
            &mut self,
            _hook: impl Fn(&mut termina::PlatformHandle) + Send + Sync + 'static,
        ) {
        }
    }

    fn has(bytes: &[u8], needle: &[u8]) -> bool {
        bytes.len() >= needle.len() && bytes.windows(needle.len()).any(|window| window == needle)
    }

    /// A fresh protocol instance for a gradient PNG: the transmit (where the
    /// protocol has one) is still intact, the image id is this instance's.
    fn fresh_proto(protocol_type: ProtocolType, size: Size) -> Protocol {
        #[allow(deprecated)]
        let mut picker = Picker::from_fontsize(FontSize::new(8, 16));
        picker.set_protocol_type(protocol_type);
        picker
            .new_protocol(
                image::ImageReader::new(std::io::Cursor::new(gradient_png()))
                    .with_guessed_format()
                    .unwrap()
                    .decode()
                    .unwrap(),
                size,
                Resize::Fit(None),
            )
            .unwrap()
    }

    fn capture_terminal() -> (
        Terminal<TerminaBackend<CaptureTerminal>>,
        Rc<std::cell::RefCell<Vec<u8>>>,
    ) {
        let capture = Rc::new(std::cell::RefCell::new(Vec::new()));
        let backend = TerminaBackend::new(CaptureTerminal {
            output: capture.clone(),
        });
        let terminal = Terminal::new(backend).unwrap();
        (terminal, capture)
    }

    /// Which protocol byte-runs look like, per kind.

    #[test]
    fn kitty_plants_the_raw_transmit_on_the_first_cell() {
        let proto = fresh_proto(ProtocolType::Kitty, Size::new(40, 10));
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        ratatui_image::Image::new(&proto).render(buf.area, &mut buf);
        let payload = buf[(0, 0)].symbol().to_string();
        assert!(
            payload.as_bytes().starts_with(b"\x1b_G"),
            "kitty plants its transmit as the first cell's symbol: {payload:?}"
        );
        use ratatui::buffer::CellDiffOption;
        // Kitty (fixed, unicode-placeholders mode) plants NO skip region:
        // every covered cell is an ordinary forced-width placeholder glyph,
        // so the diff behaves cell-by-cell and scrolling stays clean.
        assert_eq!(
            buf[(1, 0)].diff_option,
            CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(1).unwrap())
        );
    }

    #[test]
    fn kitty_transmit_reaches_the_sink_and_is_one_shot_per_protocol() {
        let (mut terminal, capture) = capture_terminal();
        let proto = fresh_proto(ProtocolType::Kitty, Size::new(40, 10));
        let render = |frame: &mut ratatui::Frame<'_>| {
            ratatui_image::Image::new(&proto).render(frame.area(), frame.buffer_mut());
        };
        terminal.draw(render).unwrap();
        let first = capture.borrow().clone();
        assert!(has(&first, b"\x1b_G"), "the transmit streams verbatim");
        let before_second = first.len();
        terminal.draw(render).unwrap();
        let second = capture.borrow()[before_second..].to_vec();
        assert!(
            !has(&second, b"\x1b_G"),
            "one-shot: the second frame transmits nothing"
        );
        assert!(
            has(&second, "\u{10EEEE}".as_bytes()) || has(&first, "\u{10EEEE}".as_bytes()),
            "kitty re-renders with unicode placeholder cells"
        );
    }

    #[test]
    fn sixel_payload_passthrough_through_the_full_ratatui_chain() {
        let (mut terminal, capture) = capture_terminal();
        let proto = fresh_proto(ProtocolType::Sixel, Size::new(40, 10));
        let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
        ratatui_image::Image::new(&proto).render(buf.area, &mut buf);
        let payload = buf[(0, 0)].symbol().to_string();
        assert!(
            payload.as_bytes().starts_with(b"\x1b[") && has(payload.as_bytes(), b"\x1bP"),
            "sixel plants cursor-wrapped DCS data on the first cell: {payload:?}"
        );
        let need = payload.into_bytes();

        let render = |frame: &mut ratatui::Frame<'_>| {
            ratatui_image::Image::new(&proto).render(frame.area(), frame.buffer_mut());
        };
        terminal.draw(render).unwrap();
        let first = capture.borrow().clone();
        assert_eq!(
            first
                .windows(need.len())
                .filter(|window| *window == need)
                .count(),
            1,
            "the planted payload survives ratatui's diff + termina's flush verbatim"
        );
        let before_second = first.len();
        terminal.draw(render).unwrap();
        let second = capture.borrow()[before_second..].to_vec();
        assert!(!has(&second, &need), "an unchanged frame re-emits nothing");
    }

    #[test]
    fn halfblocks_painting_reaches_the_sink_as_styled_cells() {
        let (mut terminal, capture) = capture_terminal();
        let proto = fresh_proto(ProtocolType::Halfblocks, Size::new(40, 10));
        let size = proto.size();
        let render = |frame: &mut ratatui::Frame<'_>| {
            ratatui_image::Image::new(&proto).render(frame.area(), frame.buffer_mut());
        };
        terminal.draw(render).unwrap();
        let bytes = capture.borrow().clone();
        let lower = "\u{2584}".as_bytes();
        let upper = "\u{2580}".as_bytes();
        let glyphs = usize::from(size.width * size.height);
        assert!(
            bytes.windows(3).filter(|window| *window == lower).count() > 0
                || bytes.windows(3).filter(|window| *window == upper).count() > 0,
            "halfblock glyphs stream as ordinary cells; head bytes: {}",
            loggable(&bytes)
        );
        assert!(
            bytes.len() > glyphs * 10,
            "every glyph carries its SGR state (fg/bg per half)"
        );
    }

    fn loggable(bytes: &[u8]) -> String {
        let mut out = String::new();
        for byte in bytes.iter().take(300) {
            match byte {
                0x20..=0x7e => out.push(char::from(*byte)),
                b'\x1b' => out.push_str("\\e"),
                _ => out.push_str(&format!("\\x{byte:02x}")),
            }
        }
        out
    }
}
