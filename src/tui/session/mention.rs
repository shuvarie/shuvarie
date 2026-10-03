//! The composer's `@` mention completion popup: a trailing `@path`-looking
//! token opens a candidate list above the input, fed by core's
//! `RequestPathCompletions` (one `read_dir` per directory part, cached here
//! so prefix edits stay local). Mirrors [`super::slash`] for the key
//! contract: Up/Down/Enter/Tab are claimed while active, Escape dismisses.

use std::collections::HashMap;
use std::rc::Rc;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Clear, ListItem, Padding};
use shuvarie_core::attachments::PathCandidate;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::list::{render_list_item_line, scroll_offset_for};
use crate::tui::theme;
use crate::tui::utils::ctrl;

/// Maximum rows visible in the tooltip before it scrolls.
pub const MAX_VISIBLE: usize = 6;

/// How many directories stay cached; beyond that the oldest entries drop
/// (an `@` mention never revisits more than a handful of directories).
const MAX_CACHED_DIRS: usize = 32;

pub enum MentionMessage {
    Next,
    Prev,
    /// Accept the highlighted candidate (Tab and Enter both).
    Accept,
    Dismiss,
}

/// The byte range the active trailing mention spans in the buffer's `value`:
/// from the `@` to the end of the buffer (a completing mention is always the
/// trailing token).
pub struct TokenRange {
    pub start: usize,
    pub end: usize,
}

pub struct MentionMenu {
    open: bool,
    /// The full buffer tail after `@` (dir part + prefix), for the popup.
    query: String,
    /// The directory key the current candidates were listed for (empty =
    /// the workspace root).
    dir: String,
    /// The `@`'s byte offset in the buffer while the popup is open.
    token: TokenRange,
    requests: u64,
    /// Listing per directory part; `Rc` so a request reply can swap in.
    cache: HashMap<String, Rc<[PathCandidate]>>,
    /// The request's key when a listing is in flight (`token` is the reply
    /// key): stale replies are dropped by `set_candidates`.
    inflight: Option<(u64, String)>,
    filtered: Vec<usize>,
    selected: usize,
    offset: usize,
    /// The query tail Escape dismissed; re-typing the same tail stays closed
    /// until it changes.
    dismissed: Option<String>,
}

impl MentionMenu {
    pub fn new() -> Self {
        Self {
            open: false,
            query: String::new(),
            dir: String::new(),
            token: TokenRange { start: 0, end: 0 },
            requests: 0,
            cache: HashMap::new(),
            inflight: None,
            filtered: Vec::new(),
            selected: 0,
            offset: 0,
            dismissed: None,
        }
    }

    pub fn active(&self) -> bool {
        self.open && !self.filtered.is_empty()
    }

    /// Tooltip rect floating above the input area, clamped to the history
    /// pane (mirrors the slash menu's geometry).
    pub fn popup_rect(&self, history: Rect, input: Rect) -> Rect {
        let visible = (self.filtered.len() as u16).min(MAX_VISIBLE as u16).max(1);
        let width = input.width.saturating_sub(4).clamp(20, 60);
        let height = (visible + 1).min(history.height.max(1));
        let y = input.y.saturating_sub(height).max(history.y);
        let height = input.y.saturating_sub(y);
        Rect::new(input.x + 2, y, width, height)
    }

    /// Re-derive popup state from the buffer. Returns `(token, query)` when
    /// the active mention's directory listing must be requested (a dir with
    /// no cached listing): the caller sends `RequestPathCompletions`; the
    /// reply lands in [`MentionMenu::set_candidates`].
    pub fn sync(&mut self, buffer: &str) -> Option<(u64, String)> {
        let mention = trailing_mention(buffer);
        let (range, tail) = match mention {
            Some((start, tail)) => (
                TokenRange {
                    start,
                    end: buffer.len(),
                },
                tail.to_string(),
            ),
            None => {
                self.open = false;
                self.query.clear();
                self.filtered.clear();
                self.dismissed = None;
                return None;
            }
        };
        // Sticky dismissal: while the user extends the dismissed tail
        // (typing refines the query), the popup stays closed; a different
        // query tail — a new @, a different base — opens it again.
        if let Some(dismissed) = self.dismissed.clone()
            && tail.starts_with(dismissed.as_str())
            && dir_part(&tail) == dir_part(&dismissed)
        {
            self.open = false;
            return None;
        }
        self.dismissed = None;
        self.open = true;
        let dir = dir_part(&tail);
        self.token = range;
        self.dir = dir.clone();
        self.query = tail.clone();
        // A cached listing (the same dir or one revisited) filters locally;
        // otherwise an inflight request for this dir waits for its reply.
        if let Some(cached) = self.cache.get(dir.as_str()).cloned() {
            self.refilter(&cached);
            return None;
        }
        if self.inflight.as_ref().is_some_and(|(_, key)| *key == dir) {
            return None;
        }
        let token = self.requests;
        self.requests += 1;
        self.inflight = Some((token, dir));
        Some((token, tail))
    }

