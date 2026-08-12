use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, ListState, Paragraph};
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::search::{Search, SearchMessage};
use super::theme;

#[derive(Clone, Copy)]
pub enum CommandAction {
    OpenModelSelect,
    AddProvider,
}

#[derive(Clone)]
pub struct CommandEntry {
    pub name: &'static str,
    pub description: &'static str,
    pub action: CommandAction,
}

pub enum CommandMenuMessage {
    Search(SearchMessage),
    Next,
    Prev,
    Run,
    Close,
}

pub enum CommandMenuEffect {
    OpenModelSelect,
    AddProvider,
}

pub fn default_commands() -> Vec<CommandEntry> {
    vec![
        CommandEntry {
            name: "Select model",
            description: "Pick the active model",
            action: CommandAction::OpenModelSelect,
        },
        CommandEntry {
            name: "Add provider",
            description: "Add a new LLM provider",
            action: CommandAction::AddProvider,
        },
    ]
}

pub struct CommandMenu {
    pub open: bool,
    pub commands: Vec<CommandEntry>,
    pub filtered: Vec<usize>,
    pub state: ListState,
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
            state: ListState::default(),
            search: Search::new(),
        }
    }

    pub fn open(&mut self) {
        self.open = true;
        self.search.clear();
        self.search.active = true;
        self.refilter();
        self.state.select(Some(0));
    }

    pub fn close(&mut self) {
        self.open = false;
        self.search.clear();
    }

    fn refilter(&mut self) {
        self.filtered = self
            .search
            .filter_indices(self.commands.len(), |i| self.commands[i].name.to_string());
        if !self.filtered.is_empty() {
            self.state.select(Some(0));
        } else {
            self.state.select(None);
        }
    }

    pub fn handle_event(key: KeyEvent) -> Option<CommandMenuMessage> {
        if ctrl(&key) {
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

    pub fn update(&mut self, msg: CommandMenuMessage) -> Option<CommandMenuEffect> {
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
                    return match action {
                        CommandAction::OpenModelSelect => Some(CommandMenuEffect::OpenModelSelect),
                        CommandAction::AddProvider => Some(CommandMenuEffect::AddProvider),
                    };
                }
            }
        }
        None
    }

    fn next(&mut self) {
        if !self.filtered.is_empty() {
            let i = self.state.selected().unwrap_or(0);
            let next = (i + 1).min(self.filtered.len() - 1);
            self.state.select(Some(next));
        }
    }

    fn prev(&mut self) {
        if !self.filtered.is_empty() {
            let i = self.state.selected().unwrap_or(0);
            let prev = i.saturating_sub(1);
            self.state.select(Some(prev));
        }
    }

    fn selected_action(&self) -> Option<CommandAction> {
        let idx = self.state.selected()?;
        let cmd_idx = self.filtered.get(idx)?;
        Some(self.commands[*cmd_idx].action)
    }

    pub fn view(&mut self, frame: &mut Frame<'_>, area: Rect) {
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
            .view(frame, input_area, "Type to search commands…");

        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .map(|&i| {
                let cmd = &self.commands[i];
                Line::from(vec![
                    Span::raw(format!("{:<20} ", cmd.name)).fg(theme::TEXT),
                    Span::raw(cmd.description.to_string()).fg(theme::TEXT_MUTED),
                ])
                .into()
            })
            .collect();
        let list = List::new(items)
            .highlight_style(Style::new().bg(theme::ACCENT_BG).fg(theme::TEXT))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, list_area, &mut self.state);

        frame.render_widget(
            Paragraph::new(theme::help_line(&[
                ("Enter", "run"),
                ("Esc", "close"),
                ("↑↓", "navigate"),
            ]))
            .fg(theme::TEXT_MUTED),
            hint_area,
        );
    }
}

fn ctrl(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
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
