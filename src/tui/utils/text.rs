use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate styled spans to `max_width` display columns. When content is cut,
/// a `…` in the last surviving style terminates the line inside the budget.
pub fn truncate_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    if max_width == 0 {
        return Vec::new();
    }
    let total: usize = spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    if total <= max_width {
        return spans;
    }
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    let mut remaining = max_width - 1;
    for span in &spans {
        if remaining == 0 {
            break;
        }
        let width: usize = UnicodeWidthStr::width(span.content.as_ref());
        if width <= remaining {
            remaining -= width;
            out.push(span.clone());
            continue;
        }
        let mut text = String::new();
        for ch in span.content.chars() {
            let cw = ch.width().unwrap_or(0);
            if cw > remaining {
                break;
            }
            text.push(ch);
            remaining -= cw;
        }
        out.push(Span::styled(text, span.style));
        break;
    }
    match out.last_mut() {
        Some(last) => last.content.to_mut().push('…'),
        None => out.push(Span::raw("…".to_string())),
    }
    out
}

/// Word-wrap `text` into rows of at most `width` display columns. Breaks ride
/// the whitespace runs between words (a break consumes its separator, so a
/// row never ends in whitespace) and a token wider than the whole row is
/// hard-split at character widths — wide chars stay whole and zero-width
/// chars ride with the piece before them. Newlines and carriage returns are
/// hard breaks; an empty segment yields one empty row.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for line in text.split('\n') {
        wrap_segment(line.strip_suffix('\r').unwrap_or(line), width, &mut rows);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn wrap_segment(text: &str, width: usize, rows: &mut Vec<String>) {
    let mut row = String::new();
    let mut row_w = 0usize;
    for (token, ws) in split_runs(text) {
        let token_w = token.width();
        if ws {
            if row_w + token_w <= width {
                row_w += token_w;
                row.push_str(&token);
            } else {
                close_row(rows, &mut row);
                row_w = 0;
            }
            continue;
        }
        if row_w + token_w <= width {
            row_w += token_w;
            row.push_str(&token);
            continue;
        }
        close_row(rows, &mut row);
        if token_w <= width {
            row_w = token_w;
            row.push_str(&token);
            continue;
        }
        let mut pieces = split_width(&token, width);
        if let Some(last) = pieces.pop() {
            rows.append(&mut pieces);
            row_w = last.width();
            row = last;
        }
    }
    rows.push(row);
}

/// Flush the current row: a break consumes its trailing whitespace and a
/// non-empty row is pushed.
fn close_row(rows: &mut Vec<String>, row: &mut String) {
    while row.ends_with([' ', '\t']) {
        row.pop();
    }
    if !row.is_empty() {
        rows.push(std::mem::take(row));
    }
}

/// Split into `(token, whitespace)` runs: space/tab runs are the break
/// opportunities, everything else is a word.
fn split_runs(text: &str) -> Vec<(String, bool)> {
    let mut runs = Vec::new();
    let mut chunk = String::new();
    let mut ws = false;
    for ch in text.chars() {
        let space = matches!(ch, ' ' | '\t');
        if space != ws && !chunk.is_empty() {
            runs.push((std::mem::take(&mut chunk), ws));
        }
        ws = space;
        chunk.push(ch);
    }
    if !chunk.is_empty() {
        runs.push((chunk, ws));
    }
    runs
}

/// Split one over-long token at character widths: a zero-width char (combining
/// mark) rides with the piece before it, and a char wider than the whole
/// column forms its own piece so the split always makes progress.
fn split_width(text: &str, width: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut piece_w = 0usize;
    for ch in text.chars() {
        let cw = ch.width().unwrap_or(0);
        if piece_w + cw > width && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            piece_w = 0;
        }
        piece.push(ch);
        piece_w += cw;
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.to_string()).collect()
    }

    #[test]
    fn fits_without_change() {
        let spans = vec![Span::raw("ab".to_string()), Span::raw("cd".to_string())];
        assert_eq!(text(&truncate_spans(spans, 10)), "abcd");
    }

    #[test]
    fn truncates_with_ellipsis_on_last_span() {
        use ratatui::style::Stylize;
        let spans = vec![
            Span::raw("abc".to_string()).fg(ratatui::style::Color::Red),
            Span::raw("def".to_string()),
        ];
        let out = truncate_spans(spans, 4);
        assert_eq!(text(&out), "abc…");
        assert_eq!(out[0].style.fg, Some(ratatui::style::Color::Red));
    }

    #[test]
    fn drops_later_spans_entirely() {
        let spans = vec![Span::raw("abcd".to_string()), Span::raw("ef".to_string())];
        assert_eq!(text(&truncate_spans(spans, 4)), "abc…");
    }

    #[test]
    fn zero_width_is_empty() {
        let spans = vec![Span::raw("abcd".to_string())];
        assert!(truncate_spans(spans, 0).is_empty());
    }

    #[test]
    fn single_wide_budget_still_shows_ellipsis() {
        let spans = vec![Span::raw("abcd".to_string())];
        assert_eq!(text(&truncate_spans(spans, 1)), "…");
    }

    #[test]
    fn counts_wide_chars_by_display_columns() {
        let spans = vec![Span::raw("你好".to_string())];
        assert_eq!(text(&truncate_spans(spans, 3)), "你…");
    }

    #[test]
    fn wraps_at_word_boundaries() {
        assert_eq!(wrap_text("one two three", 8), vec!["one two", "three"]);
    }

    #[test]
    fn empty_text_is_one_empty_row() {
        assert_eq!(wrap_text("", 5), vec![String::new()]);
    }

    #[test]
    fn hard_splits_over_long_words() {
        let long = "x".repeat(20);
        let rows = wrap_text(&long, 8);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.chars().count() <= 8));
    }

    #[test]
    fn breaks_consume_their_separator() {
        assert_eq!(wrap_text("ab   cd", 4), vec!["ab", "cd"]);
        assert_eq!(wrap_text("ab\tcd", 3), vec!["ab", "cd"]);
    }

    #[test]
    fn flush_keeps_row_w_tracking_honest() {
        assert_eq!(wrap_text("abcdefgh x y", 10), vec!["abcdefgh x", "y"]);
    }

    #[test]
    fn counts_wide_rows_by_display_columns() {
        assert_eq!(wrap_text("日本語 テスト", 8), vec!["日本語", "テスト"]);
    }

    #[test]
    fn zero_width_char_rides_with_its_base() {
        let rows = wrap_text("e\u{301}x", 1);
        assert_eq!(rows, vec!["e\u{301}".to_string(), "x".to_string()]);
    }

    #[test]
    fn newline_is_a_hard_break() {
        assert_eq!(wrap_text("one\ntwo", 10), vec!["one", "two"]);
        assert_eq!(wrap_text("a\r\nb", 10), vec!["a", "b"]);
    }
}
