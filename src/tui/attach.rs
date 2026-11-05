//! The attachment picker dialog (`/attach [dir]`): a centred popup that fuzzy
//! finds the files under its search root with the core's FFF index. Typing
//! refines the query, Enter/Tab attach the highlighted file to the composer's
//! pending strip, Escape closes.
//!
//! `Ctrl+D` opens [`DirPrompt`], a small popup that edits the search root — the
//! picker's own version of the `/attach <dir>` argument, so browsing somewhere
//! else never means closing the dialog and retyping the command.
//!
//! The ranking round-trips through the core (the index lives there, built once
//! per directory), so the dialog follows the same token discipline as the other
//! searchable popups: one request carries the query, a reply for an older token
//! is dropped, and an answered query is never asked again.

use ratatui::layout::Constraint::{Length, Min};
use ratatui::prelude::*;
use ratatui::widgets::{List, ListItem, Paragraph, Widget};
use std::path::{Component, Path, PathBuf};
use termina::event::{KeyCode, KeyEvent};

use super::add_provider::centered_rect;
use super::components::{InputBuffer, flatten_newlines};
use super::list::{render_list_item_line, scroll_offset_for};
use super::utils::{alt, ctrl};
use super::{popup, theme};

pub enum AttachMessage {
    Input(char),
    /// A bracketed paste: appended to the query.
    Paste(String),
    Backspace,
    /// Clear the query (Ctrl+U).
    Clear,
    /// Edit the search root (Ctrl+D): the app opens [`DirPrompt`].
    EditDir,
    Next,
    Prev,
    /// Attach the highlighted file and close.
    Accept,
    Close,
    /// The core's answer to a ranking request (see [`AttachFinder::request`]).
    Candidates {
        token: u64,
        items: Vec<String>,
        error: Option<String>,
    },
    /// The dialog's list height changed (the render loop hands over the area it
    /// painted, like every other scrollable popup).
    Resize {
        viewport_height: u16,
    },
    /// A key the dialog has no use for: swallowed, so it never reaches the
    /// composer behind it.
    Swallow,
}

/// What a message asks the app to do.
#[derive(Debug, PartialEq, Eq)]
pub enum AttachEffect {
    /// Attach this path to the composer's pending set, then close.
    Accept(String),
    /// Open the directory prompt over the current search root.
    EditDir,
    /// Escape / Ctrl+C: close without attaching.
    Close,
}

/// The anchors the dialog needs to interpret and describe a search root.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// The tab's workspace: a relative path — the `/attach` argument, or what
    /// the directory prompt was given — resolves against it, and a root inside
    /// it is labelled relative to it.
    pub workspace: PathBuf,
    /// The home directory, for `~`-shortened labels of roots outside the
    /// workspace.
    pub home: Option<PathBuf>,
}

impl Roots {
    /// The directory a path argument names: no argument is the workspace
    /// itself, `~`/`~/<rest>` expand against home, a relative path resolves
    /// against the workspace, and an absolute one is taken as given.
    pub fn root_for(&self, arg: Option<&str>) -> PathBuf {
        let path = match arg.map(str::trim).filter(|arg| !arg.is_empty()) {
            None => return self.workspace.clone(),
            Some("~") => self.home.clone().unwrap_or_default(),
            Some(arg) => match arg.strip_prefix("~/") {
                Some(rest) => self
                    .home
                    .as_ref()
                    .map_or_else(|| PathBuf::from(arg), |home| home.join(rest)),
                None => PathBuf::from(arg),
            },
        };
        if path.is_absolute() || self.workspace.as_os_str().is_empty() {
            path
        } else {
            self.workspace.join(path)
        }
    }

    /// The root as the dialog titles it: workspace-relative while inside it
    /// (`.` for the workspace root itself), `~`-shortened elsewhere.
    pub fn label(&self, dir: &Path) -> String {
        if !self.workspace.as_os_str().is_empty()
            && let Ok(rel) = dir.strip_prefix(&self.workspace)
        {
            return if rel.as_os_str().is_empty() {
                ".".to_string()
            } else {
                rel.to_string_lossy().into_owned()
            };
        }
        if let Some(home) = &self.home {
            if dir == home {
                return "~".to_string();
            }
            if let Ok(rel) = dir.strip_prefix(home) {
                return format!("~/{}", rel.to_string_lossy());
            }
        }
        dir.to_string_lossy().into_owned()
    }
}

pub struct AttachFinder {
    /// Whether the dialog is up (it owns every key while it is).
    pub open: bool,
    /// The absolute directory the search is rooted at.
    dir: PathBuf,
    roots: Roots,
    query: String,
    /// The last reply's ranked paths, best match first.
    candidates: Vec<String>,
    /// Why the searched directory could not be indexed.
    error: Option<String>,
    /// The request awaiting its reply: its token and the query it carried.
    pending: Option<(u64, String)>,
    /// The query whose reply the current `candidates` came from. A re-sync must
    /// not re-request an answered query (that would ping-pong forever).
    answered: Option<String>,
    requests: u64,
    selected: usize,
    offset: usize,
    viewport_height: u16,
}

