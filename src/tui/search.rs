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
    let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
    let pattern = nucleo::pattern::Pattern::parse(
        query,
        nucleo::pattern::CaseMatching::Smart,
        nucleo::pattern::Normalization::Smart,
    );
    let mut buf = Vec::new();
    let mut scored: Vec<(usize, u32)> = (0..count)
        .filter_map(|i| {
            let k = key(i);
            pattern
                .score(nucleo::Utf32Str::new(k.as_str(), &mut buf), &mut matcher)
                .map(|s| (i, s))
        })
        .collect();
    scored.sort_by_key(|b| std::cmp::Reverse(b.1));
    scored.into_iter().map(|(i, _)| i).collect()
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

    #[allow(dead_code)]
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

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect, placeholder: &str) {
        let widget = if self.query.is_empty() {
            Paragraph::new(placeholder).fg(theme::TEXT_MUTED)
        } else {
            let mut spans = vec![Span::raw("/ ").fg(theme::TEXT_MUTED)];
            let chars: Vec<char> = self.query.chars().collect();
            for c in chars {
                spans.push(Span::styled(
                    c.to_string(),
                    Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                ));
            }
            Paragraph::new(Line::from(spans))
        };
        frame.render_widget(widget, area);
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
        let search = Search::new();
        let items = ["gpt-4", "claude-3"];
        let res = search.filter_indices(items.len(), |i| items[i].to_string());
        assert_eq!(res.len(), 2);
    }

    #[test]
    fn filters_and_sorts_by_score() {
        let mut search = Search::new();
        search.query = "gpt".into();
        let items = ["claude-3-opus", "gpt-4", "gpt-3.5"];
        let res = search.filter_indices(items.len(), |i| items[i].to_string());
        assert!(res.contains(&1));
        assert!(res.contains(&2));
        assert!(!res.contains(&0));
        let gpt4 = res.iter().position(|&i| i == 1).unwrap();
        let gpt35 = res.iter().position(|&i| i == 2).unwrap();
        assert!(gpt4 < gpt35, "exact-ish match should rank higher");
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
