//! TUI-side attachment media: raw blob bytes shipped from core, decoded once,
//! and rendered into cached lines the chat virtualizer paints like any other
//! body chunk. Rendering runs in the executor pass (`MediaStore::ensure_*`,
//! called between frames where `&mut` is legitimately held); paint reads
//! entries only, so the paint path is pure and the store has no interior
//! mutability at all.
//!
//! The chat pane knows attachment *metadata* (from `ChatMsg`/`TreeNode`); the
//! blob bytes live in the session store. When a user turn with images
//! materializes, the missing hashes are requested from core
//! ([`SessionEffect::LoadMedia`] → `Command::LoadAttachmentMedia`) and arrive
//! as [`MediaBytes`] — inserted here, capped by an LRU so an image-heavy
//! session cannot balloon the TUI's memory.

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::iter::successors;
use std::rc::Rc;
use std::sync::Arc;

use image::{DynamicImage, ImageReader};
use ratatui::prelude::*;
use ratatui_image::FontSize;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use shuvarie_core::ImageProtocol;

/// Total raw media bytes the store keeps. Oldest-inserted items are evicted
/// first; an evicted image falls back to its unloaded chip until the next
/// load-shaped event re-requests it.
const MAX_MEDIA_BYTES: usize = 48 * 1024 * 1024;

/// Rows an unloaded image region reserves: its chip line. A real render
/// replaces the estimate once the turn materializes with media present.
pub const CHIP_ROWS: u32 = 1;

/// The attachment chip glyphs, shared by the pending strip, the media
/// block, the mention popup, and the viewer.
pub(crate) const CHIP_ICON_DOC: &str = "▤";
pub(crate) const CHIP_ICON_IMAGE: &str = "🖼";

/// Debug-friendly byte payload (a raw `Vec<u8>` would blow up event traces).
pub struct MediaBytes(pub Arc<[u8]>);

impl fmt::Debug for MediaBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MediaBytes({} bytes)", self.0.len())
    }
}

/// An escape payload that must reach the terminal outside the frame's cell
/// diff. Graphic-protocol protocols embed their transmission in a cell's
/// symbol when rendered the naive way; a megabyte base64 payload in a span is
/// sliced into screen-width pieces by the pane's `Paragraph` paint and
/// printed as text (a transparent/dark screenshot renders as a screen of
/// base64 `A`s), so graphic payloads are extracted from the cells and written
/// straight to the terminal instead.
#[derive(Debug)]
pub enum MediaWrite {
    /// Position-free bytes written between frames — kitty graphic
    /// transmissions. Their unicode-placeholder cells carry the placement, so
    /// no cursor positioning is involved (`q=2`: the terminal never answers).
    Raw(Vec<u8>),
    /// Bytes pasted at an absolute position — sixel/iTerm2 payloads placed at
    /// the screen coordinates the covered cells were painted at (used by the
    /// static fullscreen viewer, where nothing scrolls afterwards).
    Place { y: u16, x: u16, bytes: Vec<u8> },
}

/// One attachment's media; caches its decode on first executor use.
pub struct MediaImage {
    bytes: Rc<[u8]>,
    /// The decoded pixels — `None` after a failed attempt too, so an
    /// unrenderable byte blob never re-decodes per frame.
    decoded: Option<Rc<DynamicImage>>,
    /// The decoded image downscaled to a render width's display box, cached
    /// per `(width cells, cell size)`. Graphic-protocol transmits ride on the
    /// image's natural pixel size, so a screenshot received at 6000×4000px
    /// would push ~96MB of RGBA (128MB base64) into the terminal — far more
    /// than any terminal's kitty parser is happy with. Prescaling bounds the
    /// payload by what the display area can actually show.
    prescaled: Option<(u16, FontSize, Rc<DynamicImage>)>,
}