impl AttachFinder {
    pub fn new() -> Self {
        Self {
            open: false,
            dir: PathBuf::new(),
            roots: Roots::default(),
            query: String::new(),
            candidates: Vec::new(),
            error: None,
            pending: None,
            answered: None,
            requests: 0,
            selected: 0,
            offset: 0,
            viewport_height: 0,
        }
    }

    /// Open the dialog rooted at `dir` (an absolute path; see
    /// [`Roots::root_for`] for how a command argument becomes one).
    pub fn open(&mut self, dir: PathBuf, roots: Roots) {
        self.open = true;
        self.roots = roots;
        self.reroot(dir);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.pending = None;
    }

    /// The search root as the dialog titles it (what the directory prompt
    /// starts from).
    pub fn root_label(&self) -> String {
        self.roots.label(&self.dir)
    }

    /// Point the search at another directory — the directory prompt's answer.
    /// The query and the list belong to the old tree, so both are dropped and
    /// the next app hop ranks the new root.
    pub fn set_root(&mut self, dir: PathBuf) {
        self.reroot(dir);
    }

    /// The request a sync owes the core: `(token, dir, query)`. The directory
    /// travels absolute, so the core indexes exactly the tree the dialog is
    /// showing. `None` while the dialog is closed, or when the current query was
    /// already asked (in flight, or answered).
    pub fn request(&mut self) -> Option<(u64, Option<String>, String)> {
        if !self.open {
            return None;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, query)| *query == self.query)
            || self.answered.as_deref() == Some(self.query.as_str())
        {
            return None;
        }
        let token = self.requests;
        self.requests += 1;
        self.pending = Some((token, self.query.clone()));
        let dir = self.dir.to_string_lossy().into_owned();
        Some((token, Some(dir), self.query.clone()))
    }

    /// The reply to a request. Stale tokens (an older query, or a tree the
    /// dialog has already left) drop.
    pub fn set_candidates(&mut self, token: u64, items: Vec<String>, error: Option<String>) {
        let Some((request, query)) = self.pending.as_ref().map(|(t, q)| (*t, q.clone())) else {
            return;
        };
        if request != token {
            return;
        }
        self.pending = None;
        self.answered = Some(query);
        self.error = error;
        // Keep the highlight where it was when the ranking still holds it: a
        // keystroke that only appends narrows the list, and jumping back to the
        // top would lose the file being typed towards.
        let previous = self.candidates.get(self.selected).cloned();
        self.candidates = items;
        self.selected = previous
            .and_then(|path| self.candidates.iter().position(|c| *c == path))
            .unwrap_or(0);
        self.recompute_offset();
    }

    pub fn update(&mut self, message: AttachMessage) -> Option<AttachEffect> {
        if !self.open && !matches!(message, AttachMessage::Close) {
            return None;
        }
        match message {
            AttachMessage::Input(ch) => self.query.push(ch),
            AttachMessage::Paste(text) => {
                // A query is one line: a multi-line payload contributes its
                // first line rather than a stray newline.
                self.query
                    .push_str(text.lines().next().unwrap_or_default().trim());
            }
            AttachMessage::Backspace => {
                self.query.pop();
            }
            AttachMessage::Clear => self.query.clear(),
            AttachMessage::EditDir => return Some(AttachEffect::EditDir),
            AttachMessage::Next => self.next(),
            AttachMessage::Prev => self.prev(),
            AttachMessage::Accept => {
                if let Some(path) = self.accept() {
                    self.close();
                    return Some(AttachEffect::Accept(path));
                }
            }
            AttachMessage::Close => {
                self.close();
                return Some(AttachEffect::Close);
            }
            AttachMessage::Candidates {
                token,
                items,
                error,
            } => self.set_candidates(token, items, error),
            AttachMessage::Resize { viewport_height } => {
                self.viewport_height = viewport_height;
                self.recompute_offset();
            }
            AttachMessage::Swallow => {}
        }
        None
    }

    /// Re-root at `dir`, dropping everything that belonged to the old tree.
    fn reroot(&mut self, dir: PathBuf) {
        // Lexical `..`/`.` segments from a hand-typed path never reach the label
        // or the request: normalize without touching the disk.
        self.dir = normalize(&dir);
        self.query.clear();
        self.candidates.clear();
        self.error = None;
        self.pending = None;
        self.answered = None;
        self.selected = 0;
        self.offset = 0;
    }

    fn next(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = (self.selected + 1).min(self.candidates.len() - 1);
            self.recompute_offset();
        }
    }

    fn prev(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset();
        }
    }

    fn recompute_offset(&mut self) {
        self.offset = scroll_offset_for(
            self.selected,
            self.offset,
            self.viewport_height as usize,
            self.candidates.len(),
        );
    }

    /// The highlighted path, ready to become a pending attachment.
    fn accept(&self) -> Option<String> {
        self.candidates.get(self.selected).cloned()
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<AttachMessage> {
        if ctrl(key) {
            return Some(match key.code {
                KeyCode::Char('n') => AttachMessage::Next,
                KeyCode::Char('p') => AttachMessage::Prev,
                KeyCode::Char('u') => AttachMessage::Clear,
                KeyCode::Char('d') => AttachMessage::EditDir,
                KeyCode::Char('c') => AttachMessage::Close,
                _ => AttachMessage::Swallow,
            });
        }
        if alt(key) {
            return Some(AttachMessage::Swallow);
        }
        Some(match key.code {
            KeyCode::Up => AttachMessage::Prev,
            KeyCode::Down => AttachMessage::Next,
            KeyCode::Enter | KeyCode::Tab => AttachMessage::Accept,
            KeyCode::Escape => AttachMessage::Close,
            KeyCode::Backspace => AttachMessage::Backspace,
            KeyCode::Char(ch) => AttachMessage::Input(ch),
            _ => AttachMessage::Swallow,
        })
    }

    /// Paint the dialog. `dimmed` marks it as sitting underneath another popup.
    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        let popup = centered_rect(64, 60, area);
        let title = format!("Attach · {}", self.root_label());
        popup::dialog(frame, popup, &title, dimmed, |inner, buf| {
            let [query_area, list_area, hint_area] =
                Layout::vertical([Length(1), Min(0), Length(1)]).areas(inner);
            self.view_query(buf, query_area);
            self.view_list(buf, list_area);
            Paragraph::new(theme::help_line(&[
                ("Enter", "attach"),
                ("Ctrl+D", "directory"),
                ("Esc", "close"),
            ]))
            .fg(theme::text_muted())
            .render(hint_area, buf);
        });
    }

    fn view_query(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        let line = if self.query.is_empty() {
            Line::from(Span::raw("Type to search files").fg(theme::text_muted()))
        } else {
            Line::from(vec![
                Span::raw("› ").fg(theme::text_muted()),
                Span::raw(self.query.clone()).fg(theme::text()),
                Span::raw("▏").fg(theme::accent()),
            ])
        };
        Paragraph::new(line).render(area, buf);
    }

    fn view_list(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        if area.is_empty() {
            return;
        }
        if self.candidates.is_empty() {
            // Three empty-list reasons: a failure, a request still running, and
            // a query that matches nothing.
            let (text, color) = match (&self.error, self.pending.is_some()) {
                (Some(error), _) => (error.clone(), theme::warning()),
                (None, true) => ("searching…".to_string(), theme::text_muted()),
                (None, false) => ("no matching file".to_string(), theme::text_muted()),
            };
            Paragraph::new(text).fg(color).render(area, buf);
            return;
        }
        let offset = scroll_offset_for(
            self.selected,
            self.offset,
            area.height as usize,
            self.candidates.len(),
        );
        let visible: Vec<ListItem> = self
            .candidates
            .iter()
            .enumerate()
            .skip(offset)
            .take(area.height as usize)
            .map(|(idx, path)| {
                render_list_item_line(
                    Line::from(path.clone()).fg(theme::text()),
                    idx == self.selected,
                )
            })
            .collect();
        Widget::render(List::new(visible), area, buf);
    }
}

