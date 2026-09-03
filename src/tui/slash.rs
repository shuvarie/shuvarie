use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, ListItem, Padding};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::commands::{CommandAction, CommandEntry, TRIGGER_CHARS, default_commands, is_escaped};
use super::list::{render_list_item_line, scroll_offset_for};
use super::search;
use super::theme;

/// Maximum rows visible in the tooltip before it scrolls.
pub const MAX_VISIBLE: usize = 6;

pub enum SlashMessage {
    Next,
    Prev,
    Complete,
    Run,
    Dismiss,
}

pub struct SlashMenu {
    commands: Vec<CommandEntry>,
    open: bool,
    trigger: char,
    filtered: Vec<usize>,
    selected: usize,
    offset: usize,
    dismissed: Option<char>,
}

impl SlashMenu {
    pub fn new() -> Self {
        Self {
            commands: default_commands(),
            open: false,
            trigger: '/',
            filtered: Vec::new(),
            selected: 0,
            offset: 0,
            dismissed: None,
        }
    }

    pub fn active(&self) -> bool {
        self.open && !self.filtered.is_empty()
    }

    pub fn trigger_char(&self) -> char {
        self.trigger
    }

    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    /// Tooltip rect floating above the input area, clamped to the history
    /// pane so it never covers the title row or the input itself.
    pub fn popup_rect(&self, history: Rect, input: Rect) -> Rect {
        let visible = (self.filtered_len() as u16).min(MAX_VISIBLE as u16).max(1);
        let width = input.width.saturating_sub(4).clamp(20, 48);
        let height = (visible + 1).min(history.height.max(1));
        let y = input.y.saturating_sub(height).max(history.y);
        let height = input.y.saturating_sub(y);
        Rect::new(input.x + 2, y, width, height)
    }

    pub fn set_availability(&mut self, action: CommandAction, available: bool) {
        for cmd in &mut self.commands {
            if cmd.action == action {
                cmd.available = available;
            }
        }
    }

    /// Recompute open/filtered state from the current buffer text.
    pub fn sync(&mut self, buffer: &str) {
        match trigger_state(buffer) {
            Some((c, query)) => {
                if self.dismissed == Some(c) {
                    self.open = false;
                } else {
                    self.open = true;
                    self.trigger = c;
                    self.dismissed = None;
                    self.refilter(query);
                }
            }
            None => {
                self.open = false;
                self.dismissed = None;
            }
        }
    }

    pub fn dismiss(&mut self) {
        if self.open {
            self.dismissed = Some(self.trigger);
        }
    }

    fn refilter(&mut self, query: &str) {
        self.filtered = search::filter_indices(query, self.commands.len(), |i| {
            self.commands[i].action.slash_name().to_string()
        })
        .into_iter()
        .filter(|&i| self.commands[i].available)
        .collect();
        self.selected = 0;
        self.offset = 0;
        self.recompute_offset();
    }

    fn viewport(&self) -> usize {
        self.filtered.len().min(MAX_VISIBLE)
    }

    pub fn next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
            self.recompute_offset();
        }
    }

    pub fn prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport();
        let len = self.filtered.len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    pub fn selected_action(&self) -> Option<CommandAction> {
        let cmd_idx = self.filtered.get(self.selected)?;
        Some(self.commands[*cmd_idx].action)
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<SlashMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(SlashMessage::Next),
                KeyCode::Char('p') => Some(SlashMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Tab => Some(SlashMessage::Complete),
            KeyCode::Up => Some(SlashMessage::Prev),
            KeyCode::Down => Some(SlashMessage::Next),
            KeyCode::Enter => Some(SlashMessage::Run),
            KeyCode::Escape => Some(SlashMessage::Dismiss),
            _ => None,
        }
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.active() || area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let block = Block::new()
            .bg(theme::OVERLAY)
            .padding(Padding::new(1, 1, 0, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            inner.height as usize,
            self.filtered.len(),
        );
        let visible: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .skip(offset)
            .take(inner.height as usize)
            .map(|(idx, &i)| {
                let cmd = &self.commands[i];
                let line = Line::from(vec![
                    Span::raw(format!(
                        "{:<10}",
                        format!("{}{}", self.trigger, cmd.action.slash_name())
                    ))
                    .fg(theme::ACCENT)
                    .bold(),
                    Span::raw(cmd.description.to_string()).fg(theme::TEXT_MUTED),
                ]);
                render_list_item_line(line, idx == self.selected)
            })
            .collect();
        frame.render_widget(ratatui::widgets::List::new(visible), inner);
    }
}

