//! The tabs: one agent per tab, each with its own core task, workspace, and
//! session screen.
//!
//! A [`Tab`] owns the command sender of its core, the workspace that core runs
//! in, the scene its sessions start under, and — while the tab is in the
//! background — its session screen. There is exactly one screen slot on the
//! App, not one per tab: the active tab's screen is staged into `App::session`
//! while every background tab's is parked here (see `App::set_active`). That
//! keeps every message handler's `self.session` meaning "the session the user
//! is looking at", and makes the swap the only place tab identity matters.

use std::path::{Path, PathBuf};

use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::Command;
use shuvarie_core::scenes::SceneListEntry;
use tokio::sync::mpsc::Sender;
use unicode_width::UnicodeWidthStr;

use super::session::SessionScreen;
use super::theme;

/// A tab's identity. Allocated once and never reused, so a message in flight
/// can name a tab that has since been closed without landing in its successor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TabId(usize);

impl TabId {
    pub fn new(id: usize) -> Self {
        Self(id)
    }
}

/// One tab: a core task, the workspace it runs in, and the screen it owns while
/// it is not the active tab.
pub struct Tab {
    pub id: TabId,
    /// The workspace this tab's core resolves every path against, and what its
    /// sidebar shows.
    pub root: PathBuf,
    /// The scene this tab's sessions start under (`None` = built-in Default).
    pub scene: Option<String>,
    /// The scene set this tab's core reported. Scenes come from the config chain
    /// of the workspace, so two tabs in different projects do not share one.
    pub scenes: Vec<SceneListEntry>,
    /// The default scene this tab's core reported (`None` = built-in Default).
    pub scene_default: Option<String>,
    /// This tab's core.
    pub cmd_tx: Sender<Command>,
    /// The tab's screen, parked while another tab is active. The active tab is
    /// staged into `App::session` instead, so its entry is `None`.
    pub screen: Option<SessionScreen>,
}

/// The per-tab state the App keeps in its working slots while that tab is
/// staged (see `App::stage`): the screen, its scene, the scene set its core
/// reported, and the sender of its core. Everything else on the App is the
/// user's — a connection, the theme, a prefs dialog — and stays put across a
/// switch.
pub struct Staged {
    pub screen: SessionScreen,
    pub scene: Option<String>,
    pub scenes: Vec<SceneListEntry>,
    pub scene_default: Option<String>,
    pub cmd_tx: Sender<Command>,
}

/// What the bar calls a tab: the session title when the tab has one, else the
/// name of its workspace directory. `screen` is the tab's own screen, which for
/// the active tab is the one staged into `App::session`.
pub fn tab_label(screen: Option<&SessionScreen>, root: &Path) -> String {
    let title = screen
        .and_then(|screen| screen.session_title.as_deref())
        .and_then(|title| title.lines().next())
        .map(str::trim)
        .filter(|title| !title.is_empty());
    if let Some(title) = title {
        return title.to_string();
    }
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| root.display().to_string())
}

/// One entry of the bar: what to label it, whether its agent is busy, and
/// whether it is the tab being shown.
pub struct TabBarEntry {
    /// The tab's position in the strip, 0-based; rendered 1-based, because that
    /// is what `Alt+<n>` takes.
    pub position: usize,
    pub label: String,
    pub busy: bool,
    pub active: bool,
}

/// The bar's row height: one line. `App` only gives it room when there is more
/// than one tab, so a single-tab session paints exactly as it did before tabs.
pub const BAR_HEIGHT: u16 = 1;

/// Gap between two entries.
const GAP: usize = 1;
/// Space either side of an entry's text. The active tab is filled with a
/// background, so the padding is what makes it read as a tab rather than as a
/// highlighted word: a run of blanks under the same highlight.
const PAD: usize = 1;
/// What a collapsed run of tabs is drawn as.
const ELLIPSIS: &str = "…";

fn entry_text(entry: &TabBarEntry) -> String {
    let mut text = format!("{} {}", entry.position + 1, entry.label);
    if entry.busy {
        text.push_str(" ●");
    }
    text
}

/// An entry as it is painted: its text, padded so the active tab's background
/// surrounds it.
fn entry_span_text(entry: &TabBarEntry) -> String {
    let pad = " ".repeat(PAD);
    format!("{pad}{}{pad}", entry_text(entry))
}