impl Default for AttachFinder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirPromptMessage {
    Input(char),
    Paste(String),
    Backspace,
    Delete,
    Left,
    Right,
    LeftWord,
    RightWord,
    Home,
    End,
    /// Move the highlighted hint (Up/Down).
    Next,
    Prev,
    /// Complete the highlighted hint into the input (Tab).
    AcceptHint,
    Submit,
    Cancel,
    /// The core's directory hints for the typed path.
    Hints {
        token: u64,
        items: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirPromptEffect {
    /// The prompt was submitted: `dir` is what was typed and `hint` the
    /// highlighted completion, which the app falls back to when `dir` is not a
    /// directory (so `docs` + Enter walks into `docs/`).
    Submit {
        dir: String,
        hint: Option<String>,
    },
    Close,
}

/// The directory prompt `Ctrl+D` opens over the picker: one line naming the
/// directory to search next, prefilled with the current root and resolved like
/// the `/attach` argument (`~`, relative against the workspace, or absolute).
pub struct DirPrompt {
    pub open: bool,
    pub buffer: InputBuffer,
    /// Why the last submit was refused (the path is not a directory), shown
    /// under the input until the text changes.
    pub error: Option<String>,
    /// The core's subdirectories matching the typed path, in its order.
    hints: Vec<String>,
    selected: usize,
    /// The hints request awaiting its reply: its token and the text it carried.
    pending: Option<(u64, String)>,
    /// The text whose hints are in `hints`; the same text is never asked twice.
    answered: Option<String>,
    requests: u64,
    /// The workspace the prompt opens on, shown as the placeholder.
    placeholder: String,
}

impl DirPrompt {
    pub fn new() -> Self {
        Self {
            open: false,
            buffer: InputBuffer::new(),
            error: None,
            hints: Vec::new(),
            selected: 0,
            pending: None,
            answered: None,
            requests: 0,
            placeholder: String::new(),
        }
    }

    /// Open the prompt prefilled with the picker's current root.
    pub fn open(&mut self, current: &str, workspace: &Path) {
        self.open = true;
        self.buffer.set(current);
        self.error = None;
        self.hints.clear();
        self.selected = 0;
        self.pending = None;
        self.answered = None;
        self.placeholder = workspace.to_string_lossy().into_owned();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.buffer.clear();
        self.error = None;
        self.hints.clear();
        self.selected = 0;
        self.pending = None;
        self.answered = None;
    }

    /// Refuse the last submit without closing: the text stays for editing.
    pub fn reject(&mut self, reason: String) {
        self.error = Some(reason);
    }

    /// The hints request the prompt owes the core: `(token, path)`. `None` while
    /// it is closed, or when the current text was already asked (in flight, or
    /// answered) — the app calls this after every edit, so a re-request would
    /// otherwise loop forever.
    pub fn hints_request(&mut self) -> Option<(u64, String)> {
        if !self.open {
            return None;
        }
        let typed = self.buffer.value.clone();
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, text)| *text == typed)
            || self.answered.as_deref() == Some(typed.as_str())
        {
            return None;
        }
        let token = self.requests;
        self.requests += 1;
        self.pending = Some((token, typed.clone()));
        Some((token, typed))
    }

