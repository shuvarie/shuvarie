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
}