impl MediaImage {
    /// The decoded pixels, decoded once on first executor use (including
    /// failures: a broken image yields `None` from there on and the block
    /// shows its unloaded chip).
    fn decoded(&mut self) -> Option<Rc<DynamicImage>> {
        if self.decoded.is_none() {
            let decoded = ImageReader::new(std::io::Cursor::new(&self.bytes[..]))
                .with_guessed_format()
                .ok()
                .and_then(|reader| reader.decode().ok());
            self.decoded = decoded.map(Rc::new);
        }
        self.decoded.clone()
    }

    /// The decoded image, downscaled so it is no wider than `width` cells at
    /// the given cell metrics. Downscale-only: images smaller than the box
    /// pass through unchanged, keeping the existing halfblock output
    /// byte-identical (protocol renderers `Fit` anyway).
    fn prescaled(&mut self, width: u16, cell: FontSize) -> Option<Rc<DynamicImage>> {
        if let Some((w, c, img)) = &self.prescaled
            && *w == width
            && c.width == cell.width
            && c.height == cell.height
        {
            return Some(Rc::clone(img));
        }
        let decoded = self.decoded()?;
        let px_w = u32::from(cell.width) * u32::from(width);
        let img = if decoded.width() <= px_w || px_w == 0 {
            Rc::clone(&decoded)
        } else {
            let px_h = (decoded.height() * px_w / decoded.width()).max(1);
            Rc::new(decoded.resize_exact(px_w, px_h, image::imageops::FilterType::Triangle))
        };
        self.prescaled = Some((width, cell, Rc::clone(&img)));
        Some(img)
    }
}

/// A rendered image entry: the cells paint reads from for one image at one
/// content width, held in `MediaStore.rendered`.
struct MediaEntry {
    lines: Vec<Line<'static>>,
}

/// The chat pane's received media, keyed by content hash (shas arrive with
/// attachment metadata, so presence is testable without decoding). Every
/// mutation runs through `&mut` in executor contexts — update arms and the
/// loop's between-frames pass; paint (the media block's and the viewer's
/// `view`) only reads.
pub struct MediaStore {
    cell: FontSize,
    /// The configured image protocol: the pane clamps placement-based ones
    /// (their placements persist at old rows on scroll); the fullscreen
    /// viewer may use any of them (nothing scrolls there).
    protocol: ImageProtocol,
    /// Bumped whenever the configured cell size or protocol changes: image
    /// caches keyed on it re-render.
    cell_gen: u64,
    images: HashMap<String, MediaImage>,
    order: Vec<String>,
    total: usize,
    /// Escape payloads extracted from graphic-protocol renders, written to
    /// the terminal between frames (never through the cell diff).
    pending: Vec<MediaWrite>,
    /// Kitty image ids this store transmitted, by attachment hash, so a
    /// re-rendered or re-transmitted image deletes the terminal-side copy
    /// its new transmit replaces.
    sent: HashMap<String, u32>,
    /// Rendered image entries keyed `(sha256, content width)` — built by the
    /// executor pass, read purely by paint. A generation bump clears them.
    rendered: HashMap<(String, u16), MediaEntry>,
}

impl MediaStore {
    pub fn new(cell: FontSize) -> Self {
        Self {
            cell,
            protocol: ImageProtocol::Halfblocks,
            cell_gen: 0,
            images: HashMap::new(),
            order: Vec::new(),
            total: 0,
            pending: Vec::new(),
            sent: HashMap::new(),
            rendered: HashMap::new(),
        }
    }

    #[cfg(test)]
    pub fn cell_gen(&self) -> u64 {
        self.cell_gen
    }

    pub fn set_cell_size(&mut self, cell: FontSize) {
        if cell.width != self.cell.width || cell.height != self.cell.height {
            self.cell = cell;
            self.bump_and_drop_kitty_images();
        }
    }

    /// Switch the render protocol (from the `[ui.image] protocol` config):
    /// bumps the generation so cached renders rebuild.
    pub fn set_protocol(&mut self, protocol: ImageProtocol) {
        if protocol != self.protocol {
            self.protocol = protocol;
            self.bump_and_drop_kitty_images();
        }
    }

