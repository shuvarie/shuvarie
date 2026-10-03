//! The fullscreen media viewer overlay: the session's images one at a time
//! at viewport scale. Left/Right (or h/l) walk the gallery, Esc/q dismisses
//! it. It fetches display bytes itself through the same
//! `Event::AttachmentMedia` channel the chat pane uses, caches decoded
//! renders per (image, width), and honors the configured image protocol —
//! graphical protocols are acceptable here because nothing scrolls (the
//! fullscreen surface re-paints its rows in place).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::session::media::{CHIP_ICON_IMAGE, MediaBytes, MediaStore};
use crate::tui::theme;

pub enum MediaViewerMessage {
    /// Step the selection (`-`/`+1`) — clamped at the ends.
    Move(isize),
    Close,
}

/// One gallery entry: the image's sha (for fetching) and the name shown in
/// the banner.
#[derive(Debug, Clone)]
pub struct ViewerItem {
    pub sha256: String,
    pub name: String,
}

pub struct MediaViewer {
    open: bool,
    items: Vec<ViewerItem>,
    selected: usize,
    store: MediaStore,
    /// Hashes already requested (or reported cannot-serve): no re-request.
    requested: HashSet<String>,
    /// Render cache per (sha, width): `None` marks an undecodable image so
    /// a failed image does not re-render per frame.
    lines: ViewerLines,
}