/// `(trigger char, query)` when the buffer is a single `<trigger><query>`
/// token that is not escaped by a doubled prefix.
fn trigger_state(buffer: &str) -> Option<(char, &str)> {
    let first = buffer.chars().next()?;
    if !TRIGGER_CHARS.contains(&first) || is_escaped(buffer) {
        return None;
    }
    let query = &buffer[first.len_utf8()..];
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some((first, query))
}

impl Default for SlashMenu {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actions(menu: &SlashMenu) -> Vec<CommandAction> {
        menu.filtered
            .iter()
            .map(|&i| menu.commands[i].action)
            .collect()
    }

    #[test]
    fn opens_on_trigger_prefix() {
        let mut menu = SlashMenu::new();
        menu.sync("/");
        assert!(menu.active());
        assert_eq!(actions(&menu).len(), CommandAction::ALL.len());
        menu.sync(":mo");
        assert!(menu.active());
        assert_eq!(menu.trigger, ':');
        assert!(actions(&menu).contains(&CommandAction::OpenModelSelect));
    }

    #[test]
    fn stays_closed_on_escaped_prefix() {
        let mut menu = SlashMenu::new();
        menu.sync("//");
        assert!(!menu.active());
        menu.sync("::x");
        assert!(!menu.active());
    }

    #[test]
    fn closes_on_whitespace_or_non_trigger() {
        let mut menu = SlashMenu::new();
        menu.sync("/mo");
        assert!(menu.active());
        menu.sync("/mo del");
        assert!(!menu.open);
        menu.sync("hi");
        assert!(!menu.open);
        menu.sync("");
        assert!(!menu.open);
    }

    #[test]
    fn esc_dismisses_until_trigger_cleared() {
        let mut menu = SlashMenu::new();
        menu.sync("/mo");
        assert!(menu.active());
        menu.dismiss();
        menu.sync("/mod");
        assert!(!menu.open, "dismissed state persists while typing");
        menu.sync("/m");
        assert!(!menu.open);
        menu.sync("hi");
        assert!(!menu.open);
        menu.sync("/");
        assert!(menu.active(), "clearing the trigger re-arms the menu");
        menu.sync(":n");
        assert!(menu.active(), "the other trigger opens the menu");
    }

    #[test]
    fn filters_by_query_and_availability() {
        let mut menu = SlashMenu::new();
        menu.set_availability(CommandAction::UndoLastTurn, false);
        menu.sync("/re");
        let acts = actions(&menu);
        assert!(acts.contains(&CommandAction::Redo));
        assert!(acts.contains(&CommandAction::Replay));
        menu.sync(":undo");
        assert!(
            actions(&menu).is_empty(),
            "unavailable commands stay hidden"
        );
        menu.set_availability(CommandAction::UndoLastTurn, true);
        menu.sync(":undo");
        assert!(actions(&menu).contains(&CommandAction::UndoLastTurn));
        menu.sync("/zzz");
        assert!(menu.filtered.is_empty());
        assert!(!menu.active());
    }

    #[test]
    fn selection_moves_and_wraps_clamped() {
        let mut menu = SlashMenu::new();
        menu.sync("/");
        menu.next();
        menu.next();
        assert_eq!(menu.selected, 2);
        menu.prev();
        assert_eq!(menu.selected, 1);
        for _ in 0..10 {
            menu.prev();
        }
        assert_eq!(menu.selected, 0);
        for _ in 0..20 {
            menu.next();
        }
        assert_eq!(menu.selected, menu.filtered.len() - 1);
    }

    #[test]
    fn selected_action_resolves() {
        let mut menu = SlashMenu::new();
        menu.sync(":se");
        assert_eq!(
            menu.selected_action(),
            Some(CommandAction::OpenSessionPicker)
        );
    }
}