    /// The core's hints for the typed path. A reply for an older request drops.
    pub fn set_hints(&mut self, token: u64, items: Vec<String>) {
        let Some((request, text)) = self.pending.as_ref().map(|(t, s)| (*t, s.clone())) else {
            return;
        };
        if request != token {
            return;
        }
        self.pending = None;
        self.answered = Some(text);
        self.hints = items;
        self.selected = 0;
    }

    /// The highlighted hint, when there is one.
    fn highlighted(&self) -> Option<String> {
        self.hints.get(self.selected).cloned()
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<DirPromptMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('b') => Some(DirPromptMessage::Left),
                KeyCode::Char('f') => Some(DirPromptMessage::Right),
                KeyCode::Char('a') => Some(DirPromptMessage::Home),
                KeyCode::Char('e') => Some(DirPromptMessage::End),
                KeyCode::Char('d') => Some(DirPromptMessage::Delete),
                KeyCode::Char('h') => Some(DirPromptMessage::Backspace),
                _ => None,
            };
        }
        if alt(key) {
            return match key.code {
                KeyCode::Char('b') => Some(DirPromptMessage::LeftWord),
                KeyCode::Char('f') => Some(DirPromptMessage::RightWord),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Enter => Some(DirPromptMessage::Submit),
            KeyCode::Escape => Some(DirPromptMessage::Cancel),
            KeyCode::Up => Some(DirPromptMessage::Prev),
            KeyCode::Down => Some(DirPromptMessage::Next),
            KeyCode::Tab => Some(DirPromptMessage::AcceptHint),
            KeyCode::Backspace => Some(DirPromptMessage::Backspace),
            KeyCode::Delete => Some(DirPromptMessage::Delete),
            KeyCode::Left => Some(DirPromptMessage::Left),
            KeyCode::Right => Some(DirPromptMessage::Right),
            KeyCode::Home => Some(DirPromptMessage::Home),
            KeyCode::End => Some(DirPromptMessage::End),
            KeyCode::Char(c) => Some(DirPromptMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, message: DirPromptMessage) -> Option<DirPromptEffect> {
        if !self.open {
            return None;
        }
        match message {
            DirPromptMessage::Input(c) => {
                self.buffer.push(c);
                self.error = None;
            }
            DirPromptMessage::Paste(text) => {
                self.buffer.insert_str(&flatten_newlines(&text));
                self.error = None;
            }
            DirPromptMessage::Backspace => {
                self.buffer.backspace();
                self.error = None;
            }
            DirPromptMessage::Delete => {
                self.buffer.delete();
                self.error = None;
            }
            DirPromptMessage::Left => self.buffer.left(),
            DirPromptMessage::Right => self.buffer.right(),
            DirPromptMessage::LeftWord => self.buffer.left_word(),
            DirPromptMessage::RightWord => self.buffer.right_word(),
            DirPromptMessage::Home => self.buffer.home(),
            DirPromptMessage::End => self.buffer.end(),
            DirPromptMessage::Next => {
                if !self.hints.is_empty() {
                    self.selected = (self.selected + 1).min(self.hints.len() - 1);
                }
            }
            DirPromptMessage::Prev => {
                self.selected = self.selected.saturating_sub(1);
            }
            DirPromptMessage::AcceptHint => {
                // Completing replaces the whole line: hints come back complete
                // in the form being typed, so the result is what gets searched.
                if let Some(hint) = self.highlighted() {
                    self.buffer.set(&hint);
                    self.error = None;
                    self.selected = 0;
                }
            }
            DirPromptMessage::Hints { token, items } => self.set_hints(token, items),
            DirPromptMessage::Submit => {
                let dir = self.buffer.value.trim().to_string();
                if dir.is_empty() {
                    self.close();
                    return Some(DirPromptEffect::Close);
                }
                let hint = self.highlighted();
                // The prompt stays open until the app confirms the directory:
                // a refusal is shown in place, under the text being edited.
                return Some(DirPromptEffect::Submit { dir, hint });
            }
            DirPromptMessage::Cancel => {
                self.close();
                return Some(DirPromptEffect::Close);
            }
        }
        None
    }

    fn input_line(&self) -> Line<'static> {
        let text_style = Style::new().fg(theme::text());
        let cursor_style = Style::new()
            .fg(theme::accent())
            .add_modifier(Modifier::REVERSED);
        let mut spans = Vec::new();
        let chars: Vec<char> = self.buffer.value.chars().collect();
        let cursor_idx = self.buffer.cursor_char_index();
        for (i, c) in chars.iter().enumerate() {
            let style = if i == cursor_idx {
                cursor_style
            } else {
                text_style
            };
            spans.push(Span::styled(c.to_string(), style));
        }
        if cursor_idx >= chars.len() {
            spans.push(Span::styled(" ".to_string(), cursor_style));
        }
        Line::from(spans)
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, dimmed: bool) {
        if !self.open {
            return;
        }
        // Taller than a one-line prompt: the hint rows under the input are the
        // point of it.
        let popup = centered_rect(56, 44, area);
        popup::dialog(frame, popup, "Attach Directory", dimmed, |inner, buf| {
            if inner.width == 0 || inner.height == 0 {
                return;
            }
            let [input_area, status_area, hints_area, help_area] =
                Layout::vertical([Length(1), Length(1), Min(0), Length(1)]).areas(inner);

            if self.buffer.value.is_empty() {
                let placeholder = if self.placeholder.is_empty() {
                    "Directory to search".to_string()
                } else {
                    self.placeholder.clone()
                };
                Paragraph::new(placeholder)
                    .fg(theme::text_muted())
                    .render(input_area, buf);
            } else {
                Paragraph::new(self.input_line()).render(input_area, buf);
            }

            let status = match &self.error {
                Some(error) => Line::from(error.clone()).fg(theme::warning()),
                None => {
                    Line::from("~, a relative path, or an absolute one").fg(theme::text_muted())
                }
            };
            Paragraph::new(status).render(status_area, buf);

            self.view_hints(buf, hints_area);

            Paragraph::new(theme::help_line(&[("Enter", "search"), ("Esc", "cancel")]))
                .fg(theme::text_muted())
                .render(help_area, buf);
        });
    }

    /// The hint rows under the input: the subdirectories the typed path
    /// completes to, the highlighted one ready for Tab.
    fn view_hints(&self, buf: &mut ratatui::buffer::Buffer, area: Rect) {
        if area.is_empty() || self.hints.is_empty() {
            return;
        }
        let visible: Vec<ListItem> = self
            .hints
            .iter()
            .take(area.height as usize)
            .enumerate()
            .map(|(idx, hint)| {
                render_list_item_line(
                    Line::from(hint.clone()).fg(theme::text_dim()),
                    idx == self.selected,
                )
            })
            .collect();
        Widget::render(List::new(visible), area, buf);
    }
}

