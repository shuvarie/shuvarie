use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::prelude::*;
use ratatui::style::Modifier;
use ratatui::widgets::Paragraph;
use termina::event::{KeyCode, KeyEvent};

use crate::tui::utils::ctrl;

use super::theme;

#[derive(Debug, PartialEq)]
pub enum SearchMessage {
    Input(char),
    Backspace,
    #[allow(dead_code)]
    Clear,
    #[allow(dead_code)]
    Activate,
    #[allow(dead_code)]
    Deactivate,
}

pub fn filter_indices<F>(query: &str, count: usize, key: F) -> Vec<usize>
where
    F: Fn(usize) -> String,
{
    if query.is_empty() {
        return (0..count).collect();
    }
    // `match_list` wants the haystacks up front, so every key is materialized
    // once per keystroke -- the same number of `key` calls the per-item scorers
    // make. Its default config is what is wanted here: no typo tolerance
    // (`max_typos: Some(0)`, so a query only filters, never fuzzes into a
    // neighbour), smart casing, and `ScoreThenIndexAsc`, which is the ordering
    // already relied on -- ranking ties stay in list order instead of shuffling
    // as the query is typed.
    let haystacks: Vec<String> = (0..count).map(key).collect();
    let mut matcher = neo_frizbee::Matcher::new(query, &neo_frizbee::Config::default());
    matcher
        .match_list(&haystacks)
        .into_iter()
        .map(|m| m.index as usize)
        .collect()
}

pub struct Search {
    pub query: String,
    pub active: bool,
}

impl Search {
    pub fn new() -> Self {
        Self {
            query: String::new(),
            active: false,
        }
    }

    #[allow(dead_code)]
    pub fn map_event(&self, key: &KeyEvent) -> Option<SearchMessage> {
        match key.code {
            KeyCode::Backspace => Some(SearchMessage::Backspace),
            KeyCode::Char(c) if !ctrl(key) => Some(SearchMessage::Input(c)),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: SearchMessage) {
        match msg {
            SearchMessage::Input(c) => {
                self.query.push(c);
            }
            SearchMessage::Backspace => {
                self.query.pop();
            }
            SearchMessage::Clear => {
                self.query.clear();
            }
            SearchMessage::Activate => {
                self.active = true;
            }
            SearchMessage::Deactivate => {
                self.active = false;
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    pub fn clear(&mut self) {
        self.query.clear();
        self.active = false;
    }

    pub fn filter_indices<F>(&self, count: usize, key: F) -> Vec<usize>
    where
        F: Fn(usize) -> String,
    {
        filter_indices(&self.query, count, key)
    }

    /// Paints the query line into `buf` at `area` — buffer-based because the
    /// popup dialogs render their bodies through a buffer closure.
    pub fn view(&self, buf: &mut Buffer, area: Rect, placeholder: &str) {
        let widget = if self.query.is_empty() {
            Paragraph::new(placeholder).fg(theme::text_muted())
        } else {
            let mut spans = vec![Span::raw("/ ").fg(theme::text_muted())];
            let chars: Vec<char> = self.query.chars().collect();
            for c in chars {
                spans.push(Span::styled(
                    c.to_string(),
                    Style::new()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            Paragraph::new(Line::from(spans))
        };
        widget.render(area, buf);
    }
}

impl Default for Search {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_all() {
        let res = matches("", &["gpt-4", "claude-3"]);
        assert_eq!(res, vec![0, 1]);
    }

    #[test]
    fn filters_and_sorts_by_score() {
        let res = matches("gpt", &["claude-3-opus", "gpt-4", "gpt-3.5"]);
        assert!(res.contains(&1));
        assert!(res.contains(&2));
        assert!(!res.contains(&0));
        let gpt4 = res.iter().position(|&i| i == 1).unwrap();
        let gpt35 = res.iter().position(|&i| i == 2).unwrap();
        assert!(gpt4 < gpt35, "equal prefixes keep list order");
    }

    #[test]
    fn a_prefix_hit_outranks_a_mid_string_hit() {
        // The property the swap was made for: a query that starts a title is
        // ranked above the same word appearing later in one, which nucleo's
        // scoring did not consistently do for whole-phrase needles.
        let res = matches("search", &["add a search box", "search: tune ranking"]);
        assert_eq!(res, vec![1, 0]);
    }

    fn matches(query: &str, items: &[&str]) -> Vec<usize> {
        let mut search = Search::new();
        search.query = query.into();
        search.filter_indices(items.len(), |i| items[i].to_string())
    }

    #[test]
    fn a_typo_filters_the_row_out() {
        // `max_typos: Some(0)` -- the query narrows the list, it never fuzzes
        // into a neighbour, so a session list cannot offer an unrelated row.
        assert!(matches("gpty", &["gpt-4", "gpt-3.5"]).is_empty());
    }

    #[test]
    fn a_one_char_query_matches() {
        // `fff-search`'s own query layer drops needles under 2 bytes; matching
        // through its matcher directly does not, so the first keystroke narrows.
        let res = matches("s", &["gpt-4", "session cleanup"]);
        assert_eq!(res, vec![1]);
    }

    #[test]
    fn a_lowercase_query_matches_any_casing() {
        let res = matches("search", &["SEARCH the docs", "unrelated"]);
        assert_eq!(res, vec![0]);
    }

    #[test]
    fn an_empty_list_filters_to_nothing() {
        assert!(matches("x", &[]).is_empty());
        assert!(matches("", &[]).is_empty());
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut search = Search::new();
        search.query = "abc".into();
        search.update(SearchMessage::Backspace);
        assert_eq!(search.query, "ab");
    }

    #[test]
    fn input_appends_char() {
        let mut search = Search::new();
        search.update(SearchMessage::Input('a'));
        search.update(SearchMessage::Input('b'));
        assert_eq!(search.query, "ab");
    }

    #[test]
    fn clear_resets_query() {
        let mut search = Search::new();
        search.query = "abc".into();
        search.update(SearchMessage::Clear);
        assert_eq!(search.query, "");
    }
}