/// Lays the bar out as one row of spans: `1 title  2 other`, the active entry in
/// the accent colour on the accent background, a busy tab marked with a dot, and
/// any tabs that do not fit collapsed into an ellipsis.
///
/// The window is anchored on the active tab and grows outward, so the tab the
/// user is looking at is never the one that gets dropped when the strip is
/// longer than the terminal is wide.
pub fn bar_spans(entries: &[TabBarEntry], width: u16) -> Vec<Span<'static>> {
    let width = usize::from(width);
    if width == 0 || entries.is_empty() {
        return Vec::new();
    }
    let texts: Vec<String> = entries.iter().map(entry_span_text).collect();
    let sizes: Vec<usize> = texts.iter().map(|text| text.width()).collect();

    // Everything fits: draw the whole strip.
    let full = sizes.iter().sum::<usize>() + GAP * (entries.len() - 1);
    let (start, end) = if full <= width {
        (0, entries.len())
    } else {
        // Anchor on the active entry, then take turns adding a neighbour on
        // either side while one still fits beside the ellipses the dropped run
        // will cost.
        let active = entries.iter().position(|entry| entry.active).unwrap_or(0);
        let mut start = active;
        let mut end = active + 1;
        let mut used = sizes[active];
        loop {
            // Every run of dropped tabs costs an ellipsis plus the gap before
            // it, so the room left for a neighbour shrinks as the window moves
            // away from either end of the strip.
            let dropped = usize::from(start > 0) + usize::from(end < entries.len());
            let reserve = dropped * (GAP + ELLIPSIS.width());
            // Right first, so a strip wider than the terminal shows what comes
            // *after* the active tab before what precedes it.
            if end < entries.len() && used + GAP + sizes[end] + reserve <= width {
                used += GAP + sizes[end];
                end += 1;
            } else if start > 0 && used + GAP + sizes[start - 1] + reserve <= width {
                used += GAP + sizes[start - 1];
                start -= 1;
            } else {
                break;
            }
        }
        (start, end)
    };

    let mut spans = Vec::with_capacity(end - start + 2);
    if start > 0 {
        spans.push(Span::raw(ELLIPSIS).fg(theme::text_muted()));
        spans.push(Span::raw(" ".repeat(GAP)).fg(theme::text_muted()));
    }
    for (offset, entry) in entries[start..end].iter().enumerate() {
        if offset > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        let text = &texts[start + offset];
        let span = Span::raw(text.clone());
        // The active tab carries a background, not just a colour: it is the one
        // the keys go to, and a filled block is what makes that obvious at a
        // glance from anywhere on the row. The others stay on the bar's own
        // background, dimmer the less they have to say.
        spans.push(if entry.active {
            span.fg(theme::accent()).bg(theme::accent_bg()).bold()
        } else if entry.busy {
            span.fg(theme::text_dim())
        } else {
            span.fg(theme::text_muted())
        });
    }
    if end < entries.len() {
        spans.push(Span::raw(" ".repeat(GAP)).fg(theme::text_muted()));
        spans.push(Span::raw(ELLIPSIS).fg(theme::text_muted()));
    }
    spans
}