    /// The reply to a listing request. Stale tokens drop without consuming
    /// the inflight slot (the matching reply or a dir change releases it).
    pub fn set_candidates(&mut self, token: u64, candidates: Vec<PathCandidate>) {
        let (request, dir) = match self
            .inflight
            .as_ref()
            .map(|(request, dir)| (*request, dir.clone()))
        {
            Some((request, dir)) => (request, dir),
            None => return,
        };
        if request != token || dir != self.dir {
            return;
        }
        self.inflight = None;
        let candidates: Rc<[PathCandidate]> = candidates.into();
        if self.cache.len() >= MAX_CACHED_DIRS {
            self.cache.clear();
        }
        self.cache.insert(dir.clone(), Rc::clone(&candidates));
        self.refilter(&candidates);
    }

    /// Rebuild `filtered` from the query's name prefix against the dir's
    /// listing. The selection resets only when the candidate set changes
    /// (`Session::update` re-syncs before every message — see slash).
    fn refilter(&mut self, candidates: &[PathCandidate]) {
        let prefix = name_prefix(&self.query);
        let new_filtered: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate
                    .name
                    .to_ascii_lowercase()
                    .starts_with(&prefix.to_ascii_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        if new_filtered != self.filtered {
            self.selected = 0;
            self.offset = 0;
        }
        self.filtered = new_filtered;
        self.recompute_offset(candidates.len());
    }

    fn viewport(&self) -> usize {
        self.filtered.len().min(MAX_VISIBLE)
    }

    pub fn next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1).min(self.filtered.len() - 1);
            self.recompute_offset_for_offset();
        }
    }

    pub fn prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self.selected.saturating_sub(1);
            self.recompute_offset_for_offset();
        }
    }

    fn recompute_offset_for_offset(&mut self) {
        let vh = self.viewport();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, self.filtered.len());
    }

    fn recompute_offset(&mut self, len: usize) {
        let vh = self.viewport();
        self.offset = scroll_offset_for(self.selected, self.offset, vh, len);
    }

    /// The text to splice for the selected candidate: the typed directory
    /// part (which may be a subpath, absolute, or `~`-prefixed) plus the
    /// entry name. `None` with no selection.
    pub fn accept(&self) -> Option<String> {
        let idx = self.filtered.get(self.selected)?;
        let name = self
            .cached_candidates()?
            .get(*idx)
            .map(|candidate| candidate.name.clone())?;
        Some(if self.dir.is_empty() {
            name
        } else {
            format!("{}/{name}", self.dir)
        })
    }

    fn cached_candidates(&self) -> Option<&Rc<[PathCandidate]>> {
        self.cache.get(self.dir.as_str())
    }

    pub fn dismiss(&mut self) {
        if self.open {
            self.dismissed = Some(self.query.clone());
        }
    }

    /// The active mention's byte range in the buffer (`start`, `end`) — the
    /// `@` inclusive, buffer end exclusive. Valid while the popup is open.
    pub fn token_range(&self) -> (usize, usize) {
        (self.token.start, self.token.end)
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<MentionMessage> {
        if ctrl(key) {
            return match key.code {
                KeyCode::Char('n') => Some(MentionMessage::Next),
                KeyCode::Char('p') => Some(MentionMessage::Prev),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Tab => Some(MentionMessage::Accept),
            KeyCode::Up => Some(MentionMessage::Prev),
            KeyCode::Down => Some(MentionMessage::Next),
            KeyCode::Enter => Some(MentionMessage::Accept),
            KeyCode::Escape => Some(MentionMessage::Dismiss),
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
        let Some(candidates) = self.cached_candidates() else {
            return;
        };
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
                let candidate = &candidates[i];
                // Display the composed token so the accept outcome is what
                // the user sees (the dir part + entry leaf).
                let candidate_name = &candidate.name;
                let name = if self.dir.is_empty() {
                    format!("@{candidate_name}")
                } else {
                    format!("@{}/{candidate_name}", self.dir)
                };
                let line = if candidate.is_dir {
                    Line::from(name).fg(theme::text_dim())
                } else {
                    Line::from(name).fg(theme::text())
                };
                render_list_item_line(line, idx == self.selected)
            })
            .collect();
        frame.render_widget(ratatui::widgets::List::new(visible), inner);
    }
}

/// The trailing `@`-mention of a buffer: the last `@` that opens a token at
/// a mention boundary (line start or after whitespace) whose tail to the
/// buffer end is whitespace-free — i.e. the token being typed right now.
/// URL-ish tokens (`://`) are prose, not paths.
fn trailing_mention(buffer: &str) -> Option<(usize, &str)> {
    for (byte, ch) in buffer.char_indices().rev() {
        if ch != '@' {
            continue;
        }
        let boundary = byte == 0
            || buffer[..byte]
                .chars()
                .next_back()
                .is_some_and(char::is_whitespace);
        if !boundary {
            continue;
        }
        let tail = &buffer[byte + 1..];
        if tail.chars().any(char::is_whitespace) || tail.contains("://") {
            return None;
        }
        return Some((byte, tail));
    }
    None
}