    /// Bump the render generation; every previously transmitted kitty image
    /// is scheduled for deletion, since the rebuild re-transmits under fresh
    /// ids and the terminal's stale copies would linger.
    fn bump_and_drop_kitty_images(&mut self) {
        self.cell_gen += 1;
        for id in self.sent.drain().map(|(_, id)| id) {
            self.pending.push(MediaWrite::Raw(kitty_delete(id)));
        }
        // Every render entry was built for the old generation: they rebuild
        // (and re-transmit their payloads) when the executor pass runs again.
        self.rendered.clear();
    }

    /// The configured protocol (the viewer's unrestricted choice).
    pub fn protocol(&self) -> ImageProtocol {
        self.protocol
    }

    /// The protocol the chat pane may render with: kitty's fixed variant
    /// plants ordinary placeholder cells (scroll-safe), but placement-based
    /// protocols — sixel, iTerm2 — leave their placements behind at old
    /// rows when the buffer scrolls, so the pane clamps them to halfblocks.
    pub fn pane_protocol(&self) -> ImageProtocol {
        match self.protocol {
            ImageProtocol::Kitty | ImageProtocol::Halfblocks => self.protocol,
            _ => ImageProtocol::Halfblocks,
        }
    }

    /// Whether the store serves this hash (the attachment metadata's sha256).
    pub fn has(&self, sha256: &str) -> bool {
        self.images.contains_key(sha256)
    }

    /// Queue a kitty transmit for between-frames delivery. Re-transmission
    /// for the same attachment first deletes the terminal's previous copy,
    /// and the id is remembered for shutdown cleanup. Executor-only: called
    /// from `ensure`, never from the paint path.
    fn push_kitty_transmit(&mut self, sha: &str, payload: Vec<u8>) {
        if let Some(id) = kitty_id_from_transmit(&payload) {
            let old = self.sent.insert(sha.to_string(), id);
            if let Some(old) = old {
                self.pending.push(MediaWrite::Raw(kitty_delete(old)));
            }
            self.pending.push(MediaWrite::Raw(payload));
        }
    }

    /// Queue a cursor-anchored payload paste for the static viewer.
    /// Executor-only: called from `ensure`, never from the paint path.
    fn push_placed_payload(&mut self, y: u16, x: u16, payload: Vec<u8>) {
        self.pending.push(MediaWrite::Place {
            y,
            x,
            bytes: payload,
        });
    }

    /// Take all queued between-frames writes (kitty transmits and placed
    /// payloads alike).
    pub(crate) fn take_writes(&mut self) -> Vec<MediaWrite> {
        std::mem::take(&mut self.pending)
    }

