use std::path::Path;

use serde_json::{Value, json};
use shuvarie_llm::{FileChange, Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::lsp_manager::SharedManager;
use crate::permissions::resolve_read;

use super::{FileLocks, arg_value, compute_diff};

pub(crate) struct EditFile {
    lsp: Option<SharedManager>,
    locks: FileLocks,
}

impl EditFile {
    pub(crate) fn new(lsp: Option<SharedManager>, locks: FileLocks) -> Self {
        Self { lsp, locks }
    }
}

impl Tool for EditFile {
    const NAME: &'static str = "edit_file";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, \
         non-overlapping region of the original file; all oldTexts are matched against the file as it \
         was before the call, not after earlier edits. If two changes affect the same block or nearby \
         lines, merge them into one edit instead of emitting overlapping edits. Do not include large \
         unchanged regions just to connect distant changes."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path of the file to edit" },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement; must be unique in the original file" },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit" }
                        },
                        "required": ["oldText", "newText"]
                    },
                    "description": "One or more targeted replacements, each matched against the original file. Do not include overlapping or nested edits."
                }
            },
            "required": ["path", "edits"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let lsp = self.lsp.clone();
        let locks = self.locks.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let edits = parse_edits(&args)?;
            let abs = resolve_read(&path)?;
            let _file_lock = locks.lock(&abs).await;
            let raw = tokio::fs::read_to_string(&abs)
                .await
                .map_err(|e| format!("read {path}: {e}"))?;
            let (had_bom, content) = split_bom(&raw);
            let ending = detect_line_ending(content);
            let base = normalize_lf(content);
            let edited = apply_edits(&base, &edits, &path)?;
            let mut final_content = String::with_capacity(raw.len() + 16);
            if had_bom {
                final_content.push('\u{FEFF}');
            }
            final_content.push_str(&restore_line_endings(&edited, ending));
            let diff = compute_diff(&raw, &final_content);
            tokio::fs::write(&abs, &final_content)
                .await
                .map_err(|e| format!("write {path}: {e}"))?;
            if let Some(lsp) = &lsp {
                lsp.lock()
                    .await
                    .on_file_change(Path::new(&path), &final_content)
                    .await;
            }
            let summary = match edits.len() {
                1 => format!("edited {path}: 1 edit applied"),
                n => format!("edited {path}: {n} edits applied"),
            };
            ctx.insert_result(FileChange::Edit {
                path,
                diff,
                original: raw,
                new: final_content,
            });
            Ok(ToolOutput::text(summary))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

struct TextEdit {
    old_text: String,
    new_text: String,
}

struct MatchedEdit {
    index: usize,
    start: usize,
    len: usize,
    new_text: String,
}

fn edit_from_value(value: &Value) -> Option<TextEdit> {
    let old = value
        .get("oldText")
        .or_else(|| value.get("old"))
        .and_then(Value::as_str)?;
    let new = value
        .get("newText")
        .or_else(|| value.get("new"))
        .and_then(Value::as_str)?;
    Some(TextEdit {
        old_text: old.to_string(),
        new_text: new.to_string(),
    })
}

fn parse_edits(args: &Value) -> Result<Vec<TextEdit>, String> {
    let mut edits: Vec<TextEdit> = Vec::new();
    match args.get("edits") {
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                edits.push(edit_from_value(item).ok_or_else(|| {
                    format!("edits[{i}] must be an object with string oldText and newText")
                })?);
            }
        }
        Some(Value::String(raw)) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Array(items)) => {
                return parse_edits(&json!({ "edits": items }));
            }
            Ok(single) => {
                return edit_from_value(&single)
                    .map(|edit| vec![edit])
                    .ok_or_else(|| {
                        "'edits' string did not contain string oldText/newText".to_string()
                    });
            }
            _ => return Err("'edits' string did not parse as a JSON array".into()),
        },
        Some(single @ Value::Object(_)) => {
            edits
                .push(edit_from_value(single).ok_or_else(|| {
                    "'edits' object needs string oldText and newText".to_string()
                })?);
        }
        _ => {}
    }
    if edits.is_empty()
        && let Some(edit) = edit_from_value(args)
    {
        edits.push(edit);
    }
    if edits.is_empty() {
        return Err("edit_file requires at least one targeted replacement in 'edits'".into());
    }
    Ok(edits)
}

