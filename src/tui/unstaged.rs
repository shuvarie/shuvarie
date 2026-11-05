//! The unstaged-files guard prompt.
//!
//! A prompt submitted while the workspace's worktree still holds unstaged
//! changes is held here instead of being sent: the turn would run against
//! uncommitted work, so the user picks between staging it (`git add --all`),
//! sending anyway, or going back to the composer.

use ratatui::layout::Alignment;
use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent};

use super::git_status::UnstagedFile;
use super::{popup, theme};
use crate::tui::utils::text::truncate_spans;

/// Rows the file list may take before the rest collapse into an ellipsis.
const MAX_FILE_ROWS: u16 = 12;

/// Widest the dialog gets, however wide the terminal is.
const MAX_WIDTH: u16 = 78;

/// Narrowest a dialog is still worth painting at.
const MIN_WIDTH: u16 = 24;

#[derive(Debug, PartialEq, Eq)]
pub enum UnstagedMessage {
    /// `s`: `git add --all`, then send.
    Stage,
    /// `c`: send without touching the worktree.
    Continue,
    /// `Esc`: abort the send and hand the text back to the composer.
    Abort,
}

/// A user turn ready to send: the resolved prompt, its attachment paths, its
/// per-turn model override, and the composer draft it came from.
///
/// The prompt is held *resolved* rather than as raw text so that answering the
/// prompt sends exactly what the submit would have: a custom command keeps the
/// `model` its frontmatter named, and a skill keeps its expansion, without the
/// submit being re-run (and re-expanded) behind the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSend {
    pub content: String,
    pub attachments: Vec<String>,
    pub model: Option<String>,
    /// The raw composer text the send came from, handed back when the prompt is
    /// aborted. `None` for a command launched from a menu, which has no draft.
    pub draft: Option<String>,
}

/// The held turn, plus what git reported when it was held.
struct Held {
    send: PendingSend,
    files: Vec<UnstagedFile>,
}

/// The unstaged-files guard. Modal while up: it owns the three answers and
/// swallows everything else.
pub struct UnstagedGuard {
    /// The turn being confirmed; `None` while the prompt is closed.
    held: Option<Held>,
}

impl UnstagedGuard {
    pub fn new() -> Self {
        Self { held: None }
    }

    /// Whether the prompt is up.
    pub fn active(&self) -> bool {
        self.held.is_some()
    }

    /// Hold a turn for confirmation against the worktree's unstaged files.
    pub fn hold(&mut self, send: PendingSend, files: Vec<UnstagedFile>) {
        self.held = Some(Held { send, files });
    }

