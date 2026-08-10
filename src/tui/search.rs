use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Matcher, Utf32Str};

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

    pub fn is_empty(&self) -> bool {
        self.query.is_empty()
    }

    pub fn push(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn backspace(&mut self) {
        self.query.pop();
    }

    pub fn clear(&mut self) {
        self.query.clear();
    }

    pub fn filter_indices<F>(&self, count: usize, key: F) -> Vec<usize>
    where
        F: Fn(usize) -> String,
    {
        if self.query.is_empty() {
            return (0..count).collect();
        }
        let mut matcher = Matcher::new(nucleo::Config::DEFAULT);
        let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
        let mut buf = Vec::new();
        let mut scored: Vec<(usize, u32)> = (0..count)
            .filter_map(|i| {
                let k = key(i);
                pattern
                    .score(Utf32Str::new(k.as_str(), &mut buf), &mut matcher)
                    .map(|s| (i, s))
            })
            .collect();
        scored.sort_by_key(|b| std::cmp::Reverse(b.1));
        scored.into_iter().map(|(i, _)| i).collect()
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
        assert!(res.contains(&1)); // gpt-4
        assert!(res.contains(&2)); // gpt-3.5
        assert!(!res.contains(&0)); // claude-3-opus
        let gpt4 = res.iter().position(|&i| i == 1).unwrap();
        let gpt35 = res.iter().position(|&i| i == 2).unwrap();
        assert!(gpt4 < gpt35, "exact-ish match should rank higher");
    }

    #[test]
    fn backspace_removes_last_char() {
        let mut search = Search::new();
        search.query = "abc".into();
        search.backspace();
        assert_eq!(search.query, "ab");
    }
}
