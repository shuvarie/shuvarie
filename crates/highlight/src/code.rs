use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

/// Tab stops for code rendering: a tab spans [`TAB_STOP`] columns, aligned to
/// the next multiple of [`TAB_STOP`] when interior characters precede it.
const TAB_STOP: usize = 4;

/// No-break space: the chat pane paints with `Wrap { trim: true }`, which
/// strips regular leading whitespace, so code indentation renders as NBSPs
/// (single-width, word characters to the wrapper).
const NBSP: char = '\u{00a0}';

/// Expand tabs to spaces on [`TAB_STOP`] tab stops, counted in display
/// columns.
pub fn expand_tabs(text: &str) -> String {
    if !text.contains('\t') {
        return text.to_string();
    }
    expand_tabs_mapped(text).0
}

/// [`expand_tabs`], also returning where each original byte lands: one entry
/// per original byte, plus a final one for the end of the text.
///
/// A byte that starts a multi-byte character maps to where that character
/// starts. Byte ranges into `text` — the emphasized runs of a diff row — map
/// through it onto the expansion.
pub fn expand_tabs_mapped(text: &str) -> (String, Vec<u32>) {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    let mut col = 0usize;
    for ch in text.chars() {
        let start = out.len() as u32;
        map.extend(core::iter::repeat_n(start, ch.len_utf8()));
        if ch == '\t' {
            let stop = col / TAB_STOP * TAB_STOP + TAB_STOP;
            out.extend(core::iter::repeat_n(' ', stop - col));
            col = stop;
        } else {
            out.push(ch);
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    map.push(out.len() as u32);
    (out, map)
}

/// Rewrite a rendered code line's leading whitespace into NBSPs so the
/// trimming wrapper keeps the indentation. Regular spaces become one NBSP
/// each, tabs `TAB_STOP` each.
pub fn keep_indent(mut line: Line<'static>) -> Line<'static> {
    let mut cols = 0usize;
    let mut leading = 0usize;
    let mut cut: Option<(usize, usize)> = None;
    for (i, span) in line.spans.iter().enumerate() {
        let text: &str = &span.content;
        let mut lead_cols = 0usize;
        let mut lead_bytes = 0usize;
        let mut all_ws = true;
        for (bi, ch) in text.char_indices() {
            match ch {
                ' ' => {
                    lead_cols += 1;
                    lead_bytes = bi + 1;
                }
                '\t' => {
                    lead_cols += TAB_STOP;
                    lead_bytes = bi + 1;
                }
                _ => {
                    all_ws = false;
                    break;
                }
            }
        }
        if all_ws {
            cols += lead_cols;
            leading = i + 1;
        } else {
            if lead_bytes > 0 {
                cols += lead_cols;
                cut = Some((i, lead_bytes));
            }
            leading = i;
            break;
        }
    }
    if cols == 0 {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::raw(
        core::iter::repeat_n(NBSP, cols).collect::<String>(),
    ));
    let mut rest = core::mem::take(&mut line.spans).into_iter().skip(leading);
    if let Some((_, byte_len)) = cut
        && let Some(mut mixed) = rest.next()
    {
        let mut content = mixed.content.into_owned();
        content.drain(..byte_len);
        mixed.content = content.into();
        if !mixed.content.is_empty() {
            spans.push(mixed);
        }
    }
    spans.extend(rest);
    let mut line = line;
    line.spans = spans;
    line
}
