//! The `/manage-attachments` dialog: the composer's pending attachments as one
//! list the user can add to, reorder, and remove from.
//!
//! The list is the session's strip — this dialog owns only the cursor — so the
//! rows are the composer's own chips (same icons, same probe round-trip) and
//! every edit lands there immediately. There is nothing to confirm: closing the
//! dialog returns to the composer with the strip already updated.
//!
//! `Ctrl+A` hands off to the [`AttachFinder`](super::attach::AttachFinder), the
//! same fuzzy finder `/attach` opens, stacked over this dialog; its pick joins
//! the list and comes back here rather than closing the whole stack.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph, Widget};
use termina::event::{KeyCode, KeyEvent};

use super::add_provider::centered_rect;
use super::list::{render_list_item_line, scroll_offset_for};
use super::session::PendingAttachment;
use super::utils::{alt, ctrl};
use super::{popup, theme};

/// A message from this dialog: a cursor step, an edit to the strip, or the
/// core's resize notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageAttachmentsMessage {
    Next,
    Prev,
    First,
    Last,
    /// Ctrl+A: pick a file to append, through the `/attach` finder.
    Add,
    /// Ctrl+X / Delete: drop the selected attachment.
    Remove,
    /// Ctrl+Up / Alt+K: shift the selected attachment one row toward the top.
    MoveUp,
    /// Ctrl+Down / Alt+J: shift it one row toward the bottom.
    MoveDown,
    /// Enter / Escape / Ctrl+C: back to the composer, strip as it stands.
    Close,
    /// The dialog's list height changed (the render loop hands over the area it
    /// painted, like every other scrollable popup).
    Resize {
        viewport_height: u16,
    },
    /// A key the dialog has no use for: swallowed, so it never reaches the
    /// composer behind it.
    Swallow,
}

/// What a message asks the app to do with the strip. The dialog never touches
/// the list itself: it lives on the session, which also owns the probes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageAttachmentsEffect {
    /// Open the attach finder over the workspace root.
    Add,
    /// Drop the pending attachment at this strip index.
    Remove(usize),
    /// Move the pending attachment at `from` to `to`, shifting the rows between.
    Move { from: usize, to: usize },
    /// Close the dialog.
    Close,
}

/// The `/manage-attachments` dialog's cursor and scroll state. The list itself
/// is the session's pending attachments, so this model stays small and can
/// never drift from what the composer will send.
#[derive(Debug, Default)]
pub struct ManageAttachments {
    pub open: bool,
    selected: usize,
    offset: usize,
    /// The list area the last frame painted, for [`scroll_offset_for`].
    viewport_height: u16,
}

impl ManageAttachments {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open the dialog on the strip's first row.
    pub fn open(&mut self) {
        self.open = true;
        self.selected = 0;
        self.offset = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.selected = 0;
        self.offset = 0;
    }

    /// The strip index the dialog is pointing at (`None` on an empty strip).
    pub fn selected(&self, len: usize) -> Option<usize> {
        (self.selected < len).then_some(self.selected)
    }

