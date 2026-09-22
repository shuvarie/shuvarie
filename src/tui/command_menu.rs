use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, ListItem, Paragraph};
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::commands::{CommandAction, CommandEntry, CommandRef, default_commands};
use super::list::{render_list_item_line, scroll_offset_for};
use super::search::{Search, SearchMessage};
use super::theme;

pub enum CommandMenuMessage {
    Search(SearchMessage),
    Next,
    Prev,
    Run,
    Close,
    Resize { viewport_height: u16 },
}

pub struct CommandMenu {
    pub open: bool,
    pub commands: Vec<CommandEntry>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub offset: usize,
    viewport_height: u16,
    pub search: Search,
}

impl CommandMenu {
    pub fn new() -> Self {
        let commands = default_commands();
        let filtered = (0..commands.len()).collect();
        Self {
            open: false,
            commands,
            filtered,
            selected: 0,
            offset: 0,
            viewport_height: 0,
            search: Search::new(),
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.search.clear();
        self.search.active = true;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.commands.len(), |i| self.commands[i].name.to_string())
            .into_iter()
            .filter(|&i| self.commands[i].available)
            .collect();
        self.selected = 0;
        self.offset = 0;
        self.recompute_offset();
    }

    pub fn set_availability(&mut self, action: CommandAction, available: bool) {
        let action = CommandRef::Builtin(action);
        for cmd in &mut self.commands {
            if cmd.action == action {
                cmd.available = available;
            }
        }
        if self.open {
            self.refilter();
        }
    }

    /// Replace the custom-command suffix of the command list (builtins keep
    /// their availability state; custom commands are always available).
    pub fn set_custom_commands(&mut self, commands: &[shuvarie_core::CustomCommand]) {
        let builtins = default_commands().len();
        self.commands.truncate(builtins);
        self.commands
            .extend(commands.iter().map(CommandEntry::custom));
        if self.open {
            self.refilter();
        }
    }

    fn next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        let vh = self.viewport_height as usize;
        let len = self.filtered.len();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    fn selected_action(&self) -> Option<CommandRef> {
        let idx = self.selected;
        let cmd_idx = self.filtered.get(idx)?;
        Some(self.commands[*cmd_idx].action.clone())
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<CommandMenuMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(CommandMenuMessage::Next),
                KeyCode::Char('p') => Some(CommandMenuMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Escape => Some(CommandMenuMessage::Close),
            KeyCode::Down => Some(CommandMenuMessage::Next),
            KeyCode::Up => Some(CommandMenuMessage::Prev),
            KeyCode::Enter => Some(CommandMenuMessage::Run),
            KeyCode::Backspace => Some(CommandMenuMessage::Search(SearchMessage::Backspace)),
            KeyCode::Char(c) => Some(CommandMenuMessage::Search(SearchMessage::Input(c))),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: CommandMenuMessage) -> Option<CommandRef> {
        if !self.open && !matches!(msg, CommandMenuMessage::Close) {
            return None;
        }
        match msg {
            CommandMenuMessage::Close => {
                self.close();
            }
            CommandMenuMessage::Next => self.next(),
            CommandMenuMessage::Prev => self.prev(),
            CommandMenuMessage::Search(m) => {
                self.search.update(m);
                self.refilter();
            }
            CommandMenuMessage::Run => {
                if let Some(action) = self.selected_action() {
                    self.close();
                    return Some(action);
                }
            }
            CommandMenuMessage::Resize { viewport_height } => {
                if self.viewport_height != viewport_height {
                    self.viewport_height = viewport_height;
                    self.recompute_offset();
                }
            }
        }
        None
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let popup = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup);
        let block = theme::overlay_block("Command Menu");
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let [input_area, list_area, hint_area] =
            Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);

        self.search
            .view(frame, input_area, "Type to search commands");

        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            list_area.height as usize,
            self.filtered.len(),
        );
        let visible: Vec<ListItem> = self
            .filtered
            .iter()
            .enumerate()
            .skip(offset)
            .take(list_area.height as usize)
            .map(|(idx, &i)| {
                let cmd = &self.commands[i];
                let line = Line::from(vec![
                    Span::raw(format!("{:<20} ", cmd.name)).fg(theme::text()),
                    Span::raw(cmd.description.to_string()).fg(theme::text_muted()),
                ]);
                render_list_item_line(line, idx == self.selected)
            })
            .collect();
        frame.render_widget(ratatui::widgets::List::new(visible), list_area);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Enter", "run"),
                ("Esc", "close"),
                ("↑↓", "navigate"),
            ]))
            .fg(theme::text_muted()),
            hint_area,
        );
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pop_w = area.width * percent_x / 100;
    let pop_h = area.height * percent_y / 100;
    let x = area.x + (area.width.saturating_sub(pop_w)) / 2;
    let y = area.y + (area.height.saturating_sub(pop_h)) / 2;
    Rect::new(x, y, pop_w, pop_h)
}

impl Default for CommandMenu {
    fn default() -> Self {
        Self::new()
    }
}