    /// Delete-sequences for every kitty image this store transmitted — used
    /// at shutdown so the terminal's image cache empties with the session.
    pub(crate) fn take_kitty_deletes(&mut self) -> Vec<Vec<u8>> {
        self.sent.drain().map(|(_, id)| kitty_delete(id)).collect()
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
            MediaImage {
                bytes: bytes.into(),
                decoded: None,
                prescaled: None,
            },
        );
        self.order.push(sha256);
        while self.total > MAX_MEDIA_BYTES && self.order.len() > 1 {
            let evicted = self.order.remove(0);
            if let Some(item) = self.images.remove(&evicted) {
                self.total -= item.bytes.len();
                // An evicted image leaves a terminal-side kitty copy behind:
                // delete it and drop its render entries (paint falls back to
                // the unloaded chip until the image is re-requested).
                if let Some(id) = self.sent.remove(&evicted) {
                    self.pending.push(MediaWrite::Raw(kitty_delete(id)));
                }
                self.rendered.retain(|(sha, _), _| sha != &evicted);
            }
        }
    }

    /// Drop the stored render entries for one sha: its content arrived (or a
    /// failed attempt was replaced), so paint rebuilds whatever it showed.
    pub(crate) fn drop_rendered(&mut self, sha256: &str) {
        self.rendered.retain(|(sha, _), _| sha != sha256);
    }

    /// The chat pane's executor: render `sha` at `width` through the pane
    /// ladder when no entry exists yet, queueing the kitty transmit at
    /// creation. Runs between frames, where `&mut` is legitimately held;
    /// paint only reads entries.
    pub(crate) fn ensure_pane(&mut self, sha256: &str, width: u16) -> Option<()> {
        let ladder = pane_ladder(self.pane_protocol());
        self.ensure(&ladder, sha256, width, None)
    }

    /// The fullscreen viewer's executor: the unclamped ladder, and placement
    /// payloads (sixel/iTerm2) anchor at the image area's origin.
    pub(crate) fn ensure_full(
        &mut self,
        sha256: &str,
        width: u16,
        place: (u16, u16),
    ) -> Option<()> {
        let ladder = viewer_ladder(self.protocol());
        self.ensure(&ladder, sha256, width, Some(place))
    }

    /// Build the `(sha, width)` render entry when it is missing; its
    /// transmission payload queues exactly once, at creation (later runs hit
    /// the entry map and do nothing). Returns `None` when the media is
    /// absent, undecodable, or no ladder protocol constructs.
    fn ensure(
        &mut self,
        ladder: &[ImageProtocol],
        sha256: &str,
        width: u16,
        place: Option<(u16, u16)>,
    ) -> Option<()> {
        if self.rendered.contains_key(&(sha256.to_string(), width)) {
            return Some(());
        }
        let item = self.images.get_mut(sha256)?;
        let decoded = item.prescaled(width, self.cell)?;
        let render = render_through(ladder, &decoded, self.cell, width)?;
        if let Some(payload) = render.payload {
            match (render.protocol, place) {
                (ImageProtocol::Kitty, _) => self.push_kitty_transmit(sha256, payload),
                (_, Some((y, x))) => self.push_placed_payload(y, x, payload),
                // The pane ladder's only payload-carrying protocol is kitty;
                // halfblocks renders plain cells.
                (_, None) => {}
            }
        }
        self.rendered.insert(
            (sha256.to_string(), width),
            MediaEntry {
                lines: render.lines,
            },
        );
        Some(())
    }
}

/// A picker for the given font metrics and protocol (deprecated
/// constructor — the only one that takes a font size without querying
/// stdio).
fn picker_for(cell: FontSize, protocol: ProtocolType) -> Picker {
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(cell);
    picker.set_protocol_type(protocol);
    picker
}

/// The config enum → the picker's protocol type — the single mapping point
/// (the pane, the viewer and the tests all render through it).
pub fn protocol_type(protocol: ImageProtocol) -> ProtocolType {
    match protocol {
        ImageProtocol::Sixel => ProtocolType::Sixel,
        ImageProtocol::Kitty => ProtocolType::Kitty,
        ImageProtocol::Iterm2 => ProtocolType::Iterm2,
        ImageProtocol::Halfblocks => ProtocolType::Halfblocks,
    }
}

/// The chat pane's ladder: kitty is the pane's one scroll-safe graphical
/// protocol (its fixed variant plants ordinary placeholder cells); every
/// other start clamps to halfblocks.
fn pane_ladder(protocol: ImageProtocol) -> Vec<ImageProtocol> {
    match protocol {
        ImageProtocol::Kitty => vec![ImageProtocol::Kitty, ImageProtocol::Halfblocks],
        _ => vec![ImageProtocol::Halfblocks],
    }
}

/// The fullscreen viewer's ladder: the configured protocol first, then the
/// priority chain down to halfblocks — kitty → sixel → iterm2 → halfblocks.
fn viewer_ladder(protocol: ImageProtocol) -> Vec<ImageProtocol> {
    let mut ladder = vec![protocol];
    ladder.extend(successors(protocol.degrade(), |p| p.degrade()));
    ladder
}