fn split_bom(raw: &str) -> (bool, &str) {
    match raw.strip_prefix('\u{FEFF}') {
        Some(rest) => (true, rest),
        None => (false, raw),
    }
}

fn detect_line_ending(content: &str) -> &'static str {
    match (content.find("\r\n"), content.find('\n')) {
        (Some(crlf), Some(lf)) if crlf < lf => "\r\n",
        _ => "\n",
    }
}

fn normalize_lf(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn restore_line_endings(text: &str, ending: &str) -> String {
    if ending == "\r\n" {
        text.replace('\n', "\r\n")
    } else {
        text.to_string()
    }
}

fn apply_edits(base: &str, edits: &[TextEdit], path: &str) -> Result<String, String> {
    let single = edits.len() == 1;
    for (i, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(match single {
                true => "oldText must not be empty.".into(),
                false => format!("edits[{i}].oldText must not be empty."),
            });
        }
    }
    let mut used_fuzzy = false;
    for edit in edits {
        let (_, _, fuzzy) = fuzzy_find(base, &edit.old_text);
        if fuzzy {
            used_fuzzy = true;
        }
    }
    let replacement_base = if used_fuzzy {
        normalize_for_fuzzy_match(base)
    } else {
        base.to_string()
    };
    let mut matched: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let (index, len, _) = fuzzy_find(&replacement_base, &edit.old_text);
        let occurrences = count_fuzzy_occurrences(&replacement_base, &edit.old_text);
        match (index, occurrences) {
            (None, _) => {
                return Err(match single {
                    true => format!(
                        "Could not find the text in {path}. It must match the file content exactly, including all whitespace and newlines."
                    ),
                    false => format!(
                        "edits[{i}]: could not find the text in {path}. It must match the file content exactly, including all whitespace and newlines."
                    ),
                });
            }
            (Some(start), 1) => matched.push(MatchedEdit {
                index: i,
                start,
                len,
                new_text: edit.new_text.clone(),
            }),
            (Some(_), n) => {
                return Err(match single {
                    true => format!(
                        "Found {n} occurrences of the text in {path}. The text must be unique; include more surrounding lines to disambiguate."
                    ),
                    false => format!(
                        "Found {n} occurrences of edits[{i}].oldText in {path}. Each oldText must be unique; include more surrounding lines to disambiguate."
                    ),
                });
            }
        }
    }
    matched.sort_by_key(|m| m.start);
    for pair in matched.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        if previous.start + previous.len > current.start {
            return Err(format!(
                "edits[{}] and edits[{}] overlap in {path}. Merge them into one edit or target disjoint regions.",
                previous.index, current.index
            ));
        }
    }
    let result = if used_fuzzy {
        apply_replacements_preserving_unchanged_lines(base, &replacement_base, &matched)?
    } else {
        let mut result = base.to_string();
        for m in matched.iter().rev() {
            result.replace_range(m.start..m.start + m.len, &m.new_text);
        }
        result
    };
    if result == base {
        return Err(match single {
            true => format!(
                "No changes made to {path}. The replacement produced identical content; check for special characters or a mistaken match."
            ),
            false => {
                format!("No changes made to {path}. The replacements produced identical content.")
            }
        });
    }
    Ok(result)
}

fn fuzzy_find(content: &str, old: &str) -> (Option<usize>, usize, bool) {
    if let Some(index) = content.find(old) {
        return (Some(index), old.len(), false);
    }
    let normalized_content = normalize_for_fuzzy_match(content);
    let normalized_old = normalize_for_fuzzy_match(old);
    if let Some(index) = normalized_content.find(&normalized_old) {
        return (Some(index), normalized_old.len(), true);
    }
    (None, 0, false)
}