    /// Handle a message. `len` is the strip's length: the dialog reads the list
    /// through the session, so the length comes in with every message rather
    /// than being cached here (a stale copy would let the cursor point past the
    /// end after a removal).
    pub fn update(
        &mut self,
        message: ManageAttachmentsMessage,
        len: usize,
    ) -> Option<ManageAttachmentsEffect> {
        if !self.open && !matches!(message, ManageAttachmentsMessage::Close) {
            return None;
        }
        self.clamp(len);
        let selected = self.selected;
        match message {
            ManageAttachmentsMessage::Next => {
                if len > 0 {
                    self.selected = (self.selected + 1).min(len - 1);
                    self.recompute_offset(len);
                }
                None
            }
            ManageAttachmentsMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
                self.recompute_offset(len);
                None
            }
            ManageAttachmentsMessage::First => {
                self.selected = 0;
                self.recompute_offset(len);
                None
            }
            ManageAttachmentsMessage::Last => {
                self.selected = len.saturating_sub(1);
                self.recompute_offset(len);
                None
            }
            ManageAttachmentsMessage::Add => Some(ManageAttachmentsEffect::Add),
            ManageAttachmentsMessage::Remove => {
                (selected < len).then_some(ManageAttachmentsEffect::Remove(selected))
            }
            // A move at either end of the strip is not an edit, so a held-down
            // reorder key does not ask the core to re-probe an unchanged set.
            ManageAttachmentsMessage::MoveUp if selected > 0 && selected < len => {
                // The cursor travels with the row, so the next keystroke moves
                // the same attachment (and one back undoes the move).
                self.selected = selected - 1;
                self.recompute_offset(len);
                Some(ManageAttachmentsEffect::Move {
                    from: selected,
                    to: selected - 1,
                })
            }
            ManageAttachmentsMessage::MoveDown if selected + 1 < len => {
                self.selected = selected + 1;
                self.recompute_offset(len);
                Some(ManageAttachmentsEffect::Move {
                    from: selected,
                    to: selected + 1,
                })
            }
            ManageAttachmentsMessage::MoveUp | ManageAttachmentsMessage::MoveDown => None,
            ManageAttachmentsMessage::Close => {
                self.close();
                Some(ManageAttachmentsEffect::Close)
            }
            ManageAttachmentsMessage::Resize { viewport_height } => {
                self.viewport_height = viewport_height;
                self.recompute_offset(len);
                None
            }
            ManageAttachmentsMessage::Swallow => None,
        }
    }

    /// Keep the cursor inside a list that shrank underneath it (a removal, or a
    /// strip a probe reply emptied).
    fn clamp(&mut self, len: usize) {
        if len > 0 {
            self.selected = self.selected.min(len - 1);
        } else {
            self.selected = 0;
        }
    }

    fn recompute_offset(&mut self, len: usize) {
        self.offset = scroll_offset_for(
            self.selected,
            self.offset,
            self.viewport_height as usize,
            len,
        );
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<ManageAttachmentsMessage> {
        use ManageAttachmentsMessage::*;
        if ctrl(key) {
            return Some(match key.code {
                KeyCode::Char('n') => Next,
                KeyCode::Char('p') => Prev,
                // Ctrl+Up/Down is the editor's "move line" chord; the alt
                // letters below are the same move for terminals that eat it.
                KeyCode::Up => MoveUp,
                KeyCode::Down => MoveDown,
                KeyCode::Char('a') => Add,
                KeyCode::Char('x') => Remove,
                KeyCode::Char('c') => Close,
                _ => Swallow,
            });
        }
        if alt(key) {
            return Some(match key.code {
                KeyCode::Char('k') => MoveUp,
                KeyCode::Char('j') => MoveDown,
                _ => Swallow,
            });
        }
        Some(match key.code {
            KeyCode::Down => Next,
            KeyCode::Up => Prev,
            KeyCode::Home => First,
            KeyCode::End => Last,
            KeyCode::Char('a') => Add,
            KeyCode::Delete => Remove,
            KeyCode::Enter | KeyCode::Escape | KeyCode::Char('q') => Close,
            _ => Swallow,
        })
    }

    /// Paint the dialog over the session's strip. `items` is the session's
    /// pending list, painted row for row exactly as the composer's strip
    /// paints its chips.
    pub fn view(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        dimmed: bool,
        items: &[PendingAttachment],
    ) {
        if !self.open {
            return;
        }
        // Wider than the finder it hands off to: the hint bar carries four
        // bindings and must not clip on an 80-column terminal.
        let popup = centered_rect(70, 60, area);
        let title = format!("Attachments ({})", items.len());
        popup::dialog(frame, popup, &title, dimmed, |inner, buf| {
            if inner.width == 0 || inner.height == 0 {
                return;
            }
            // One hint row under the list; the app's resize hook splits the same
            // way, so the cursor's scroll window matches what is painted.
            let [list_area, hint_area] = Layout::vertical([Min(0), Length(1)]).areas(inner);
            self.view_list(buf, list_area, items);
            Paragraph::new(theme::help_line(&[
                ("Ctrl+A", "add"),
                ("Ctrl+↑↓", "move"),
                ("Del", "remove"),
                ("Esc", "close"),
            ]))
            .fg(theme::text_muted())
            .render(hint_area, buf);
        });
    }

    fn view_list(
        &self,
        buf: &mut ratatui::buffer::Buffer,
        area: Rect,
        items: &[PendingAttachment],
    ) {
        if area.is_empty() {
            return;
        }
        let Some(selected) = self.selected(items.len()) else {
            Paragraph::new("No attachments — Ctrl+A attaches a file")
                .fg(theme::text_muted())
                .render(area, buf);
            return;
        };
        let offset = scroll_offset_for(selected, self.offset, area.height as usize, items.len());
        let rows: Vec<ListItem> = items
            .iter()
            .enumerate()
            .skip(offset)
            .take(area.height as usize)
            .map(|(idx, attachment)| render_list_item_line(attachment.line(), idx == selected))
            .collect();
        Widget::render(List::new(rows), area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use termina::event::Modifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, Modifiers::NONE)
    }

    fn edited(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn open() -> ManageAttachments {
        let mut dialog = ManageAttachments::new();
        dialog.open();
        dialog
    }

    #[test]
    fn the_cursor_walks_the_strip_and_stops_at_both_ends() {
        let mut dialog = open();
        assert_eq!(dialog.selected(3), Some(0));
        dialog.update(ManageAttachmentsMessage::Prev, 3);
        assert_eq!(dialog.selected(3), Some(0), "the first row is the floor");
        dialog.update(ManageAttachmentsMessage::Next, 3);
        dialog.update(ManageAttachmentsMessage::Next, 3);
        assert_eq!(dialog.selected(3), Some(2));
        dialog.update(ManageAttachmentsMessage::Next, 3);
        assert_eq!(dialog.selected(3), Some(2), "the last row is the ceiling");
        dialog.update(ManageAttachmentsMessage::First, 3);
        assert_eq!(dialog.selected(3), Some(0));
        dialog.update(ManageAttachmentsMessage::Last, 3);
        assert_eq!(dialog.selected(3), Some(2));
        // An empty strip has no row to point at.
        dialog.update(ManageAttachmentsMessage::Next, 0);
        assert_eq!(dialog.selected(0), None);
    }

    #[test]
    fn removing_asks_the_app_to_drop_the_selected_row() {
        let mut dialog = open();
        dialog.update(ManageAttachmentsMessage::Next, 3);
        assert_eq!(
            dialog.update(ManageAttachmentsMessage::Remove, 3),
            Some(ManageAttachmentsEffect::Remove(1))
        );
        // With the row gone the cursor is held inside the shorter list.
        assert_eq!(
            dialog.update(ManageAttachmentsMessage::Remove, 2),
            Some(ManageAttachmentsEffect::Remove(1))
        );
        let mut empty = open();
        assert_eq!(empty.update(ManageAttachmentsMessage::Remove, 0), None);
    }

    #[test]
    fn reordering_moves_the_selected_row_and_stops_at_the_ends() {
        let mut dialog = open();
        // The first row cannot move up, the last cannot move down.
        assert_eq!(dialog.update(ManageAttachmentsMessage::MoveUp, 3), None);
        dialog.update(ManageAttachmentsMessage::Last, 3);
        assert_eq!(dialog.update(ManageAttachmentsMessage::MoveDown, 3), None);
        // A move swaps with the neighbour and the cursor follows the row.
        assert_eq!(
            dialog.update(ManageAttachmentsMessage::MoveUp, 3),
            Some(ManageAttachmentsEffect::Move { from: 2, to: 1 })
        );
        assert_eq!(
            dialog.update(ManageAttachmentsMessage::MoveDown, 3),
            Some(ManageAttachmentsEffect::Move { from: 1, to: 2 })
        );
    }

    #[test]
    fn keys_map_to_the_strip_edits() {
        let dialog = open();
        let ctrl_key = |code| edited(code, Modifiers::CONTROL);
        let alt_key = |code| edited(code, Modifiers::ALT);
        assert_eq!(
            dialog.map_event(&ctrl_key(KeyCode::Char('a'))),
            Some(ManageAttachmentsMessage::Add)
        );
        assert_eq!(
            dialog.map_event(&ctrl_key(KeyCode::Up)),
            Some(ManageAttachmentsMessage::MoveUp)
        );
        assert_eq!(
            dialog.map_event(&ctrl_key(KeyCode::Down)),
            Some(ManageAttachmentsMessage::MoveDown)
        );
        // Alt letters are the same move, and every other alt chord is swallowed
        // rather than leaking to the composer.
        assert_eq!(
            dialog.map_event(&alt_key(KeyCode::Char('k'))),
            Some(ManageAttachmentsMessage::MoveUp)
        );
        assert_eq!(
            dialog.map_event(&alt_key(KeyCode::Char('j'))),
            Some(ManageAttachmentsMessage::MoveDown)
        );
        assert_eq!(
            dialog.map_event(&alt_key(KeyCode::Char('b'))),
            Some(ManageAttachmentsMessage::Swallow)
        );
        assert_eq!(
            dialog.map_event(&ctrl_key(KeyCode::Char('x'))),
            Some(ManageAttachmentsMessage::Remove)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Delete)),
            Some(ManageAttachmentsMessage::Remove)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Char('a'))),
            Some(ManageAttachmentsMessage::Add)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Down)),
            Some(ManageAttachmentsMessage::Next)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Up)),
            Some(ManageAttachmentsMessage::Prev)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Enter)),
            Some(ManageAttachmentsMessage::Close)
        );
        assert_eq!(
            dialog.map_event(&key(KeyCode::Escape)),
            Some(ManageAttachmentsMessage::Close)
        );
        // Typing goes nowhere: the dialog has no input line, so a bare letter
        // that means nothing here is swallowed.
        assert_eq!(
            dialog.map_event(&key(KeyCode::Char('z'))),
            Some(ManageAttachmentsMessage::Swallow)
        );
    }

    #[test]
    fn a_closed_dialog_ignores_everything_but_a_close() {
        let mut dialog = ManageAttachments::new();
        assert_eq!(dialog.update(ManageAttachmentsMessage::Add, 2), None);
        assert!(!dialog.open);
    }

    fn paint(dialog: &ManageAttachments, items: &[PendingAttachment]) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| dialog.view(frame, frame.area(), false, items))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn attachment(path: &str) -> PendingAttachment {
        PendingAttachment::from_path(path)
    }

    #[test]
    fn view_lists_the_strip_with_the_hint_bar() {
        let _guard = crate::tui::theme::lock_for_tests();
        let dialog = open();
        let items = vec![attachment("docs/plan.pdf"), attachment("shots/ui.png")];
        let painted = text(&paint(&dialog, &items));
        assert!(painted.contains("Attachments (2)"), "{painted}");
        assert!(painted.contains("docs/plan.pdf"), "{painted}");
        assert!(painted.contains("shots/ui.png"), "{painted}");
        assert!(painted.contains("Ctrl+A"), "{painted}");
        assert!(painted.contains("remove"), "{painted}");
    }

    #[test]
    fn view_says_how_to_fill_an_empty_strip() {
        let _guard = crate::tui::theme::lock_for_tests();
        let dialog = open();
        let painted = text(&paint(&dialog, &[]));
        assert!(painted.contains("No attachments"), "{painted}");
        assert!(painted.contains("Ctrl+A"), "{painted}");
    }

    #[test]
    fn view_marks_the_selected_row() {
        let _guard = crate::tui::theme::lock_for_tests();
        let mut dialog = open();
        dialog.update(ManageAttachmentsMessage::Next, 2);
        let items = vec![attachment("a.md"), attachment("b.md")];
        let painted = text(&paint(&dialog, &items));
        let selected_row = painted
            .lines()
            .find(|line| line.contains("b.md"))
            .unwrap()
            .to_string();
        assert!(selected_row.contains('▶'), "{painted}");
        let other_row = painted.lines().find(|line| line.contains("a.md")).unwrap();
        assert!(!other_row.contains('▶'), "{painted}");
    }
}
