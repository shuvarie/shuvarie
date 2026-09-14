use std::path::Path;

use serde_json::{Value, json};
use shuvarie_llm::{FileChange, Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::lsp_manager::SharedManager;
use crate::permissions::{Access, PathKind, resolve_read};

use super::{FileLocks, arg_value, compute_diff};

pub(crate) struct EditFile {
    lsp: Option<SharedManager>,
    locks: FileLocks,
    access: Access,
}

impl EditFile {
    pub(crate) fn new(lsp: Option<SharedManager>, locks: FileLocks, access: Access) -> Self {
        Self { lsp, locks, access }
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
         unchanged regions just to connect distant changes. Each edit may be scoped with startLine \
         (1-based, inclusive) and lineCount (defaults to 1): the oldText search then runs only within \
         those lines, which disambiguates text repeated elsewhere in the file. With wholeLine toggled \
         on an edit replaces the whole lines in its scope instead of matching oldText: omit oldText, \
         give startLine (required) and the replacement lines as newText. An empty newText deletes \
         the scoped lines; otherwise the replacement keeps the replaced block's trailing newline."
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
                            "oldText": { "type": "string", "description": "Exact text for one targeted replacement; must be unique within the edit's scope (the whole file when unscoped). Required unless wholeLine is true" },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit" },
                            "startLine": { "type": "integer", "minimum": 1, "description": "First line of the edit's scope (1-based, inclusive). Required for wholeLine edits" },
                            "lineCount": { "type": "integer", "minimum": 1, "description": "Number of lines in the scope starting at startLine. Defaults to 1" },
                            "wholeLine": { "type": "boolean", "description": "Replace the whole lines in scope with newText instead of matching oldText; oldText must be omitted" }
                        },
                        "required": ["newText"]
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
        let access = self.access.clone();
        let result: Result<ToolOutput, String> = async move {
            let path = arg_value(&args, "path")?;
            let edits = parse_edits(&args)?;
            let abs = resolve_read(&path)?;
            access.authorize_path(PathKind::Write, &abs, &path).await?;
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
    old_text: Option<String>,
    new_text: String,
    start_line: Option<usize>,
    line_count: Option<usize>,
    whole_line: bool,
}

struct MatchedEdit {
    index: usize,
    start: usize,
    len: usize,
    new_text: String,
}

fn edit_from_value(value: &Value) -> Option<TextEdit> {
    let new = value
        .get("newText")
        .or_else(|| value.get("new"))
        .and_then(Value::as_str)?;
    let old = value
        .get("oldText")
        .or_else(|| value.get("old"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let whole_line = value
        .get("wholeLine")
        .or_else(|| value.get("whole_line"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let start_line = value
        .get("startLine")
        .or_else(|| value.get("start_line"))
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let line_count = value
        .get("lineCount")
        .or_else(|| value.get("line_count"))
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    Some(TextEdit {
        old_text: old,
        new_text: new.to_string(),
        start_line,
        line_count,
        whole_line,
    })
}

fn parse_edits(args: &Value) -> Result<Vec<TextEdit>, String> {
    let mut edits: Vec<TextEdit> = Vec::new();
    match args.get("edits") {
        Some(Value::Array(items)) => {
            for (i, item) in items.iter().enumerate() {
                edits.push(edit_from_value(item).ok_or_else(|| {
                    format!("edits[{i}] must be an object with a string newText")
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
                    .ok_or_else(|| "'edits' string did not contain a string newText".to_string());
            }
            _ => return Err("'edits' string did not parse as a JSON array".into()),
        },
        Some(single @ Value::Object(_)) => {
            edits.push(
                edit_from_value(single)
                    .ok_or_else(|| "'edits' object needs a string newText".to_string())?,
            );
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

fn edit_label(single: bool, i: usize) -> String {
    if single {
        String::new()
    } else {
        format!("edits[{i}]: ")
    }
}

fn scope_lines(edit: &TextEdit) -> Option<(usize, usize)> {
    let start = edit.start_line?;
    Some((
        start,
        start.saturating_add(edit.line_count.unwrap_or(1) - 1),
    ))
}

fn scope_window(edit: &TextEdit, spans: &[(usize, usize)]) -> Option<(usize, usize)> {
    let (first, last) = scope_lines(edit)?;
    Some((spans[first - 1].0, spans[last - 1].1))
}

fn validate_edit(
    edit: &TextEdit,
    i: usize,
    single: bool,
    line_total: usize,
    path: &str,
) -> Result<(), String> {
    let label = edit_label(single, i);
    if edit.whole_line {
        if edit.start_line.is_none() {
            return Err(format!(
                "{label}wholeLine edits require startLine (the first line to replace)."
            ));
        }
        if edit.old_text.is_some() {
            return Err(format!(
                "{label}wholeLine edits replace whole lines; omit oldText and give the replacement as newText."
            ));
        }
    } else {
        let Some(old) = edit.old_text.as_deref() else {
            return Err(format!(
                "{label}oldText is required unless wholeLine is true."
            ));
        };
        if old.is_empty() {
            return Err(format!("{label}oldText must not be empty."));
        }
    }
    if edit.line_count.is_some() && edit.start_line.is_none() {
        return Err(format!("{label}lineCount requires startLine."));
    }
    let Some(start) = edit.start_line else {
        return Ok(());
    };
    if start == 0 {
        return Err(format!(
            "{label}startLine is 1-based and must be at least 1."
        ));
    }
    let count = edit.line_count.unwrap_or(1);
    if count == 0 {
        return Err(format!("{label}lineCount must be at least 1."));
    }
    let end = start.saturating_add(count - 1);
    if end > line_total {
        return Err(format!(
            "{label}line range {start}-{end} is beyond the end of {path} ({line_total} lines)."
        ));
    }
    Ok(())
}

fn whole_line_replacement(new_text: &str, had_trailing_newline: bool) -> String {
    if new_text.is_empty() {
        return String::new();
    }
    let mut replacement = new_text.to_string();
    if had_trailing_newline && !replacement.ends_with('\n') {
        replacement.push('\n');
    }
    replacement
}

fn cannot_find(edit: &TextEdit, path: &str) -> String {
    match scope_lines(edit) {
        Some((first, last)) => format!(
            "Could not find the text in {path} within lines {first}-{last}. It must match the file content exactly, including all whitespace and newlines, and fit entirely inside the scoped lines."
        ),
        None => format!(
            "Could not find the text in {path}. It must match the file content exactly, including all whitespace and newlines."
        ),
    }
}

fn not_unique(edit: &TextEdit, path: &str, count: usize) -> String {
    match scope_lines(edit) {
        Some((first, last)) => format!(
            "Found {count} occurrences of the text in {path} within lines {first}-{last}. The text must be unique within the scope; narrow the scope or extend the surrounding lines to disambiguate."
        ),
        None => format!(
            "Found {count} occurrences of the text in {path}. The text must be unique; include more surrounding lines or add a startLine/lineCount scope to disambiguate."
        ),
    }
}

fn apply_edits(base: &str, edits: &[TextEdit], path: &str) -> Result<String, String> {
    let single = edits.len() == 1;
    let spans = line_spans(base);
    for (i, edit) in edits.iter().enumerate() {
        validate_edit(edit, i, single, spans.len(), path)?;
    }
    let mut used_fuzzy = false;
    let mut normalized: Option<String> = None;
    for (i, edit) in edits.iter().enumerate() {
        if edit.whole_line {
            continue;
        }
        let old = edit.old_text.as_deref().unwrap_or_default();
        let exact = match scope_window(edit, &spans) {
            Some((start, end)) => base[start..end].contains(old),
            None => base.contains(old),
        };
        if exact {
            continue;
        }
        let nbase = normalized.get_or_insert_with(|| normalize_for_fuzzy_match(base));
        let nspans = line_spans(nbase);
        let normalized_old = normalize_for_fuzzy_match(old);
        let fuzzy = match scope_window(edit, &nspans) {
            Some((start, end)) => nbase[start..end].contains(&normalized_old),
            None => nbase.contains(&normalized_old),
        };
        if !fuzzy {
            return Err(format!(
                "{}{}",
                edit_label(single, i),
                cannot_find(edit, path)
            ));
        }
        used_fuzzy = true;
    }
    let replacement_base = if used_fuzzy {
        normalized.expect("used fuzzy implies a normalized base")
    } else {
        base.to_string()
    };
    let rspans = line_spans(&replacement_base);
    let mut matched: Vec<MatchedEdit> = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        if edit.whole_line {
            let (first, last) = scope_lines(edit).expect("validated scope");
            let start = rspans[first - 1].0;
            let end = rspans[last - 1].1;
            let region = &base[spans[first - 1].0..spans[last - 1].1];
            matched.push(MatchedEdit {
                index: i,
                start,
                len: end - start,
                new_text: whole_line_replacement(&edit.new_text, region.ends_with('\n')),
            });
            continue;
        }
        let old = edit.old_text.as_deref().unwrap_or_default();
        let (search_start, hay) = match scope_window(edit, &rspans) {
            Some((start, end)) => (start, &replacement_base[start..end]),
            None => (0, replacement_base.as_str()),
        };
        let (index, len, _) = fuzzy_find(hay, old);
        let occurrences = count_fuzzy_occurrences(hay, old);
        match (index, occurrences) {
            (None, _) => {
                return Err(format!(
                    "{}{}",
                    edit_label(single, i),
                    cannot_find(edit, path)
                ));
            }
            (Some(offset), 1) => matched.push(MatchedEdit {
                index: i,
                start: search_start + offset,
                len,
                new_text: edit.new_text.clone(),
            }),
            (Some(_), n) => {
                return Err(format!(
                    "{}{}",
                    edit_label(single, i),
                    not_unique(edit, path, n)
                ));
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
    use crate::test_util::{new_ctx, tempdir};

    #[tokio::test]
    async fn edit_applies_multiple_disjoint_edits() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha beta\ngamma delta\n").unwrap();
        let mut ctx = new_ctx();
        let out = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": [{ "oldText": "zzz", "newText": "x" }] }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Could not find"));
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "old": "hello", "new": "hi" }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi world");
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "edits": { "oldText": "world", "newText": "there" } }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "hi there");
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
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
        let tool_a = EditFile::new(None, locks.clone(), crate::test_util::access());
        let tool_b = EditFile::new(None, locks, crate::test_util::access());
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

    #[tokio::test]
    async fn whole_line_edits_replace_scoped_lines() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "one\ntwo\nthree\nfour\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [
                        { "wholeLine": true, "startLine": 2, "newText": "TWO" },
                        { "wholeLine": true, "startLine": 3, "lineCount": 2, "newText": "III\nIV" }
                    ]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "one\nTWO\nIII\nIV\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn whole_line_edits_delete_and_replace_last_line() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "one\ntwo\nthree\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "wholeLine": true, "startLine": 2, "newText": "" }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "one\nthree\n");
        std::fs::write("last.txt", "one\nlast").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "last.txt",
                    "edits": [{ "wholeLine": true, "startLine": 2, "newText": "final" }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("last.txt").unwrap(), "one\nfinal");
        drop(dir);
    }

    #[tokio::test]
    async fn whole_line_edits_reject_bad_arguments() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "one\ntwo\n").unwrap();
        let tool = || EditFile::new(None, FileLocks::new(), crate::test_util::access());
        let err = tool()
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "wholeLine": true, "newText": "TWO" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("wholeLine edits require startLine"),
            "{}",
            err
        );
        let err = tool()
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "wholeLine": true, "startLine": 2, "oldText": "two", "newText": "TWO" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("omit oldText"), "{}", err);
        let err = tool()
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "wholeLine": true, "startLine": 5, "newText": "FIVE" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("beyond the end of e.txt (2 lines)"),
            "{}",
            err
        );
        let err = tool()
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "one", "lineCount": 2, "newText": "x" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("lineCount requires startLine"),
            "{}",
            err
        );
        let err = tool()
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "newText": "x" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("oldText is required unless wholeLine is true"),
            "{}",
            err
        );
        drop(dir);
    }

    #[tokio::test]
    async fn scoped_text_edits_limit_search_and_disambiguate() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha\nbeta gamma\nalpha beta\nalpha\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "alpha", "newText": "ALPHA", "startLine": 1, "lineCount": 1 }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "ALPHA\nbeta gamma\nalpha beta\nalpha\n"
        );
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "ALPHA", "newText": "nope", "startLine": 3, "lineCount": 1 }]
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("within lines 3-3"), "{}", err);
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "alpha beta\nalpha", "newText": "x", "startLine": 3, "lineCount": 1 }]
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("within lines 3-3"), "{}", err);
        std::fs::write("amb.txt", "a a\nb\n").unwrap();
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "amb.txt",
                    "edits": [{ "oldText": "a", "newText": "z", "startLine": 1 }]
                }),
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Found 2 occurrences of the text in amb.txt within lines 1-1"),
            "{}",
            err
        );
        drop(dir);
    }

    #[tokio::test]
    async fn scoped_text_edits_fuzzy_match_within_scope() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha   \nbeta\ngamma\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "alpha\nbeta", "newText": "ONE\nTWO", "startLine": 1, "lineCount": 2 }]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "ONE\nTWO\ngamma\n"
        );
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "oldText": "ONE\nTWO", "newText": "x", "startLine": 2, "lineCount": 1 }]
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("within lines 2-2"), "{}", err);
        drop(dir);
    }

    #[tokio::test]
    async fn mixed_edits_apply_together() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha beta\ngamma delta\nepsilon\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [
                        { "wholeLine": true, "startLine": 2, "newText": "GAMMA" },
                        { "oldText": "alpha", "newText": "ALPHA" }
                    ]
                }),
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string("e.txt").unwrap(),
            "ALPHA beta\nGAMMA\nepsilon\n"
        );
        drop(dir);
    }

    #[tokio::test]
    async fn mixed_edits_reject_overlap_across_modes() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "alpha beta\ngamma\n").unwrap();
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [
                        { "wholeLine": true, "startLine": 1, "newText": "ALPHA" },
                        { "oldText": "beta", "newText": "BETA" }
                    ]
                }),
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("edits[0] and edits[1] overlap"), "{}", msg);
        drop(dir);
    }

    #[tokio::test]
    async fn whole_line_accepts_legacy_flat_args() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "one\ntwo\n").unwrap();
        EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({ "path": "e.txt", "wholeLine": true, "startLine": 2, "new": "TWO" }),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string("e.txt").unwrap(), "one\nTWO\n");
        drop(dir);
    }

    #[tokio::test]
    async fn whole_line_noop_is_rejected() {
        let (dir, _guard) = tempdir();
        std::fs::write("e.txt", "one\ntwo\n").unwrap();
        let err = EditFile::new(None, FileLocks::new(), crate::test_util::access())
            .call(
                &mut new_ctx(),
                json!({
                    "path": "e.txt",
                    "edits": [{ "wholeLine": true, "startLine": 2, "newText": "two" }]
                }),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("No changes made"), "{}", err);
        drop(dir);
    }
}