fn count_fuzzy_occurrences(content: &str, old: &str) -> usize {
    normalize_for_fuzzy_match(content)
        .matches(&normalize_for_fuzzy_match(old))
        .count()
}

fn normalize_for_fuzzy_match(text: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = text.chars().nfkc().collect();
    let mut out = String::with_capacity(nfkc.len());
    for (i, line) in nfkc.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(line.trim_end());
    }
    out.replace(['\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'], "'")
        .replace(['\u{201C}', '\u{201D}', '\u{201E}', '\u{201F}'], "\"")
        .replace(
            [
                '\u{2010}', '\u{2011}', '\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}',
            ],
            "-",
        )
        .replace(
            [
                '\u{00A0}', '\u{2002}', '\u{2003}', '\u{2004}', '\u{2005}', '\u{2006}', '\u{2007}',
                '\u{2008}', '\u{2009}', '\u{200A}', '\u{202F}', '\u{205F}', '\u{3000}',
            ],
            " ",
        )
}

fn lines_with_endings(content: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (idx, _) in content.match_indices('\n') {
        lines.push(&content[start..idx + 1]);
        start = idx + 1;
    }
    if start < content.len() {
        lines.push(&content[start..]);
    }
    lines
}

fn line_spans(content: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in lines_with_endings(content) {
        spans.push((offset, offset + line.len()));
        offset += line.len();
    }
    spans
}

fn replacement_line_range(
    spans: &[(usize, usize)],
    match_start: usize,
    match_end: usize,
) -> Result<(usize, usize), String> {
    let mut first = None;
    for (i, span) in spans.iter().enumerate() {
        if match_start >= span.0 && match_start < span.1 {
            first = Some(i);
            break;
        }
    }
    let Some(mut last) = first else {
        return Err("replacement range is outside the base content".into());
    };
    while last < spans.len() && spans[last].1 < match_end {
        last += 1;
    }
    if last >= spans.len() {
        return Err("replacement range is outside the base content".into());
    }
    Ok((first.unwrap(), last + 1))
}