    /// Close the prompt, returning the held turn (`None` when it was closed).
    pub fn take(&mut self) -> Option<PendingSend> {
        self.held.take().map(|held| held.send)
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<UnstagedMessage> {
        if !self.active() {
            return None;
        }
        match key.code {
            KeyCode::Escape => Some(UnstagedMessage::Abort),
            KeyCode::Char('s') => Some(UnstagedMessage::Stage),
            KeyCode::Char('c') => Some(UnstagedMessage::Continue),
            _ => None,
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        let Some(held) = &self.held else {
            return;
        };
        let rect = dialog_rect(area, held.files.len());
        if rect.width < MIN_WIDTH || rect.height < 4 {
            return;
        }
        popup::dialog(frame, rect, "Unstaged changes", false, |inner, buf| {
            let [body_area, help_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
            let lines = body_lines(&held.files, body_area.width, body_area.height as usize);
            Paragraph::new(lines).render(body_area, buf);
            Paragraph::new(theme::help_line(&[
                ("s", "stage & send"),
                ("c", "send anyway"),
                ("Esc", "cancel"),
            ]))
            .alignment(Alignment::Center)
            .render(help_area, buf);
        });
    }
}

impl Default for UnstagedGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// The dialog's geometry: centered on `area`, wide enough for a path, and tall
/// enough for the header, the (clipped) list, and the bindings.
fn dialog_rect(area: Rect, files: usize) -> Rect {
    let listed = files.min(MAX_FILE_ROWS as usize);
    // The ellipsis row when the list does not fit.
    let rows = listed + usize::from(files > listed);
    // Header, question, blank above and below the list, the bindings, and the
    // border ring.
    let height = (rows as u16 + 7).min(area.height);
    let width = area.width.saturating_sub(4).min(MAX_WIDTH);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

/// The dialog body: the warning, the question, and the file list clipped to
/// `max_rows` (the bindings paint outside it).
fn body_lines(files: &[UnstagedFile], width: u16, max_rows: usize) -> Vec<Line<'static>> {
    let width = width.max(1) as usize;
    let count = if files.len() == 1 {
        "1 file is".to_string()
    } else {
        format!("{} files are", files.len())
    };
    let mut lines = vec![
        Line::from(vec![
            Span::raw("▸ ").fg(theme::warning()),
            Span::raw(format!("{count} not staged"))
                .fg(theme::warning())
                .bold(),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::raw("Send this prompt anyway?").fg(theme::text_muted()),
        ]),
        Line::from(""),
    ];
    // The rows left for the list and the blank that separates it from the
    // bindings; a clipped list also spends one on the ellipsis.
    let room = max_rows.saturating_sub(lines.len() + 1);
    let listed = files.len().min(MAX_FILE_ROWS as usize);
    let shown = if listed > room {
        room.saturating_sub(1)
    } else {
        listed
    };
    for file in files.iter().take(shown) {
        lines.push(file_line(file, width));
    }
    if shown < files.len() {
        lines.push(Line::from(
            Span::raw(format!("… and {} more", files.len() - shown)).fg(theme::text_dim()),
        ));
    }
    lines.push(Line::from(""));
    lines.truncate(max_rows);
    lines
}

/// One file row: the porcelain code, tinted by what happened to the path, then
/// the path itself (truncated to the dialog's inner width).
fn file_line(file: &UnstagedFile, width: usize) -> Line<'static> {
    let color = if file.deleted() {
        theme::error()
    } else if file.untracked() || file.added() {
        theme::text_muted()
    } else {
        theme::warning()
    };
    let spans = vec![
        Span::raw(format!("{:<2} ", file.code())).fg(color),
        Span::raw(file.path.clone()).fg(theme::text()),
    ];
    Line::from(truncate_spans(spans, width))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::git_status::UnstagedKind;
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::empty())
    }

    fn files(count: usize) -> Vec<UnstagedFile> {
        (0..count)
            .map(|i| UnstagedFile {
                path: format!("src/file{i}.rs"),
                kind: UnstagedKind::Modified,
            })
            .collect()
    }