/// The kitty unicode placeholder character (the payload marker this module
/// splits graphic transmissions out at).
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// A graphic-protocol render: the pane-safe cell lines plus the transmission
/// payload to deliver outside the diff, when the protocol has one.
pub struct MediaRender {
    /// The protocol that actually rendered (the ladder's winner).
    pub protocol: ImageProtocol,
    pub lines: Vec<Line<'static>>,
    pub payload: Option<Vec<u8>>,
}

/// The cells paint reads for one image attachment at one content width:
/// `None` keeps the block's unloaded chip (media absent/undecodable, or an
/// entry not yet built by the executor pass). Pure — the render itself is
/// `MediaStore`'s executor methods.
pub fn image_lines(store: &MediaStore, sha256: &str, width: u16) -> Option<Vec<Line<'static>>> {
    let entry = store.rendered.get(&(sha256.to_string(), width))?;
    Some(entry.lines.clone())
}

/// Walk `ladder` in priority order, rendering through the first protocol
/// that constructs. The decoded pixels are cloned per attempt — a
/// one-copy-per-protocol-attempt cost.
fn render_through(
    ladder: &[ImageProtocol],
    image: &DynamicImage,
    cell: FontSize,
    width: u16,
) -> Option<MediaRender> {
    if width == 0 {
        return None;
    }
    for protocol in ladder {
        let picker = picker_for(cell, protocol_type(*protocol));
        let proto = picker
            .new_protocol(
                image.clone(),
                Size::new(width, u16::MAX),
                ratatui_image::Resize::Fit(None),
            )
            .ok();
        if let Some(proto) = proto {
            let mut lifted = lift_lines(&proto);
            lifted.protocol = *protocol;
            return Some(lifted);
        }
    }
    None
}

/// Render the protocol into a scratch buffer of exactly its own size and
/// lift every cell to an owned styled span (the segment painter fills the
/// rest of the row with the block background).
///
/// Graphic protocols embed their transmission inside the first cell's
/// symbol; a megabyte payload must never ride through a span — the pane's
/// `Paragraph` paint would wrap-slice it into screen-width text cells and
/// print the base64. The payload is split off (after the kitty placeholder
/// char for kitty, the whole first symbol for sixel/iTerm2) and returned
/// separately; the cells keep only the small graphic references.
fn lift_lines(proto: &Protocol) -> MediaRender {
    let size = proto.size();
    let mut buf = Buffer::empty(Rect::new(0, 0, size.width.max(1), size.height.max(1)));
    ratatui_image::Image::new(proto).render(buf.area, &mut buf);
    let mut payload: Option<String> = None;
    let lines = (0..buf.area.height)
        .map(|y| {
            let spans = (0..buf.area.width)
                .map(|x| {
                    let cell = &buf[(x, y)];
                    let mut symbol = cell.symbol().to_string();
                    if symbol.contains('\x1b') {
                        // The one-shot transmission, planted in cell (0, 0).
                        let tail = match symbol.split_once(KITTY_PLACEHOLDER) {
                            // Kitty: the symbol continues with the
                            // placeholder cluster the terminal resolves.
                            Some((seq, rest)) => {
                                payload = Some(seq.to_string());
                                rest.to_string()
                            }
                            // Sixel/iTerm2: the payload is the whole symbol
                            // and no cell text remains.
                            None => {
                                payload = Some(std::mem::take(&mut symbol));
                                String::new()
                            }
                        };
                        symbol = tail;
                    }
                    Span::styled(symbol, Style::new().fg(cell.fg).bg(cell.bg))
                })
                .collect::<Vec<_>>();
            Line::from(spans)
        })
        .collect();
    MediaRender {
        protocol: ImageProtocol::Halfblocks,
        lines,
        payload: payload.map(String::into_bytes),
    }
}