/// The directory part of a mention tail (without the trailing slash; `""`
/// = the workspace root; absolute and `~`-prefixed directories keep their
/// prefix). Accept-composition re-attaches the slash.
fn dir_part(tail: &str) -> String {
    match tail.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

/// The name prefix after the tail's last `/`.
fn name_prefix(tail: &str) -> String {
    match tail.rsplit_once('/') {
        Some((_, prefix)) => prefix.to_string(),
        None => tail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str) -> PathCandidate {
        PathCandidate {
            name: name.to_string(),
            is_dir: name.ends_with('/'),
        }
    }

    #[test]
    fn trailing_mention_tracks_the_active_token() {
        assert_eq!(trailing_mention("look at @src/col"), Some((8, "src/col")));
        assert_eq!(trailing_mention("@"), Some((0, "")));
        assert_eq!(trailing_mention("line1\n@x"), Some((6, "x")));
        assert_eq!(trailing_mention("ping @bob and rest"), None, "closed tail");
        assert_eq!(trailing_mention("plain text"), None);
        assert_eq!(trailing_mention("@http://x"), None, "URL-shaped");
        assert_eq!(trailing_mention("email me a@b"), None, "mid-token @");
    }

    #[test]
    fn sync_opens_and_requests_uncached_directories() {
        let mut menu = MentionMenu::new();
        // "@" opens with an empty-prefix root request.
        assert_eq!(menu.sync("@"), Some((0, "".to_string())));
        assert!(!menu.active(), "no candidates yet");
        menu.set_candidates(0, vec![candidate("main.rs"), candidate("src/")]);
        assert!(menu.active(), "candidates arrive → the popup opens");
        // Extending the prefix inside the same dir stays local.
        assert_eq!(menu.sync("@m"), None);
        assert!(menu.active());
        // Entering a directory requests its listing.
        let (token, query) = menu.sync("@src/").expect("uncached dir requests");
        assert_eq!(query, "src/");
        assert_eq!(token, 1);
        // Same dir again (cached) → no request.
        assert_eq!(menu.sync("@src/f"), None);
    }

    #[test]
    fn stale_reply_tokens_are_dropped() {
        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@"), Some((0, "".to_string())));
        // A newer request superseded the old token before its reply landed.
        assert_eq!(menu.sync("@src/"), Some((1, "src/".to_string())));
        menu.set_candidates(0, vec![candidate("stale.rs")]);
        assert!(menu.sync("@src/").is_none(), "state kept, not re-requested");
        assert!(!menu.active(), "the stale listing never applies");
        menu.set_candidates(1, vec![candidate("inside.rs")]);
        assert!(menu.active(), "the matching token applies");
    }

    #[test]
    fn dismiss_sticks_while_the_tail_extends_and_reopens_elsewhere() {
        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@ab"), Some((0, "ab".to_string())));
        menu.set_candidates(0, vec![candidate("abc.rs")]);
        assert!(menu.active());
        menu.dismiss();
        // Extending the dismissed tail keeps the popup closed.
        assert_eq!(
            menu.sync("@abc"),
            None,
            "same base, extended: sticky dismissal"
        );
        assert!(!menu.active());
        // A different base (cached dir) refilters locally without a request.
        assert_eq!(menu.sync("@x"), None);
        assert!(!menu.active());
        // A different directory is a different query: it reopens and
        // requests a fresh listing.
        assert_eq!(menu.sync("@src/"), Some((1, "src/".to_string())));
    }

    #[test]
    fn accept_composes_the_typed_directory_part_with_the_entry() {
        // Root-level: the bare entry name is the whole token.
        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@s"), Some((0, "s".to_string())));
        menu.set_candidates(0, vec![candidate("src/"), candidate("sql.rs")]);
        assert_eq!(menu.accept().as_deref(), Some("src/"));
        menu.next();
        assert_eq!(menu.accept().as_deref(), Some("sql.rs"));

        // A subdirectory query keeps its prefix: `@src/mai` completes to
        // `@src/main.rs`, not `@main.rs`.
        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@src/mai"), Some((0, "src/mai".to_string())));
        menu.set_candidates(0, vec![candidate("main.rs"), candidate("lib.rs")]);
        assert_eq!(menu.accept().as_deref(), Some("src/main.rs"));

        // Absolute and `~`-prefixed queries compose the same way.
        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@/etc/ap"), Some((0, "/etc/ap".to_string())));
        menu.set_candidates(0, vec![candidate("apache2/")]);
        assert_eq!(menu.accept().as_deref(), Some("/etc/apache2/"));

        let mut menu = MentionMenu::new();
        assert_eq!(menu.sync("@~/docs/pl"), Some((0, "~/docs/pl".to_string())));
        menu.set_candidates(0, vec![candidate("plans.md")]);
        assert_eq!(menu.accept().as_deref(), Some("~/docs/plans.md"));
    }

    #[test]
    fn trailing_mention_accepts_tilde_tokens() {
        assert_eq!(trailing_mention("attach @~/notes"), Some((7, "~/notes")));
        assert_eq!(trailing_mention("check @~"), Some((6, "~")));
        assert_eq!(trailing_mention("@/var/lo"), Some((0, "/var/lo")));
    }
}