impl Default for DirPrompt {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop `.` segments and fold `..` into its parent, lexically (no symlink
/// resolution, no disk access): a hand-typed `../shots` must still show a stable
/// label and travel as the tree it landed on.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let foldable = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if foldable {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Roots {
        Roots {
            workspace: PathBuf::from("/ws"),
            home: Some(PathBuf::from("/home/me")),
        }
    }

    fn paths(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn open_finder() -> AttachFinder {
        let mut finder = AttachFinder::new();
        finder.open(PathBuf::from("/ws"), roots());
        finder
    }

    fn reply(finder: &mut AttachFinder, token: u64, items: &[&str]) {
        finder.update(AttachMessage::Candidates {
            token,
            items: paths(items),
            error: None,
        });
    }

    #[test]
    fn the_argument_resolves_against_the_workspace_and_home() {
        let roots = roots();
        assert_eq!(roots.root_for(None), PathBuf::from("/ws"));
        assert_eq!(roots.root_for(Some("  ")), PathBuf::from("/ws"));
        assert_eq!(roots.root_for(Some("docs")), PathBuf::from("/ws/docs"));
        assert_eq!(roots.root_for(Some("/tmp/x")), PathBuf::from("/tmp/x"));
        assert_eq!(
            roots.root_for(Some("~/shots")),
            PathBuf::from("/home/me/shots")
        );
        assert_eq!(roots.root_for(Some("~")), PathBuf::from("/home/me"));
        // No workspace known (an unstarted tab): a relative argument stays as
        // given rather than being glued onto an empty path.
        let bare = Roots::default();
        assert_eq!(bare.root_for(Some("docs")), PathBuf::from("docs"));
    }

    #[test]
    fn labels_are_workspace_relative_then_home_shortened() {
        let roots = roots();
        assert_eq!(roots.label(Path::new("/ws")), ".");
        assert_eq!(roots.label(Path::new("/ws/docs/deep")), "docs/deep");
        assert_eq!(roots.label(Path::new("/home/me/shots")), "~/shots");
        assert_eq!(roots.label(Path::new("/home/me")), "~");
        assert_eq!(roots.label(Path::new("/elsewhere/x")), "/elsewhere/x");
    }

    #[test]
    fn opening_requests_the_unfiltered_listing_once() {
        let mut finder = AttachFinder::new();
        finder.open(PathBuf::from("/ws/docs"), roots());
        assert!(finder.open);
        assert_eq!(
            finder.request(),
            Some((0, Some("/ws/docs".into()), String::new()))
        );
        // The request is in flight: a re-sync must not ask again.
        assert_eq!(finder.request(), None);
        reply(&mut finder, 0, &["docs/plan.pdf"]);
        // Answered: still no new request for the same query.
        assert_eq!(finder.request(), None);
    }

    #[test]
    fn typing_requests_again_and_drops_stale_replies() {
        let mut finder = open_finder();
        assert_eq!(finder.request().map(|(t, ..)| t), Some(0));
        finder.update(AttachMessage::Input('p'));
        finder.update(AttachMessage::Input('l'));
        assert_eq!(
            finder.request(),
            Some((1, Some("/ws".into()), "pl".into())),
            "a new query supersedes the in-flight one"
        );
        // The older reply (token 0) never lands.
        reply(&mut finder, 0, &["stale.pdf"]);
        assert!(!finder.candidates.iter().any(|path| path == "stale.pdf"));
        reply(&mut finder, 1, &["assets/plan.pdf"]);
        assert_eq!(
            finder.update(AttachMessage::Accept),
            Some(AttachEffect::Accept("assets/plan.pdf".into()))
        );
    }

    #[test]
    fn a_new_reply_keeps_the_selected_path_when_it_survives() {
        let mut finder = open_finder();
        finder.request();
        reply(&mut finder, 0, &["a.pdf", "b.pdf", "c.pdf"]);
        finder.update(AttachMessage::Next);
        assert_eq!(finder.selected, 1);
        // A refined query keeps `b.pdf` at a different rank.
        finder.update(AttachMessage::Input('b'));
        finder.request();
        reply(&mut finder, 1, &["c.pdf", "b.pdf"]);
        assert_eq!(finder.candidates[finder.selected], "b.pdf");
        // A reply without the selected path resets to the best match.
        finder.update(AttachMessage::Backspace);
        finder.request();
        reply(&mut finder, 2, &["a.pdf"]);
        assert_eq!(finder.selected, 0);
    }

    #[test]
    fn an_error_is_reported_instead_of_results() {
        let mut finder = AttachFinder::new();
        finder.open(PathBuf::from("/ws/nope"), roots());
        finder.request();
        finder.update(AttachMessage::Candidates {
            token: 0,
            items: Vec::new(),
            error: Some("not a directory: /ws/nope".into()),
        });
        assert_eq!(finder.error.as_deref(), Some("not a directory: /ws/nope"));
        // Nothing to attach, so Enter is a no-op rather than an empty pick.
        assert_eq!(finder.update(AttachMessage::Accept), None);
        assert!(finder.open);
    }

    #[test]
    fn accept_hands_back_the_path_and_closes() {
        let mut finder = open_finder();
        finder.request();
        reply(&mut finder, 0, &["a.pdf", "b.png"]);
        finder.update(AttachMessage::Next);
        assert_eq!(
            finder.update(AttachMessage::Accept),
            Some(AttachEffect::Accept("b.png".into()))
        );
        assert!(!finder.open, "accepting closes the dialog");
        assert_eq!(finder.request(), None, "a closed dialog owes nothing");
    }

    #[test]
    fn escape_closes_without_attaching() {
        let mut finder = open_finder();
        finder.request();
        reply(&mut finder, 0, &["a.pdf"]);
        assert!(matches!(
            finder.update(AttachMessage::Close),
            Some(AttachEffect::Close)
        ));
        assert!(!finder.open);
    }

    #[test]
    fn closed_dialog_ignores_everything_but_close() {
        let mut finder = AttachFinder::new();
        assert_eq!(finder.request(), None);
        assert_eq!(finder.update(AttachMessage::Input('a')), None);
        assert!(finder.query.is_empty());
        assert!(matches!(
            finder.update(AttachMessage::Close),
            Some(AttachEffect::Close)
        ));
    }

    #[test]
    fn ctrl_d_asks_for_the_directory_prompt() {
        let mut finder = open_finder();
        let key =
            termina::event::KeyEvent::new(KeyCode::Char('d'), termina::event::Modifiers::CONTROL);
        let message = finder.map_event(&key).expect("a key mapping");
        assert!(matches!(message, AttachMessage::EditDir));
        assert!(matches!(
            finder.update(message),
            Some(AttachEffect::EditDir)
        ));
        assert!(finder.open, "the prompt sits on top of the picker");
    }

    #[test]
    fn keys_are_mapped_or_swallowed() {
        let finder = open_finder();
        let plain = |code| termina::event::KeyEvent::new(code, termina::event::Modifiers::NONE);
        assert!(matches!(
            finder.map_event(&plain(KeyCode::Char('a'))),
            Some(AttachMessage::Input('a'))
        ));
        assert!(matches!(
            finder.map_event(&plain(KeyCode::Enter)),
            Some(AttachMessage::Accept)
        ));
        assert!(matches!(
            finder.map_event(&plain(KeyCode::Escape)),
            Some(AttachMessage::Close)
        ));
        // A key the dialog does not use is swallowed rather than leaking into
        // the composer behind it.
        assert!(matches!(
            finder.map_event(&plain(KeyCode::Home)),
            Some(AttachMessage::Swallow)
        ));
        let alt = termina::event::KeyEvent::new(KeyCode::Char('x'), termina::event::Modifiers::ALT);
        assert!(matches!(
            finder.map_event(&alt),
            Some(AttachMessage::Swallow)
        ));
    }

    #[test]
    fn resize_shifts_the_scroll_window() {
        let mut finder = open_finder();
        finder.request();
        reply(
            &mut finder,
            0,
            &(0..10)
                .map(|i| Box::leak(format!("f{i}.txt").into_boxed_str()) as &str)
                .collect::<Vec<_>>(),
        );
        finder.update(AttachMessage::Resize { viewport_height: 3 });
        for _ in 0..9 {
            finder.update(AttachMessage::Next);
        }
        assert_eq!(finder.selected, 9);
        assert_eq!(finder.offset, 7, "the last row stays inside a 3-row view");
    }

    #[test]
    fn setting_the_root_reranks_the_new_tree() {
        let mut finder = open_finder();
        finder.request();
        reply(&mut finder, 0, &["plan.md"]);
        finder.update(AttachMessage::Input('p'));
        finder.set_root(PathBuf::from("/ws/shots"));
        assert_eq!(finder.root_label(), "shots");
        assert!(finder.query.is_empty(), "the old tree's query is dropped");
        assert!(finder.candidates.is_empty());
        assert_eq!(
            finder.request(),
            Some((1, Some("/ws/shots".into()), String::new())),
            "the new tree is ranked immediately"
        );
    }

    #[test]
    fn a_hand_typed_path_normalizes_before_it_is_used() {
        let mut finder = AttachFinder::new();
        finder.open(PathBuf::from("/ws/../shots/./today"), roots());
        assert_eq!(finder.root_label(), "/shots/today");
        assert_eq!(
            finder.request(),
            Some((0, Some("/shots/today".into()), String::new()))
        );
        // A leading `..` has nothing to fold into, so it survives.
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
    }

    #[test]
    fn the_directory_prompt_prefills_and_resolves_its_answer() {
        let mut prompt = DirPrompt::new();
        prompt.open("docs", Path::new("/ws"));
        assert!(prompt.open);
        assert_eq!(prompt.buffer.value, "docs");
        // Typing clears a previous refusal.
        prompt.reject("not a directory: /ws/nope".into());
        assert!(prompt.error.is_some());
        prompt.update(DirPromptMessage::Input('x'));
        assert!(prompt.error.is_none());
        assert_eq!(
            prompt.update(DirPromptMessage::Submit),
            Some(DirPromptEffect::Submit {
                dir: "docsx".into(),
                hint: None,
            })
        );
        // The prompt stays open until the app confirms the directory, so a
        // refusal can be shown in place.
        assert!(prompt.open);
        prompt.update(DirPromptMessage::Cancel);
        assert!(!prompt.open);
        assert!(prompt.buffer.value.is_empty());
    }

    #[test]
    fn an_empty_directory_submit_is_a_cancel() {
        let mut prompt = DirPrompt::new();
        prompt.open("", Path::new("/ws"));
        assert_eq!(
            prompt.update(DirPromptMessage::Submit),
            Some(DirPromptEffect::Close)
        );
        assert!(!prompt.open);
    }

    #[test]
    fn the_directory_prompt_routes_editing_keys() {
        use termina::event::Modifiers;

        let prompt = DirPrompt::new();
        let key = |code, modifiers| termina::event::KeyEvent::new(code, modifiers);
        assert_eq!(
            prompt.map_event(&key(KeyCode::Char('x'), Modifiers::NONE)),
            Some(DirPromptMessage::Input('x'))
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Char('a'), Modifiers::CONTROL)),
            Some(DirPromptMessage::Home)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Char('f'), Modifiers::ALT)),
            Some(DirPromptMessage::RightWord)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Enter, Modifiers::NONE)),
            Some(DirPromptMessage::Submit)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Escape, Modifiers::NONE)),
            Some(DirPromptMessage::Cancel)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Down, Modifiers::NONE)),
            Some(DirPromptMessage::Next)
        );
        assert_eq!(
            prompt.map_event(&key(KeyCode::Tab, Modifiers::NONE)),
            Some(DirPromptMessage::AcceptHint)
        );
    }

    #[test]
    fn the_directory_prompt_asks_for_hints_once_per_text() {
        let mut prompt = DirPrompt::new();
        prompt.open("doc", Path::new("/ws"));
        assert_eq!(prompt.hints_request(), Some((0, "doc".into())));
        // In flight: a re-sync must not ask again.
        assert_eq!(prompt.hints_request(), None);
        prompt.set_hints(0, paths(&["docs", "docs-old"]));
        assert_eq!(prompt.hints_request(), None, "the text is answered");
        // Typing asks for the new text; a stale reply never lands.
        prompt.update(DirPromptMessage::Input('s'));
        assert_eq!(prompt.hints_request(), Some((1, "docs".into())));
        prompt.set_hints(0, paths(&["stale"]));
        assert_eq!(prompt.hints[0], "docs", "the older reply dropped");
        prompt.set_hints(1, Vec::new());
        assert!(prompt.hints.is_empty());
    }

    #[test]
    fn tab_completes_the_highlighted_hint() {
        let mut prompt = DirPrompt::new();
        prompt.open("doc", Path::new("/ws"));
        prompt.hints_request();
        prompt.set_hints(0, paths(&["docs", "docs-old"]));
        prompt.update(DirPromptMessage::Next);
        prompt.update(DirPromptMessage::AcceptHint);
        assert_eq!(prompt.buffer.value, "docs-old");
        // The completed text is a fresh query: the hints re-ask for it.
        assert_eq!(prompt.hints_request(), Some((1, "docs-old".into())));
        // A refusal from the last submit never survives a completion.
        prompt.reject("not a directory: /ws/nope".into());
        prompt.set_hints(1, paths(&["docs-old/deep"]));
        prompt.update(DirPromptMessage::AcceptHint);
        assert_eq!(prompt.buffer.value, "docs-old/deep");
        assert!(prompt.error.is_none());
    }

    #[test]
    fn entering_hands_the_hint_over_as_a_fallback() {
        let mut prompt = DirPrompt::new();
        prompt.open("doc", Path::new("/ws"));
        prompt.hints_request();
        prompt.set_hints(0, paths(&["docs"]));
        assert_eq!(
            prompt.update(DirPromptMessage::Submit),
            Some(DirPromptEffect::Submit {
                dir: "doc".into(),
                hint: Some("docs".into()),
            }),
            "the app decides whether the typed text or its completion wins"
        );
        // No hints: a bare submit with no fallback.
        let mut prompt = DirPrompt::new();
        prompt.open("docs", Path::new("/ws"));
        prompt.update(DirPromptMessage::Submit);
        assert_eq!(
            prompt.update(DirPromptMessage::Submit),
            Some(DirPromptEffect::Submit {
                dir: "docs".into(),
                hint: None,
            })
        );
    }

    fn draw(finder: &AttachFinder, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| finder.view(frame, frame.area(), false))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn draw_prompt(prompt: &DirPrompt, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| prompt.view(frame, frame.area(), false))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn find_row(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buf.area().height {
            let row: String = (0..buf.area().width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if let Some(at) = row.find(needle) {
                return Some((at as u16, y));
            }
        }
        None
    }

    #[test]
    fn the_dialog_paints_centered_with_its_directory_and_ranked_paths() {
        let mut finder = AttachFinder::new();
        finder.open(PathBuf::from("/ws/docs"), roots());
        finder.request();
        reply(&mut finder, 0, &["docs/plan.pdf", "docs/shot.png"]);
        let buf = draw(&finder, 100, 40);
        let (title_x, title_y) = find_row(&buf, "Attach · docs").expect("the title names the dir");
        // Centred: the dialog spans 64% of the width, so its title starts well
        // inside the left edge rather than hugging it.
        assert!(title_x > 10, "title x = {title_x}");
        assert!(title_y > 2, "title y = {title_y}");
        assert!(
            find_row(&buf, "Type to search files").is_some(),
            "the empty query shows its placeholder"
        );
        assert!(find_row(&buf, "docs/plan.pdf").is_some());
        assert!(find_row(&buf, "docs/shot.png").is_some());
        assert!(
            find_row(&buf, "Ctrl+D").is_some(),
            "the hint bar names the directory key"
        );
        // The typed query replaces the placeholder.
        finder.update(AttachMessage::Input('p'));
        let buf = draw(&finder, 100, 40);
        assert!(find_row(&buf, "Type to search files").is_none());
        assert!(find_row(&buf, "› p").is_some());
    }

    #[test]
    fn the_directory_prompt_paints_its_prefilled_root_and_refusal() {
        let mut prompt = DirPrompt::new();
        prompt.open("docs", Path::new("/ws"));
        let buf = draw_prompt(&prompt, 80, 24);
        assert!(find_row(&buf, "Attach Directory").is_some());
        assert!(find_row(&buf, "docs").is_some());
        prompt.reject("not a directory: /ws/nope".into());
        let buf = draw_prompt(&prompt, 80, 24);
        assert!(find_row(&buf, "not a directory").is_some());
    }

    #[test]
    fn the_directory_prompt_lists_its_hints() {
        let mut prompt = DirPrompt::new();
        prompt.open("doc", Path::new("/ws"));
        prompt.hints_request();
        prompt.set_hints(0, paths(&["docs", "docs-old", "documents"]));
        let buf = draw_prompt(&prompt, 80, 24);
        assert!(find_row(&buf, "docs-old").is_some());
        assert!(find_row(&buf, "documents").is_some());
        // The highlight follows the selection, and Tab completes it into the
        // input line.
        prompt.update(DirPromptMessage::Next);
        prompt.update(DirPromptMessage::AcceptHint);
        assert_eq!(prompt.buffer.value, "docs-old");
        let buf = draw_prompt(&prompt, 80, 24);
        // The input line now holds the completed path, and the hint rows are
        // still listed under it.
        assert!(find_row(&buf, "docs-old").is_some());
    }

    #[test]
    fn a_closed_dialog_paints_nothing() {
        let finder = AttachFinder::new();
        let buf = draw(&finder, 40, 10);
        assert!(
            (0..buf.area().height)
                .all(|y| { (0..buf.area().width).all(|x| buf[(x, y)].symbol() == " ") }),
            "a closed dialog leaves the buffer untouched"
        );
    }
}