/// The kitty image id a transmit names (`i=`), for later deletion.
fn kitty_id_from_transmit(payload: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(payload).ok()?;
    let start = text.find("i=")? + 2;
    let end = text[start..].find(',')? + start;
    text[start..end].parse().ok()
}

/// Delete a previously transmitted kitty image by id (`q=2`: no response).
fn kitty_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d={id},q=2;\x1b\\").into_bytes()
}

/// Write one queued [`MediaWrite`] to the terminal: kitty transmissions go
/// out as-is, placed payloads after an absolute cursor move to their cell
/// coordinates (one-based). The next frame's diff repositions the cursor
/// per cell, so nothing needs restoring.
pub(crate) fn write_media_write<W: io::Write>(
    terminal: &mut W,
    media_write: MediaWrite,
) -> io::Result<()> {
    match media_write {
        MediaWrite::Raw(bytes) => terminal.write_all(&bytes),
        MediaWrite::Place { y, x, bytes } => {
            write!(terminal, "\x1b[{};{}H", u32::from(y) + 1, u32::from(x) + 1)?;
            terminal.write_all(&bytes)
        }
    }
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

    /// Executor-then-read: render through the pane ladder and return what
    /// paint would read back.
    pub(super) fn render(
        store: &mut MediaStore,
        sha256: &str,
        width: u16,
    ) -> Option<Vec<Line<'static>>> {
        store.ensure_pane(sha256, width);
        image_lines(store, sha256, width)
    }

    #[test]
    fn image_lines_renders_and_lifts_styled_cells() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("sha1".into(), gradient_png());
        let lines = render(&mut store, "sha1", 40).expect("renders");
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
            render(&mut store, "broken", 40).is_none(),
            "undecodable media stays unloaded"
        );
    }

    #[test]
    fn decode_failure_stays_none_across_repeated_renders() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("broken".into(), b"\x89PNG\r\n\x1a\ngarbage".to_vec());
        assert!(render(&mut store, "broken", 40).is_none());
        assert!(render(&mut store, "broken", 80).is_none());
    }

    #[test]
    fn pane_ladder_kitty_then_halfblocks_others_clamp() {
        use ImageProtocol as P;
        assert_eq!(
            pane_ladder(P::Kitty),
            vec![P::Kitty, P::Halfblocks],
            "kitty is the pane's one scroll-safe graphical protocol"
        );
        for clamped in [P::Sixel, P::Iterm2, P::Halfblocks] {
            assert_eq!(
                pane_ladder(clamped),
                vec![P::Halfblocks],
                "placement-based protocols never render in the pane"
            );
        }
    }

    #[test]
    fn viewer_ladder_descends_the_priority_chain() {
        use ImageProtocol as P;
        assert_eq!(
            viewer_ladder(P::Kitty),
            vec![P::Kitty, P::Sixel, P::Iterm2, P::Halfblocks]
        );
        assert_eq!(
            viewer_ladder(P::Sixel),
            vec![P::Sixel, P::Iterm2, P::Halfblocks]
        );
        assert_eq!(viewer_ladder(P::Iterm2), vec![P::Iterm2, P::Halfblocks]);
        assert_eq!(viewer_ladder(P::Halfblocks), vec![P::Halfblocks]);
    }

    #[test]
    fn ladder_renders_through_the_first_constructible_protocol() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.insert("sha1".into(), gradient_png());
        let has_halfblocks = |lines: &[Line<'static>]| {
            lines.iter().any(|line| {
                line.spans
                    .iter()
                    .any(|span| matches!(span.style.bg, Some(bg) if bg != Color::Reset))
            })
        };
        // First success wins: kitty ahead of halfblocks renders placeholder
        // cells and hands the transmission out of band; halfblocks ahead of
        // kitty wins there instead.
        let decoded = image::ImageReader::new(std::io::Cursor::new(gradient_png()))
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        let render = render_through(
            &[ImageProtocol::Kitty, ImageProtocol::Halfblocks],
            &decoded,
            FontSize::new(8, 16),
            40,
        )
        .expect("kitty constructs");
        assert_eq!(render.protocol, ImageProtocol::Kitty);
        assert!(
            render.payload.is_some(),
            "kitty yields its transmit payload"
        );
        assert!(
            render.lines.iter().any(|line| line
                .spans
                .iter()
                .any(|span| span.content.contains('\u{10EEEE}'))),
            "kitty renders unicode placeholder cells"
        );
        let render = render_through(
            &[ImageProtocol::Halfblocks, ImageProtocol::Kitty],
            &decoded,
            FontSize::new(8, 16),
            40,
        )
        .expect("halfblocks constructs");
        assert_eq!(render.protocol, ImageProtocol::Halfblocks);
        assert!(render.payload.is_none(), "halfblocks has no payload");
        assert!(has_halfblocks(&render.lines));
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

    use super::tests::{gradient_png, render};
    use super::{
        ImageProtocol, MediaStore, MediaWrite, kitty_delete, kitty_id_from_transmit,
        write_media_write,
    };
    use image::ImageEncoder;
    use ratatui::widgets::Paragraph;

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

    #[test]
    fn kitty_pane_render_extracts_the_transmit_from_the_cells() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.set_protocol(ImageProtocol::Kitty);
        store.insert("sha-k".into(), gradient_png());
        let lines = render(&mut store, "sha-k", 40).expect("renders");
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\x1b'),
                    "an escape payload leaked into a cell span"
                );
                assert!(
                    span.content.chars().count() <= 5,
                    "no wrap-blowing spans: {} chars",
                    span.content.chars().count()
                );
            }
        }
        let writes = store.take_writes();
        let [MediaWrite::Raw(payload)] = &writes[..] else {
            panic!("exactly one raw kitty transmit queued: {writes:?}")
        };
        let text = std::str::from_utf8(payload).expect("transmit is ascii/utf-8");
        assert!(payload.starts_with(b"\x1b_Gq=2,"), "APC start: {text:?}");
        assert!(payload.ends_with(b"\x1b\\"), "APC terminator");
        assert!(text.contains("a=T,U=1,f=32,t=d,"), "virtual placement");
        assert!(text.contains("s=64,v=32,"), "gradient 64×32 stays natural");
        assert!(
            payload
                .windows(2)
                .rposition(|window| window == b"\x1b\\")
                .map(|i| i + 2 == payload.len())
                .unwrap_or(false),
            "the payload ends at its final terminator"
        );
    }

    #[test]
    fn kitty_transmit_is_prescaled_to_the_display_box() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.set_protocol(ImageProtocol::Kitty);
        let big = image::ImageBuffer::from_fn(2000, 1200, |x, y| {
            image::Rgba([x as u8, y as u8, 30, 255])
        });
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
            .write_image(big.as_raw(), 2000, 1200, image::ExtendedColorType::Rgba8)
            .unwrap();
        store.insert("big".into(), bytes);
        render(&mut store, "big", 20).expect("renders");
        let writes = store.take_writes();
        let [MediaWrite::Raw(payload)] = &writes[..] else {
            panic!("one raw transmit queued")
        };
        let text = std::str::from_utf8(payload).expect("transmit is utf-8");
        // 20 cells × 8px/width cell = a 160px-wide display box; the height
        // follows the 2000:1200 aspect (96px).
        assert!(text.contains("s=160,v=96,"), "prescaled dims: {text:?}");
        assert!(
            payload.len() < 400 * 1024,
            "the transmit is bounded by the display box ({} bytes)",
            payload.len()
        );
    }

    /// A 2000×1200 RGBA image at a 20-cell display box must not wrap-slice a
    /// megabyte span through the pane's `Paragraph` paint: the same store,
    /// rendered twice (identical frame, then shifted by one row), streams
    /// the transmit exactly once and nothing but placeholder cells after.
    #[test]
    fn kitty_frames_and_scroll_stream_no_payload_text() {
        let (mut terminal, capture) = capture_terminal();
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.set_protocol(ImageProtocol::Kitty);
        let big = image::ImageBuffer::from_fn(2000, 1200, |x, y| {
            image::Rgba([x as u8, y as u8, 30, 255])
        });
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(std::io::Cursor::new(&mut bytes))
            .write_image(big.as_raw(), 2000, 1200, image::ExtendedColorType::Rgba8)
            .unwrap();
        store.insert("big".into(), bytes);
        let lines = render(&mut store, "big", 20).expect("renders");
        for write in store.take_writes() {
            write_media_write(&mut *capture.borrow_mut(), write).unwrap();
        }
        let mut paint_at = |y: u16| {
            let area = Rect::new(0, y, 80, 20);
            terminal
                .draw(|frame| Paragraph::new(lines.clone()).render(area, frame.buffer_mut()))
                .unwrap();
        };
        paint_at(2);
        let first = capture.borrow().len();
        let transmit_count = capture.borrow()[..first]
            .windows(3)
            .filter(|w| *w == b"\x1b_G")
            .count();
        assert!(transmit_count >= 1, "the first frame carries the transmit");
        paint_at(2);
        let unchanged = capture.borrow().len();
        // An identical frame streams only the backend's per-draw housekeeping
        // (a trailing SGR reset + cursor hide, ~9 bytes) — no cell payload.
        assert!(
            unchanged - first < 32,
            "an identical frame streams nothing payload-like ({} bytes: {})",
            unchanged - first,
            loggable(&capture.borrow()[first..unchanged])
        );
        paint_at(3);
        let scrolled = capture.borrow()[unchanged..].to_vec();
        assert!(
            !has(&scrolled, b"\x1b_G"),
            "a scrolled frame re-emits placeholders, not the transmit"
        );
        assert!(
            scrolled.len() < 20 * 1024,
            "scroll re-emission is placeholder-sized ({} bytes)",
            scrolled.len()
        );
    }

    #[test]
    fn gen_bump_and_retransmit_delete_stale_kitty_copies() {
        let mut store = MediaStore::new(FontSize::new(8, 16));
        store.set_protocol(ImageProtocol::Kitty);
        store.insert("sha".into(), gradient_png());
        render(&mut store, "sha", 40).expect("renders");
        let writes = store.take_writes();
        let [MediaWrite::Raw(first)] = &writes[..] else {
            panic!("one transmit queued")
        };
        let id = kitty_id_from_transmit(first).expect("transmit names its id");
        // A cell-size change schedules the delete of the just-transmitted id.
        store.set_cell_size(FontSize::new(10, 20));
        let writes = store.take_writes();
        let [MediaWrite::Raw(delete)] = &writes[..] else {
            panic!("one delete queued")
        };
        let text = std::str::from_utf8(delete).expect("delete is utf-8");
        assert!(
            text.contains(&format!("a=d,d={id},")),
            "the deleted id matches the transmit: {text:?}"
        );
        // Re-materializing the same image under the new generation queues
        // exactly one fresh transmit (a shutdown delete may also be queued).
        render(&mut store, "sha", 40).expect("renders");
        let writes = store.take_writes();
        assert!(
            writes
                .iter()
                .filter(|write| matches!(write, MediaWrite::Raw(p) if p != &kitty_delete(id)))
                .count()
                >= 1,
            "a fresh transmit is queued"
        );
    }

    #[test]
    fn placed_payload_write_prefixes_the_cursor_position() {
        let mut out = Vec::new();
        write_media_write(
            &mut out,
            MediaWrite::Place {
                y: 3,
                x: 10,
                bytes: b"payload".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(out, b"\x1b[4;11Hpayload");
        let mut out = Vec::new();
        write_media_write(&mut out, MediaWrite::Raw(b"raw".to_vec())).unwrap();
        assert_eq!(out, b"raw");
    }
}
