use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, ListItem, Padding};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::commands::{
    CommandAction, CommandEntry, CommandRef, TRIGGER_CHARS, default_commands, is_escaped,
};
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
    /// The query the current `filtered`/`selected` state was built from.
    query: String,
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
            query: String::new(),
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
        let action = CommandRef::Builtin(action);
        let mut changed = false;
        for cmd in &mut self.commands {
            if cmd.action == action {
                changed |= cmd.available != available;
                cmd.available = available;
            }
        }
        if changed && self.open {
            self.refilter();
        }
    }

    /// Replace the custom-command suffix of the command list. Builtins keep
    /// their availability state; custom commands are always available and
    /// sort after the builtins in name order.
    pub fn set_custom_commands(&mut self, commands: &[shuvarie_core::CustomCommand]) {
        let builtins = default_commands().len();
        self.commands.truncate(builtins);
        self.commands
            .extend(commands.iter().map(CommandEntry::custom));
        if self.open {
            self.refilter();
        }
    }

    /// Whether `action` is currently marked available.
    #[cfg(test)]
    pub fn available(&self, action: CommandAction) -> bool {
        self.commands
            .iter()
            .find(|cmd| cmd.action == CommandRef::Builtin(action))
            .is_some_and(|cmd| cmd.available)
    }

    /// Recompute open/filtered state from the current buffer text. Called on
    /// every `Session::update`, so re-syncing with an unchanged trigger and
    /// query must not disturb the selection (see `refilter`).
    pub fn sync(&mut self, buffer: &str) {
        match trigger_state(buffer) {
            Some((c, query)) => {
                if self.dismissed == Some(c) {
                    self.open = false;
                } else {
                    self.open = true;
                    self.trigger = c;
                    self.dismissed = None;
                    self.query = query.to_string();
                    self.refilter();
                }
            }
            None => {
                self.open = false;
                self.dismissed = None;
                self.query.clear();
            }
        }
    }

    pub fn dismiss(&mut self) {
        if self.open {
            self.dismissed = Some(self.trigger);
        }
    }

    /// Rebuild `filtered` from the current query. Resetting the selection on
    /// every call would make navigation impossible (`Session::update` re-syncs
    /// the menu before each message), so the selection is only reset when the
    /// filtered list actually changes — i.e. when the query or the command
    /// set/availability moved under it.
    fn refilter(&mut self) {
        let query = self.query.clone();
        let new_filtered = search::filter_indices(&query, self.commands.len(), |i| {
            self.commands[i].action.slash_alias()
        })
        .into_iter()
        .filter(|&i| self.commands[i].available)
        .collect();
        if new_filtered != self.filtered {
            self.selected = 0;
            self.offset = 0;
        }
        self.filtered = new_filtered;
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

    pub fn selected_action(&self) -> Option<CommandRef> {
        let cmd_idx = self.filtered.get(self.selected)?;
        Some(self.commands[*cmd_idx].action.clone())
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
            .bg(theme::overlay())
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
                        format!("{}{}", self.trigger, cmd.action.slash_alias())
                    ))
                    .fg(theme::accent())
                    .bold(),
                    Span::raw(cmd.description.to_string()).fg(theme::text_muted()),
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
            .filter_map(|&i| match &menu.commands[i].action {
                CommandRef::Builtin(action) => Some(*action),
                CommandRef::Custom { .. } => None,
            })
            .collect()
    }

    fn custom_names(menu: &SlashMenu) -> Vec<String> {
        menu.filtered
            .iter()
            .filter_map(|&i| match &menu.commands[i].action {
                CommandRef::Custom { name, .. } => Some(name.clone()),
                CommandRef::Builtin(_) => None,
            })
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
        assert!(acts.contains(&CommandAction::Replay));
        assert!(acts.contains(&CommandAction::Reload));
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
    fn re_sync_with_unchanged_buffer_keeps_selection() {
        let mut menu = SlashMenu::new();
        menu.sync("/");
        menu.next();
        // `Session::update` re-syncs the menu before handling every message;
        // the selection must survive that (otherwise navigation could never
        // move past the second row).
        menu.sync("/");
        assert_eq!(menu.selected, 1);
        menu.next();
        menu.sync("/");
        assert_eq!(menu.selected, 2);
        for _ in 0..(menu.filtered.len() + 5) {
            menu.sync("/");
            menu.next();
        }
        assert_eq!(menu.selected, menu.filtered.len() - 1);
        menu.sync("/");
        menu.prev();
        assert_eq!(menu.selected, menu.filtered.len() - 2);
    }

    #[test]
    fn query_change_resets_selection() {
        let mut menu = SlashMenu::new();
        menu.sync("/");
        menu.next();
        menu.next();
        menu.sync("/mo");
        assert_eq!(menu.selected, 0, "a changed query resets the selection");
    }

    #[test]
    fn availability_flip_refreshes_open_menu() {
        let mut menu = SlashMenu::new();
        menu.sync("/");
        let len = menu.filtered.len();
        menu.set_availability(CommandAction::Quit, false);
        assert_eq!(menu.filtered.len(), len - 1);
        assert!(menu.selected_action().is_some());
        menu.set_availability(CommandAction::Quit, true);
        assert_eq!(menu.filtered.len(), len);
    }

    #[test]
    fn custom_commands_refilter_with_open_query() {
        let mut menu = SlashMenu::new();
        menu.sync("/com");
        assert!(menu.filtered.is_empty(), "no builtin matches 'com'");
        menu.set_custom_commands(&[custom_command("commit", "Commit code", None)]);
        assert_eq!(custom_names(&menu), vec!["commit"]);
        assert_eq!(menu.selected, 0);
    }

    #[test]
    fn selected_action_resolves() {
        let mut menu = SlashMenu::new();
        menu.sync(":se");
        assert_eq!(
            menu.selected_action(),
            Some(CommandRef::Builtin(CommandAction::OpenSessionPicker))
        );
    }

    #[test]
    fn custom_commands_append_after_builtins() {
        let mut menu = SlashMenu::new();
        menu.set_custom_commands(&[
            custom_command("commit", "Commit code", None),
            custom_command("model", "Fallback model", None),
        ]);
        menu.sync("/");
        // Builtins first (all still present), then the custom commands in
        // name order.
        assert_eq!(actions(&menu).len(), CommandAction::ALL.len());
        assert_eq!(custom_names(&menu), vec!["commit", "model"]);
        // Re-setting replaces (never duplicates) the custom suffix.
        menu.set_custom_commands(&[custom_command("review", "Review", None)]);
        menu.sync("/");
        assert_eq!(custom_names(&menu), vec!["review"]);
        assert_eq!(actions(&menu).len(), CommandAction::ALL.len());
    }

    #[test]
    fn custom_commands_survive_availability_updates() {
        let mut menu = SlashMenu::new();
        menu.set_custom_commands(&[custom_command("commit", "Commit code", None)]);
        menu.set_availability(CommandAction::Quit, false);
        menu.sync("/");
        assert_eq!(custom_names(&menu), vec!["commit"]);
        assert!(
            !actions(&menu).contains(&CommandAction::Quit),
            "builtin availability still applies"
        );
    }

    fn custom_command(
        name: &str,
        title: &str,
        model: Option<&str>,
    ) -> shuvarie_core::CustomCommand {
        shuvarie_core::CustomCommand {
            name: name.to_string(),
            title: title.to_string(),
            model: model.map(ToOwned::to_owned),
            path: std::path::PathBuf::from("/tmp"),
        }
    }
}