    fn send(content: &str) -> PendingSend {
        PendingSend {
            content: content.to_string(),
            attachments: Vec::new(),
            model: None,
            draft: Some(content.to_string()),
        }
    }

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.clone()).collect())
            .collect()
    }

    #[test]
    fn closed_prompt_maps_nothing() {
        let guard = UnstagedGuard::new();
        assert!(!guard.active());
        assert_eq!(guard.map_event(&key(KeyCode::Char('s'))), None);
    }

    #[test]
    fn the_three_answers_map_and_other_keys_fall_through() {
        let mut guard = UnstagedGuard::new();
        guard.hold(send("hello"), files(2));

        assert_eq!(
            guard.map_event(&key(KeyCode::Char('s'))),
            Some(UnstagedMessage::Stage)
        );
        assert_eq!(
            guard.map_event(&key(KeyCode::Char('S'))),
            None,
            "the stash binding is gone, so Shift+S maps nothing"
        );
        assert_eq!(
            guard.map_event(&key(KeyCode::Char('c'))),
            Some(UnstagedMessage::Continue)
        );
        assert_eq!(
            guard.map_event(&key(KeyCode::Escape)),
            Some(UnstagedMessage::Abort)
        );
        assert_eq!(guard.map_event(&key(KeyCode::Enter)), None);
        assert_eq!(guard.map_event(&key(KeyCode::Char('x'))), None);
    }

    #[test]
    fn take_hands_back_the_held_submit_once() {
        let mut guard = UnstagedGuard::new();
        guard.hold(send("ship it"), files(1));
        assert!(guard.active());
        assert_eq!(guard.take(), Some(send("ship it")));
        assert!(!guard.active());
        assert_eq!(guard.take(), None);
    }

    #[test]
    fn body_lists_the_files_under_the_warning() {
        let mut guard = UnstagedGuard::new();
        guard.hold(send("x"), files(2));
        let lines = body_lines(&guard.held.as_ref().unwrap().files, 40, 20);
        assert_eq!(
            text(&lines),
            vec![
                "▸ 2 files are not staged".to_string(),
                "  Send this prompt anyway?".to_string(),
                String::new(),
                "M  src/file0.rs".to_string(),
                "M  src/file1.rs".to_string(),
                String::new(),
            ]
        );
    }

    #[test]
    fn a_single_file_reads_singular() {
        let mut guard = UnstagedGuard::new();
        guard.hold(send("x"), files(1));
        let lines = body_lines(&guard.held.as_ref().unwrap().files, 40, 20);
        assert_eq!(text(&lines)[0], "▸ 1 file is not staged");
    }

    #[test]
    fn long_lists_clip_to_the_budget_with_an_ellipsis_row() {
        let all = files(30);
        let lines = body_lines(&all, 40, 20);
        assert!(lines.len() <= 20, "body fits the area: {}", lines.len());
        let rendered = text(&lines);
        assert_eq!(rendered[2], "");
        assert!(
            rendered.iter().any(|l| l.contains("… and")),
            "clipped list reports the remainder: {rendered:?}"
        );

        let short = body_lines(&all, 40, 6);
        assert_eq!(short.len(), 6, "never more than the area allows");
        assert!(text(&short).iter().any(|l| l.contains("… and")));
    }

    #[test]
    fn dialog_grows_with_the_list_and_stays_inside_the_area() {
        let area = Rect::new(0, 0, 120, 40);
        let small = dialog_rect(area, 1);
        let large = dialog_rect(area, 5);
        assert!(large.height > small.height);
        assert_eq!(small.width, MAX_WIDTH, "capped for very wide terminals");
        assert_eq!(
            small.x + small.width / 2,
            area.x + area.width / 2,
            "centered horizontally"
        );

        // A tall list and a short terminal: clipped to the area, still painted.
        let clipped = dialog_rect(Rect::new(0, 0, 60, 12), 50);
        assert_eq!(clipped.height, 12);
        assert!(clipped.y + clipped.height <= 12);
    }

    #[test]
    fn file_rows_tint_deletions_and_untracked_paths() {
        let deleted = UnstagedFile {
            path: "gone.rs".into(),
            kind: UnstagedKind::Deleted,
        };
        assert_eq!(
            file_line(&deleted, 40).spans[0].style.fg,
            Some(theme::error())
        );
        let untracked = UnstagedFile {
            path: "new.rs".into(),
            kind: UnstagedKind::Untracked,
        };
        assert_eq!(
            file_line(&untracked, 40).spans[0].style.fg,
            Some(theme::text_muted())
        );
    }

    #[test]
    fn a_long_path_is_truncated_to_the_dialog_width() {
        let file = UnstagedFile {
            path: "deeply/nested/directory/with/a/long/file/name.rs".into(),
            kind: UnstagedKind::Modified,
        };
        let line = file_line(&file, 20);
        let width: usize = line.spans.iter().map(Span::width).sum();
        assert!(width <= 20, "row is {width} cols wide");
    }
}