fn apply_replacements_preserving_unchanged_lines(
    original: &str,
    base: &str,
    matched: &[MatchedEdit],
) -> Result<String, String> {
    let original_lines = lines_with_endings(original);
    let spans = line_spans(base);
    if original_lines.len() != spans.len() {
        return Err(
            "fuzzy-matched edit could not map to the original file (line count mismatch)".into(),
        );
    }
    let mut groups: Vec<(usize, usize, Vec<&MatchedEdit>)> = Vec::new();
    for m in matched {
        let (start_line, end_line) = replacement_line_range(&spans, m.start, m.start + m.len)?;
        let merged = match groups.last_mut() {
            Some((_, group_end, list)) if start_line < *group_end => {
                *group_end = (*group_end).max(end_line);
                list.push(m);
                true
            }
            _ => false,
        };
        if !merged {
            groups.push((start_line, end_line, vec![m]));
        }
    }
    let mut result = String::with_capacity(original.len());
    let mut original_index = 0;
    for (group_start, group_end, replacements) in &groups {
        result.push_str(&original_lines[original_index..*group_start].concat());
        let slice_start = spans[*group_start].0;
        let slice_end = spans[*group_end - 1].1;
        let mut group_text = base[slice_start..slice_end].to_string();
        for m in replacements.iter().rev() {
            let at = m.start - slice_start;
            group_text.replace_range(at..at + m.len, &m.new_text);
        }
        result.push_str(&group_text);
        original_index = *group_end;
    }
    result.push_str(&original_lines[original_index..].concat());
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_util::{new_ctx, tempdir};

    #[tokio::test]
    async fn edit_applies_multiple_disjoint_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha beta\ngamma delta\n").unwrap();
        let mut ctx = new_ctx();
        let out = EditFile::new(None, FileLocks::new())
            .call(
                &mut ctx,
                json!({
                    "path": "e.txt",
                    "edits": [
                        { "oldText": "gamma delta", "newText": "gamma delta epsilon" },
                        { "oldText": "alpha", "newText": "ALPHA" }
                    ]
                }),
            )
            .await
            .unwrap();
        assert!(out.as_text().unwrap().contains("2 edits applied"));
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "ALPHA beta\ngamma delta epsilon\n"
        );
        assert!(matches!(
            ctx.result::<FileChange>(),
            Some(FileChange::Edit { diff, .. }) if !diff.is_empty()
        ));
        drop(dir);
    }

    #[tokio::test]
    async fn edit_rejects_ambiguous_missing_empty_and_noop() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "a b a").unwrap();
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "a", "newText": "x" }] }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must be unique"),
            "{}",
            err.to_string()
        );
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "zzz", "newText": "x" }] }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Could not find"));
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "", "newText": "x" }] }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("must not be empty"),
            "{}",
            err.to_string()
        );
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "a b", "newText": "a b" }] }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("No changes made"),
            "{}",
            err.to_string()
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_rejects_overlapping_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "abcd").unwrap();
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [
                        { "oldText": "abc", "newText": "x" },
                        { "oldText": "bcd", "newText": "y" }
                    ]
                }),
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("edits[0] and edits[1] overlap")
                || msg.contains("edits[1] and edits[0] overlap"),
            "{}",
            msg
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_accepts_legacy_flat_and_string_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "hello world").unwrap();
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "old": "hello", "new": "hi" }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi world");
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": { "oldText": "world", "newText": "there" } }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi there");
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": "[{\"oldText\":\"there\",\"newText\":\"you\"}]" }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi you");
        drop(dir);
    }

    #[tokio::test]
    async fn edit_preserves_bom_and_crlf() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "\u{FEFF}one\r\ntwo\r\n").unwrap();
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "one", "newText": "1\n2" }] }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "\u{FEFF}1\r\n2\r\ntwo\r\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn edit_fuzzy_matches_unicode_and_trailing_ws() {
        let (dir, _guard) = tempdir();
        std::fs::write(
            "fuzzy.txt",
            "say \u{201C}hello\u{201D} ok\nplain line\n\u{2014}\u{2014}\n",
        )
        .unwrap();
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "fuzzy.txt",
                    "edits": [{ "oldText": "\"hello\" ok", "newText": "'goodbye' ok" }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("fuzzy.txt").unwrap(),
            "say 'goodbye' ok\nplain line\n\u{2014}\u{2014}\n"
        );
        std::fs::write("ws.txt", "head   \nkeep  me\n").unwrap();
        let err = EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "ws.txt",
                    "edits": [{ "oldText": "keep me", "newText": "KEEP ME" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("Could not find"),
            "internal double spaces are not normalized: {}",
            err
        );
        EditFile::new(None, FileLocks::new())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "ws.txt",
                    "edits": [{ "oldText": "head\nkeep  me", "newText": "HEAD\nKEEP  ME" }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("ws.txt").unwrap(),
            "HEAD\nKEEP  ME\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn concurrent_edits_serialize_per_path() {
        let (dir, _guard) = tempdir();
        std::fs::write("f.txt", "token alpha and token beta\n").unwrap();
        let locks = FileLocks::new();
        let tool_a = EditFile::new(None, locks.clone());
        let tool_b = EditFile::new(None, locks);
        let (ra, rb) = tokio::join!(
            async {
                tool_a
                    .call(
                        &mut new_ctx(),
                        json!({ "path": "f.txt", "edits": [{ "oldText": "alpha", "newText": "ALPHA" }] }),
                    )
                    .await
            },
            async {
                tool_b
                    .call(
                        &mut new_ctx(),
                        json!({ "path": "f.txt", "edits": [{ "oldText": "beta", "newText": "BETA" }] }),
                    )
                    .await
            }
        );
        ra.unwrap();
        rb.unwrap();
        assert_eq!(
            std::fs::read_to_string("f.txt").unwrap(),
            "token ALPHA and token BETA\n"
        );
        drop(dir);
    }
}