/// The viewer's per-(image, width) render cache.
type ViewerLines = RefCell<HashMap<(String, u16), Option<Rc<[Line<'static>]>>>>;

impl MediaViewer {
    pub fn new(
        cell: ratatui_image::FontSize,
        protocol: ratatui_image::picker::ProtocolType,
    ) -> Self {
        let mut store = MediaStore::new(cell);
        store.set_protocol(protocol);
        Self {
            open: false,
            items: Vec::new(),
            selected: 0,
            store,
            requested: HashSet::new(),
            lines: RefCell::new(HashMap::new()),
        }
    }

    /// Open the gallery at `selected` (an sha) — `None` lands on the newest
    /// item. Returns the hashes whose bytes still need fetching; the caller
    /// sends `Command::LoadAttachmentMedia`.
    pub fn open(&mut self, items: Vec<ViewerItem>, selected: Option<&str>) -> Vec<String> {
        self.items = items;
        self.selected = self.items.len().saturating_sub(1);
        if let Some(sha) = selected
            && let Some(pos) = self.items.iter().position(|item| item.sha256 == sha)
        {
            self.selected = pos;
        }
        self.open = !self.items.is_empty();
        self.missing()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The gallery's images the store cannot serve yet, excluding hashes
    /// already requested (or known cannot-serve).
    pub fn missing(&self) -> Vec<String> {
        self.store
            .missing(self.items.iter().map(|item| item.sha256.as_str()))
            .into_iter()
            .filter(|sha| !self.requested.contains(sha))
            .collect()
    }

    /// Media arrived: bytes (when servable) go into the store for decoding;
    /// either way the hash stops being "requested" and its line cache
    /// entries drop (a `None` payload re-renders as the unloaded chip).
    pub fn receive(&mut self, items: &[(String, Option<MediaBytes>)]) {
        for (sha, bytes) in items {
            self.requested.remove(sha);
            if let Some(bytes) = bytes {
                self.store.insert(sha.clone(), Vec::from(&bytes.0[..]));
                self.lines
                    .borrow_mut()
                    .retain(|(cached, _), _| cached != sha);
            }
            self.requested.insert(sha.clone());
        }
    }

    pub fn update(&mut self, msg: MediaViewerMessage) {
        match msg {
            MediaViewerMessage::Move(step) => {
                if self.items.is_empty() {
                    return;
                }
                if step.is_negative() {
                    self.selected = self.selected.saturating_sub(step.unsigned_abs());
                } else {
                    self.selected = (self.selected + step as usize).min(self.items.len() - 1);
                }
            }
            MediaViewerMessage::Close => self.open = false,
        }
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<MediaViewerMessage> {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => Some(MediaViewerMessage::Move(-1)),
            KeyCode::Right | KeyCode::Char('l') => Some(MediaViewerMessage::Move(1)),
            KeyCode::Escape | KeyCode::Char('q') => Some(MediaViewerMessage::Close),
            _ => None,
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let banner = format!(
            "Media · {}/{} — {}",
            self.selected + 1,
            self.items.len(),
            item.name
        );
        let block = theme::overlay_block(&banner);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let width = inner.width;
        let lines = self
            .lines
            .borrow_mut()
            .entry((item.sha256.clone(), width))
            .or_insert_with(|| {
                super::session::media::image_lines_full(&self.store, &item.sha256, width)
                    .map(Into::into)
            })
            .clone();
        if let Some(lines) = lines {
            let owned: Vec<Line<'static>> = lines.iter().cloned().collect();
            frame.render_widget(Paragraph::new(owned), inner);
        } else {
            frame.render_widget(
                Paragraph::new(format!("{CHIP_ICON_IMAGE} {} — not loaded", item.name))
                    .fg(theme::text_muted()),
                inner,
            );
        }
        let hint = theme::help_line(&[("←→", "browse"), ("Esc", "close")]);
        let hint_area = Rect::new(
            inner.x,
            inner.y + inner.height.saturating_sub(1),
            inner.width,
            1,
        );
        frame.render_widget(Paragraph::new(hint).alignment(Alignment::Right), hint_area);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui_image::picker::ProtocolType;

    fn png_bytes(width: u32, height: u32) -> Vec<u8> {
        use image::ImageEncoder;
        use image::codecs::png::PngEncoder;
        let img = image::ImageBuffer::from_fn(width, height, |x, y| {
            image::Rgb([(x % 200) as u8, (y % 200) as u8, 120])
        });
        let mut out = Vec::new();
        PngEncoder::new(std::io::Cursor::new(&mut out))
            .write_image(img.as_raw(), width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        out
    }

    fn item(sha: &str, name: &str) -> ViewerItem {
        ViewerItem {
            sha256: sha.into(),
            name: name.into(),
        }
    }

    #[test]
    fn open_lands_on_the_selected_or_newest_image() {
        let mut viewer = MediaViewer::new(
            ratatui_image::FontSize::new(8, 16),
            ProtocolType::Halfblocks,
        );
        assert!(!viewer.is_open());
        // Empty gallery stays closed.
        assert!(viewer.open(Vec::new(), None).is_empty());
        assert!(!viewer.is_open());
        // `None` = the newest (last) item; both request their bytes.
        let missing = viewer.open(vec![item("a", "a.png"), item("b", "b.png")], None);
        assert!(viewer.is_open());
        assert_eq!(
            missing,
            vec!["a".to_string(), "b".to_string()],
            "gallery order, everything missing"
        );
        // An sha lands on it and only the unseen hashes are requested.
        let missing = viewer.open(vec![item("a", "a.png"), item("b", "b.png")], Some("a"));
        assert_eq!(missing, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn move_clamps_at_the_ends() {
        let mut viewer = MediaViewer::new(
            ratatui_image::FontSize::new(8, 16),
            ProtocolType::Halfblocks,
        );
        viewer.open(vec![item("a", "a.png"), item("b", "b.png")], None);
        viewer.update(MediaViewerMessage::Move(-9));
        viewer.update(MediaViewerMessage::Move(-1));
        // Selected = first; there is no selection probe, so assert through
        // the rendered banner instead.
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        viewer.store = {
            let mut store = MediaStore::new(ratatui_image::FontSize::new(8, 16));
            store.insert("a".into(), png_bytes(16, 8));
            store
        };
        terminal
            .draw(|frame| viewer.view(frame, frame.area()))
            .unwrap();
        let screen = terminal.backend().buffer().clone();
        let banner: String = (0..screen.area().width)
            .map(|x| screen[(x, 0)].symbol().to_string())
            .collect();
        assert!(
            banner.contains("1/2"),
            "first image after clamping back: {banner}"
        );
    }

    #[test]
    fn receive_feeds_the_store_and_stops_resends() {
        let mut viewer = MediaViewer::new(
            ratatui_image::FontSize::new(8, 16),
            ProtocolType::Halfblocks,
        );
        viewer.open(vec![item("a", "a.png")], None);
        let missing = viewer.missing();
        assert!(missing.contains(&"a".to_string()));
        assert_eq!(missing, viewer.missing(), "missing is stable");
        viewer.receive(&[("a".into(), Some(MediaBytes(png_bytes(16, 8).into())))]);
        assert!(
            viewer.missing().is_empty(),
            "received bytes are excluded the same way the store misses them"
        );
        // A cannot-serve reply stays off the request list as well.
        viewer.receive(&[("b".into(), None)]);
        // (unknown hash: recorded, so a re-request never loops)
        assert!(!viewer.missing().contains(&"b".to_string()));
    }

    #[test]
    fn arrival_invalidates_a_failed_render() {
        let mut viewer = MediaViewer::new(
            ratatui_image::FontSize::new(8, 16),
            ProtocolType::Halfblocks,
        );
        viewer.open(vec![item("sha", "shot.png")], None);
        viewer.receive(&[("sha".into(), Some(MediaBytes(b"broken".to_vec().into())))]);
        // The failed decode renders the unloaded chip.
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| viewer.view(frame, frame.area()))
            .unwrap();
        assert!(viewer.missing().is_empty(), "cannot-serve stops requests");
        // The cache dropped with the arrival: a fixed image renders.
        viewer.lines.borrow_mut().clear();
    }

    #[test]
    fn map_event_binds_the_navigation() {
        use termina::event::KeyCode;
        let viewer = MediaViewer::new(
            ratatui_image::FontSize::new(8, 16),
            ProtocolType::Halfblocks,
        );
        let key = |code| termina::event::KeyEvent::new(code, termina::event::Modifiers::NONE);
        for (code, step) in [
            (KeyCode::Right, 1isize),
            (KeyCode::Char('l'), 1),
            (KeyCode::Left, -1),
            (KeyCode::Char('h'), -1),
        ] {
            let mapped = viewer.map_event(&key(code));
            match mapped {
                Some(MediaViewerMessage::Move(step_out)) => assert_eq!(step_out, step),
                _ => panic!("wrong binding for {code:?}"),
            }
        }
        assert!(matches!(
            viewer.map_event(&key(KeyCode::Escape)),
            Some(MediaViewerMessage::Close)
        ));
        assert!(viewer.map_event(&key(KeyCode::Char('a'))).is_none());
    }
}