/// Paints the bar across `area`.
pub fn view(frame: &mut Frame<'_>, area: Rect, entries: &[TabBarEntry]) {
    if area.is_empty() || entries.is_empty() {
        return;
    }
    let block = Block::new()
        .bg(theme::surface())
        .padding(Padding::new(1, 1, 0, 0));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let spans = bar_spans(entries, inner.width);
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(position: usize, label: &str, busy: bool, active: bool) -> TabBarEntry {
        TabBarEntry {
            position,
            label: label.to_string(),
            busy,
            active,
        }
    }

    /// The bar's text, for asserting on layout without a terminal.
    fn bar_text(entries: &[TabBarEntry], width: u16) -> String {
        bar_spans(entries, width)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_short_strip_lists_every_tab_with_its_number() {
        let entries = [entry(0, "one", false, true), entry(1, "two", false, false)];
        // Padded either side, so the active tab's background surrounds it.
        assert_eq!(bar_text(&entries, 40), " 1 one   2 two ");
        assert!(
            bar_spans(&entries, 40)
                .iter()
                .all(|span| { span.content.as_ref() != ELLIPSIS }),
            "nothing is dropped when the strip fits"
        );
    }

    /// The point of the padding: the tab the keys go to is the only one with a
    /// background, so it is distinguishable from anywhere on the row rather than
    /// only by the colour of its letters.
    #[test]
    fn only_the_active_entry_is_filled() {
        let entries = [
            entry(0, "first", false, false),
            entry(1, "second", true, true),
            entry(2, "third", false, false),
        ];
        for span in bar_spans(&entries, 80) {
            let text = span.content.as_ref();
            if text.contains("second") {
                assert_eq!(
                    span.style.bg,
                    Some(theme::accent_bg()),
                    "the active tab is filled"
                );
                assert_eq!(span.style.fg, Some(theme::accent()));
                // The fill reaches past the label: that is the padding.
                assert!(
                    text.starts_with(' ') && text.ends_with(' '),
                    "the highlight surrounds the label: {text:?}"
                );
            } else {
                assert_eq!(
                    span.style.bg, None,
                    "`{text}` sits on the bar's own background"
                );
            }
        }
    }

    #[test]
    fn the_active_entry_is_the_accent_one_and_a_busy_tab_is_marked() {
        let entries = [
            entry(0, "idle", false, false),
            entry(1, "working", true, true),
        ];
        let spans = bar_spans(&entries, 60);
        let active = spans
            .iter()
            .find(|span| span.content.contains("working"))
            .expect("the active entry is drawn");
        assert_eq!(active.style.fg, Some(theme::accent()));
        assert_eq!(active.style.add_modifier, Modifier::BOLD);
        assert!(
            active.content.contains('●'),
            "a busy tab carries the marker: {:?}",
            active.content
        );

        let idle = spans
            .iter()
            .find(|span| span.content.contains("idle"))
            .expect("the idle entry is drawn");
        assert_eq!(idle.style.fg, Some(theme::text_muted()));
    }

    #[test]
    fn a_strip_wider_than_the_bar_collapses_into_an_ellipsis() {
        let entries: Vec<TabBarEntry> =
            (0..8).map(|i| entry(i, "project", false, i == 0)).collect();
        let text = bar_text(&entries, 30);
        assert!(text.starts_with(" 1 project"), "{text}");
        assert!(text.ends_with(ELLIPSIS), "the tail is collapsed: {text}");
        assert!(
            !text.contains("8 project"),
            "the last tab is the one dropped: {text}"
        );
        assert!(text.width() <= 30, "the bar never overflows: {text}");
    }

    #[test]
    fn the_active_tab_survives_a_bar_narrower_than_the_strip() {
        // The active tab is last: a bar anchored at the start would drop it,
        // which would leave the user looking at a tab the bar does not show.
        let entries: Vec<TabBarEntry> =
            (0..8).map(|i| entry(i, "project", false, i == 7)).collect();
        let text = bar_text(&entries, 30);
        assert!(text.contains("8 project"), "{text}");
        assert!(text.starts_with(ELLIPSIS), "the head is collapsed: {text}");
        assert!(text.width() <= 30, "the bar never overflows: {text}");
    }

    #[test]
    fn a_label_falls_back_to_the_workspace_directory() {
        let root = PathBuf::from("/home/user/repos/shuvarie");
        assert_eq!(tab_label(None, &root), "shuvarie");
        // A filesystem root has no directory name to show, so the path stands.
        assert_eq!(tab_label(None, Path::new("/")), "/");
    }

    /// The fill has to reach the terminal, not just the span: `view` paints the
    /// bar over its own block background, so this reads the painted cells back.
    #[test]
    fn the_painted_bar_fills_the_active_tab_only() {
        use ratatui::{Terminal, backend::TestBackend};

        let entries = [
            entry(0, "first", false, false),
            entry(1, "second", false, true),
            entry(2, "third", false, false),
        ];
        let mut terminal = Terminal::new(TestBackend::new(60, 1)).unwrap();
        terminal
            .draw(|frame| view(frame, frame.area(), &entries))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        let at = |needle: &str| row.find(needle).expect("the label is painted");

        let filled = at("second");
        let before = at("first");
        let after = at("third");
        assert_eq!(
            buffer[(filled as u16, 0)].bg,
            theme::accent_bg(),
            "the active tab's cells carry its background: {row:?}"
        );
        assert_eq!(
            buffer[(before as u16, 0)].bg,
            theme::surface(),
            "an inactive tab keeps the bar's background"
        );
        assert_eq!(buffer[(after as u16, 0)].bg, theme::surface());
        // The bar's own trailing space is the block's background too, so the
        // strip reads as one row with one raised tab in it.
        assert_eq!(buffer[(buffer.area.width - 2, 0)].bg, theme::surface());
    }
}
