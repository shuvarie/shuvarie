use serde_json::{Value, json};
use shuvarie_llm::{
    FileChange, PatchFileChange, PatchFileKind, Tool, ToolContext, ToolExecutionError, ToolOutput,
};

use crate::lsp_manager::SharedManager;
use crate::permissions::resolve_write;
use crate::tools::{FileLocks, arg_value, compute_diff};

#[derive(Debug, Clone, PartialEq)]
pub enum Hunk {
    Add {
        path: String,
        contents: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_path: Option<String>,
        chunks: Vec<UpdateChunk>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpdateChunk {
    old_lines: Vec<String>,
    new_lines: Vec<String>,
    change_context: Option<String>,
    is_end_of_file: bool,
}

const BEGIN_MARKER: &str = "*** Begin Patch";
const END_MARKER: &str = "*** End Patch";
const ADD_HEADER: &str = "*** Add File: ";
const DELETE_HEADER: &str = "*** Delete File: ";
const UPDATE_HEADER: &str = "*** Update File: ";
const MOVE_HEADER: &str = "*** Move to: ";
const END_OF_FILE_MARKER: &str = "*** End of File";

fn strip_heredoc(input: &str) -> &str {
    let trimmed = input.trim();
    let Some((first, rest)) = trimmed.split_once('\n') else {
        return trimmed;
    };
    let first = first.trim();
    let Some(body) = first
        .strip_prefix("cat ")
        .or_else(|| first.strip_prefix("<<"))
        .filter(|_| first.contains("<<"))
    else {
        return trimmed;
    };
    let header = body.trim();
    let delimiter = header
        .trim_start_matches("<<")
        .trim_matches(['\'', '"'])
        .trim();
    if delimiter.is_empty() {
        return trimmed;
    }
    let mut lines = rest.lines().rev();
    let last = lines.next().unwrap_or("").trim();
    if last == delimiter {
        let upto = rest.len() - last.len();
        let end = rest[..upto].trim_end_matches(['\n', '\r']).len();
        rest[..end].trim_end()
    } else {
        trimmed
    }
}

fn parse_hunk_header(line: &str) -> Option<(&'static str, String)> {
    for (prefix, tag) in [
        (ADD_HEADER, "add"),
        (DELETE_HEADER, "delete"),
        (UPDATE_HEADER, "update"),
    ] {
        if let Some(path) = line.strip_prefix(prefix) {
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            return Some((tag, path.to_string()));
        }
    }
    None
}

pub fn parse_patch(patch_text: &str) -> Result<Vec<Hunk>, String> {
    let cleaned = strip_heredoc(patch_text);
    let lines: Vec<&str> = cleaned.lines().collect();

    let begin_idx = lines
        .iter()
        .position(|l| l.trim() == BEGIN_MARKER)
        .ok_or("Invalid patch format: missing Begin marker")?;
    let end_idx = lines
        .iter()
        .position(|l| l.trim() == END_MARKER)
        .ok_or("Invalid patch format: missing End marker")?;
    if begin_idx >= end_idx {
        return Err("Invalid patch format: Begin marker after End marker".into());
    }

    let mut hunks = Vec::new();
    let mut i = begin_idx + 1;
    while i < end_idx {
        let line = lines[i];
        if line.starts_with(ADD_HEADER) {
            let Some((_, path)) = parse_hunk_header(line) else {
                i += 1;
                continue;
            };
            let mut contents = String::new();
            i += 1;
            while i < end_idx && !lines[i].starts_with("***") {
                if let Some(content) = lines[i].strip_prefix('+') {
                    contents.push_str(content);
                    contents.push('\n');
                }
                i += 1;
            }
            if contents.ends_with('\n') {
                contents.pop();
            }
            hunks.push(Hunk::Add { path, contents });
        } else if line.starts_with(DELETE_HEADER) {
            let Some((_, path)) = parse_hunk_header(line) else {
                i += 1;
                continue;
            };
            hunks.push(Hunk::Delete { path });
            i += 1;
        } else if line.starts_with(UPDATE_HEADER) {
            let Some((_, path)) = parse_hunk_header(line) else {
                i += 1;
                continue;
            };
            i += 1;
            let mut move_path = None;
            if i < end_idx && lines[i].starts_with(MOVE_HEADER) {
                let target = lines[i][MOVE_HEADER.len()..].trim();
                if !target.is_empty() {
                    move_path = Some(target.to_string());
                }
                i += 1;
            }
            let mut chunks = Vec::new();
            while i < end_idx && !lines[i].starts_with("***") {
                if let Some(rest) = lines[i].strip_prefix("@@") {
                    let context = rest.trim();
                    let mut context = context.strip_prefix('!').map(str::trim).unwrap_or(context);
                    if context == "@@" {
                        context = "";
                    }
                    i += 1;
                    let mut old_lines = Vec::new();
                    let mut new_lines = Vec::new();
                    let mut is_end_of_file = false;
                    while i < end_idx
                        && !lines[i].starts_with("@@")
                        && (!lines[i].starts_with("***") || lines[i].trim() == END_OF_FILE_MARKER)
                    {
                        let change_line = lines[i];
                        if change_line.trim() == END_OF_FILE_MARKER {
                            is_end_of_file = true;
                            i += 1;
                            break;
                        }
                        if let Some(content) = change_line.strip_prefix(' ') {
                            old_lines.push(content.to_string());
                            new_lines.push(content.to_string());
                        } else if let Some(content) = change_line.strip_prefix('-') {
                            old_lines.push(content.to_string());
                        } else if let Some(content) = change_line.strip_prefix('+') {
                            new_lines.push(content.to_string());
                        }
                        i += 1;
                    }
                    chunks.push(UpdateChunk {
                        old_lines,
                        new_lines,
                        change_context: (!context.is_empty()).then(|| context.to_string()),
                        is_end_of_file,
                    });
                } else {
                    i += 1;
                }
            }
            hunks.push(Hunk::Update {
                path,
                move_path,
                chunks,
            });
        } else {
            i += 1;
        }
    }
    Ok(hunks)
}

fn normalize_unicode(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            '\u{2026}' => '.',
            _ => c,
        })
        .collect()
}

fn try_match(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    eq: impl Fn(&str, &str) -> bool,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() || pattern.len() > lines.len() {
        return None;
    }
    if eof {
        let from_end = lines.len() - pattern.len();
        if from_end >= start_index && lines[from_end..].iter().zip(pattern).all(|(a, b)| eq(a, b)) {
            return Some(from_end);
        }
    }
    for i in start_index..=lines.len() - pattern.len() {
        if lines[i..i + pattern.len()]
            .iter()
            .zip(pattern)
            .all(|(a, b)| eq(a, b))
        {
            return Some(i);
        }
    }
    None
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start_index: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return None;
    }
    if pattern.iter().any(|p| p.chars().any(|c| c as u32 > 127))
        && let Some(found) = try_match(
            lines,
            pattern,
            start_index,
            |a, b| normalize_unicode(a.trim()) == normalize_unicode(b.trim()),
            eof,
        )
    {
        return Some(found);
    }
    if let Some(found) = try_match(lines, pattern, start_index, |a, b| a == b, eof) {
        return Some(found);
    }
    if let Some(found) = try_match(
        lines,
        pattern,
        start_index,
        |a, b| a.trim_end() == b.trim_end(),
        eof,
    ) {
        return Some(found);
    }
    try_match(
        lines,
        pattern,
        start_index,
        |a, b| a.trim() == b.trim(),
        eof,
    )
}

type Replacement = (usize, usize, Vec<String>);

fn compute_replacements(
    original_lines: &[String],
    file_path: &str,
    chunks: &[UpdateChunk],
) -> Result<Vec<Replacement>, String> {
    let mut replacements: Vec<Replacement> = Vec::new();
    let mut line_index = 0usize;

    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            let context_idx = seek_sequence(
                original_lines,
                std::slice::from_ref(context),
                line_index,
                false,
            )
            .ok_or_else(|| format!("Failed to find context '{context}' in {file_path}"))?;
            line_index = context_idx + 1;
        }

        if chunk.old_lines.is_empty() {
            let insertion_idx = if original_lines.last().is_some_and(|l| l.is_empty()) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_idx, 0, chunk.new_lines.clone()));
            continue;
        }

        let mut pattern = chunk.old_lines.clone();
        let mut new_slice = chunk.new_lines.clone();
        let mut found = seek_sequence(original_lines, &pattern, line_index, chunk.is_end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern.pop();
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice.pop();
            }
            found = seek_sequence(original_lines, &pattern, line_index, chunk.is_end_of_file);
        }

        match found {
            Some(idx) => {
                replacements.push((idx, pattern.len(), new_slice));
                line_index = idx + pattern.len();
            }
            None => {
                return Err(format!(
                    "Failed to find expected lines in {file_path}:\n{}",
                    chunk.old_lines.join("\n")
                ));
            }
        }
    }

    replacements.sort_by_key(|(idx, _, _)| *idx);
    Ok(replacements)
}

fn apply_replacements(lines: &[String], replacements: &[Replacement]) -> Vec<String> {
    let mut result: Vec<String> = lines.to_vec();
    for &(start, old_len, ref new_segment) in replacements.iter().rev() {
        let segment = new_segment.clone();
        result.splice(start..start + old_len, segment);
    }
    result
}

fn derive_new_contents(
    file_path: &str,
    chunks: &[UpdateChunk],
    original_text: &str,
) -> Result<String, String> {
    let mut original_lines: Vec<String> = original_text.split('\n').map(str::to_string).collect();
    if original_lines.last().is_some_and(String::is_empty) {
        original_lines.pop();
    }

    let replacements = compute_replacements(&original_lines, file_path, chunks)?;
    let mut new_lines = apply_replacements(&original_lines, &replacements);

    if new_lines.is_empty() || !new_lines.last().is_some_and(String::is_empty) {
        new_lines.push(String::new());
    }
    Ok(new_lines.join("\n"))
}

pub struct ApplyPatch {
    lsp: Option<SharedManager>,
    locks: FileLocks,
}

impl ApplyPatch {
    pub fn new(lsp: Option<SharedManager>, locks: FileLocks) -> Self {
        Self { lsp, locks }
    }
}

impl Tool for ApplyPatch {
    const NAME: &'static str = "apply_patch";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Apply a patch that updates multiple files in one call. The patch uses the \
         high-level envelope format: `*** Begin Patch` … `*** End Patch` with \
         `*** Add File:` / `*** Delete File:` / `*** Update File:` sections \
         (optional `*** Move to:` for renames) and `@@`-marked change chunks \
         whose lines are prefixed with ` ` (context), `-` (remove), or `+` (add)."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "patchText": { "type": "string", "description": "The full patch text that describes all changes to be made" }
            },
            "required": ["patchText"]
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
            let patch_text = arg_value(&args, "patchText")?;
            let hunks = parse_patch(&patch_text)?;
            if hunks.is_empty() {
                return Err("patch rejected: no hunks found".into());
            }

            let mut lock_keys: Vec<std::path::PathBuf> = Vec::new();
            for hunk in &hunks {
                match hunk {
                    Hunk::Add { path, .. } => {
                        lock_keys.push(resolve_write(path)?);
                    }
                    Hunk::Delete { path, .. } => {
                        lock_keys.push(resolve_write(path)?);
                    }
                    Hunk::Update {
                        path, move_path, ..
                    } => {
                        lock_keys.push(resolve_write(path)?);
                        if let Some(target) = move_path {
                            lock_keys.push(resolve_write(target)?);
                        }
                    }
                }
            }
            lock_keys.sort();
            lock_keys.dedup();
            let mut patch_locks = Vec::new();
            for key in &lock_keys {
                patch_locks.push(locks.lock(key).await);
            }

            enum Planned {
                Add {
                    path: String,
                    content: String,
                },
                Update {
                    path: String,
                    content: String,
                },
                Move {
                    from: String,
                    to: String,
                    content: String,
                },
                Delete {
                    path: String,
                },
            }

            let mut planned: Vec<Planned> = Vec::new();
            let mut changes: Vec<PatchFileChange> = Vec::new();

            for hunk in &hunks {
                match hunk {
                    Hunk::Add { path, contents } => {
                        let content = if contents.is_empty() || contents.ends_with('\n') {
                            contents.clone()
                        } else {
                            format!("{contents}\n")
                        };
                        let abs = resolve_write(path)?;
                        if abs.exists() {
                            return Err(format!("cannot add {path}: file already exists"));
                        }
                        let diff = compute_diff("", &content);
                        changes.push(PatchFileChange {
                            path: path.clone(),
                            kind: PatchFileKind::Add,
                            moved_to: None,
                            original: None,
                            new: Some(content.clone()),
                            diff,
                        });
                        planned.push(Planned::Add {
                            path: path.clone(),
                            content,
                        });
                    }
                    Hunk::Delete { path } => {
                        let abs = resolve_write(path)?;
                        let original = tokio::fs::read_to_string(&abs)
                            .await
                            .map_err(|e| format!("read {path}: {e}"))?;
                        let diff = compute_diff(&original, "");
                        changes.push(PatchFileChange {
                            path: path.clone(),
                            kind: PatchFileKind::Delete,
                            moved_to: None,
                            original: Some(original),
                            new: None,
                            diff,
                        });
                        planned.push(Planned::Delete {
                            path: abs.to_string_lossy().into_owned(),
                        });
                    }
                    Hunk::Update {
                        path,
                        move_path,
                        chunks,
                    } => {
                        let abs = resolve_write(path)?;
                        if let Some(target) = move_path {
                            let target_abs = resolve_write(target)?;
                            if target_abs == abs {
                                return Err(format!("move target {target} matches source {path}"));
                            }
                        }
                        let original = tokio::fs::read_to_string(&abs)
                            .await
                            .map_err(|e| format!("read {path}: {e}"))?;
                        let new = derive_new_contents(path, chunks, &original)?;
                        let diff = compute_diff(&original, &new);
                        changes.push(PatchFileChange {
                            path: path.clone(),
                            kind: PatchFileKind::Update,
                            moved_to: move_path.clone(),
                            original: Some(original),
                            new: Some(new.clone()),
                            diff,
                        });
                        if let Some(target) = move_path {
                            planned.push(Planned::Move {
                                from: abs.to_string_lossy().into_owned(),
                                to: target.clone(),
                                content: new,
                            });
                        } else {
                            planned.push(Planned::Update {
                                path: abs.to_string_lossy().into_owned(),
                                content: new,
                            });
                        }
                    }
                }
            }

            let mut summary_lines = Vec::new();
            for planned in &planned {
                match planned {
                    Planned::Add { path, content } => {
                        if let Some(parent) = std::path::Path::new(path).parent() {
                            tokio::fs::create_dir_all(parent)
                                .await
                                .map_err(|e| format!("create dir for {path}: {e}"))?;
                        }
                        tokio::fs::write(path, content)
                            .await
                            .map_err(|e| format!("write {path}: {e}"))?;
                        summary_lines.push(format!("A {path}"));
                    }
                    Planned::Update { path, content } => {
                        tokio::fs::write(path, content)
                            .await
                            .map_err(|e| format!("write {path}: {e}"))?;
                        summary_lines.push(format!("M {path}"));
                    }
                    Planned::Move { from, to, content } => {
                        if let Some(parent) = std::path::Path::new(to).parent() {
                            tokio::fs::create_dir_all(parent)
                                .await
                                .map_err(|e| format!("create dir for {to}: {e}"))?;
                        }
                        tokio::fs::write(to, content)
                            .await
                            .map_err(|e| format!("write {to}: {e}"))?;
                        tokio::fs::remove_file(from)
                            .await
                            .map_err(|e| format!("remove {from}: {e}"))?;
                        summary_lines.push(format!("M {from} -> {to}"));
                    }
                    Planned::Delete { path } => {
                        tokio::fs::remove_file(path)
                            .await
                            .map_err(|e| format!("remove {path}: {e}"))?;
                        summary_lines.push(format!("D {path}"));
                    }
                }
            }

            for planned in &planned {
                let (notify_path, notify_content) = match planned {
                    Planned::Add { path, content } | Planned::Update { path, content } => {
                        (path.clone(), Some(content.clone()))
                    }
                    Planned::Move { to, content, .. } => (to.clone(), Some(content.clone())),
                    Planned::Delete { .. } => continue,
                };
                if let Some(lsp) = &lsp {
                    lsp.lock()
                        .await
                        .on_file_change(
                            std::path::Path::new(&notify_path),
                            &notify_content.unwrap_or_default(),
                        )
                        .await;
                }
            }

            let output = format!(
                "Success. Updated the following files:\n{}",
                summary_lines.join("\n")
            );
            ctx.insert_result(FileChange::Patch { files: changes });
            Ok(ToolOutput::text(output))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_llm::PatchFileKind;

    #[test]
    fn parses_add_update_delete_and_move() {
        let patch = r#"*** Begin Patch
*** Add File: new.txt
+hello
+world
*** Update File: src/app.rs
*** Move to: src/main.rs
@@ fn old():
-fn old() {}
+fn new() {}
*** Delete File: obsolete.txt
*** End Patch"#;
        let hunks = parse_patch(patch).unwrap();
        assert_eq!(
            hunks,
            vec![
                Hunk::Add {
                    path: "new.txt".into(),
                    contents: "hello\nworld".into(),
                },
                Hunk::Update {
                    path: "src/app.rs".into(),
                    move_path: Some("src/main.rs".into()),
                    chunks: vec![UpdateChunk {
                        old_lines: vec!["fn old() {}".into()],
                        new_lines: vec!["fn new() {}".into()],
                        change_context: Some("fn old():".into()),
                        is_end_of_file: false,
                    }],
                },
                Hunk::Delete {
                    path: "obsolete.txt".into(),
                },
            ]
        );
    }

    #[test]
    fn parses_end_of_file_anchor() {
        let patch = "*** Begin Patch\n*** Update File: a.txt\n@@\n x\n+tail\n*** End of File\n*** End Patch";
        let hunks = parse_patch(patch).unwrap();
        let Hunk::Update { chunks, .. } = hunks.into_iter().next().unwrap() else {
            panic!("expected update hunk");
        };
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_end_of_file);
        assert_eq!(chunks[0].old_lines, vec!["x".to_string()]);
        assert_eq!(
            chunks[0].new_lines,
            vec!["x".to_string(), "tail".to_string()]
        );
    }

    #[test]
    fn rejects_missing_markers() {
        assert!(parse_patch("no markers here").is_err());
        assert!(
            parse_patch("*** Begin Patch\n*** End Patch")
                .map(|h| h.is_empty())
                .unwrap_or(false)
        );
    }

    #[test]
    fn strips_heredoc_wrapper() {
        let wrapped = "cat <<'EOF'\n*** Begin Patch\n*** Add File: x.txt\n+hi\n*** End Patch\nEOF";
        let hunks = parse_patch(wrapped).unwrap();
        assert_eq!(
            hunks,
            vec![Hunk::Add {
                path: "x.txt".into(),
                contents: "hi".into(),
            }]
        );
    }

    #[test]
    fn matching_falls_back_to_trim() {
        let lines: Vec<String> = vec!["let a = 1;  ".into(), "let b = 2;".into()];
        let pattern: Vec<String> = vec!["let a = 1;".into(), "let b = 2; ".into()];
        assert_eq!(seek_sequence(&lines, &pattern, 0, false), Some(0));
    }

    fn tempdir() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = crate::test_util::test_util::lock_cwd();
        let dir = tempfile::TempDir::new().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();
        (dir, guard)
    }

    #[tokio::test]
    async fn applies_multi_file_patch() {
        let (_dir, _guard) = tempdir();
        std::fs::create_dir_all("src").unwrap();
        std::fs::write("src/app.rs", "fn old() {}\nfn main() {}\n").unwrap();
        std::fs::write("obsolete.txt", "gone\n").unwrap();

        let patch = r#"*** Begin Patch
*** Add File: new.txt
+created
*** Update File: src/app.rs
@@
-fn old() {}
+fn new() {}
*** Delete File: obsolete.txt
*** End Patch"#;
        let tool = ApplyPatch::new(None, FileLocks::new());
        let mut ctx = ToolContext::default();
        let out = tool
            .call(&mut ctx, json!({ "patchText": patch }))
            .await
            .unwrap();

        assert_eq!(std::fs::read_to_string("new.txt").unwrap(), "created\n");
        assert_eq!(
            std::fs::read_to_string("src/app.rs").unwrap(),
            "fn new() {}\nfn main() {}\n"
        );
        assert!(!std::path::Path::new("obsolete.txt").exists());

        let change = ctx.result::<FileChange>().unwrap();
        let FileChange::Patch { files } = change else {
            panic!("expected patch change");
        };
        assert_eq!(files.len(), 3);
        assert_eq!(files[0].kind, PatchFileKind::Add);
        assert_eq!(files[0].original, None);
        assert_eq!(files[1].kind, PatchFileKind::Update);
        assert_eq!(
            files[1].original.as_deref(),
            Some("fn old() {}\nfn main() {}\n")
        );
        assert_eq!(files[1].new.as_deref(), Some("fn new() {}\nfn main() {}\n"));
        assert_eq!(files[2].kind, PatchFileKind::Delete);
        assert!(files[2].new.is_none());
        assert!(out.as_text().expect("text output").contains("Success"));
    }

    #[tokio::test]
    async fn applies_move_patch() {
        let (_dir, _guard) = tempdir();
        std::fs::write("old.txt", "data\n").unwrap();

        let patch = r#"*** Begin Patch
*** Update File: old.txt
*** Move to: new/nested.txt
@@
-data
-moved
+data
+moved
*** End Patch"#;
        let tool = ApplyPatch::new(None, FileLocks::new());
        let mut ctx = ToolContext::default();
        let err = tool
            .call(&mut ctx, json!({ "patchText": patch }))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Failed to find expected lines"),
            "unexpected error: {err}"
        );

        let patch = r#"*** Begin Patch
*** Update File: old.txt
*** Move to: new/nested.txt
@@
-data
+data
+moved
*** End Patch"#;
        tool.call(&mut ctx, json!({ "patchText": patch }))
            .await
            .unwrap();
        assert!(!std::path::Path::new("old.txt").exists());
        assert_eq!(
            std::fs::read_to_string("new/nested.txt").unwrap(),
            "data\nmoved\n"
        );
        let FileChange::Patch { files } = ctx.result::<FileChange>().unwrap() else {
            panic!("expected patch change");
        };
        assert_eq!(files[0].path, "old.txt");
        assert_eq!(files[0].moved_to.as_deref(), Some("new/nested.txt"));
    }

    #[tokio::test]
    async fn rejects_mismatch_without_writing() {
        let (_dir, _guard) = tempdir();
        std::fs::write("a.txt", "one\ntwo\n").unwrap();

        let patch = r#"*** Begin Patch
*** Update File: a.txt
@@
-three
+3
*** End Patch"#;
        let tool = ApplyPatch::new(None, FileLocks::new());
        let mut ctx = ToolContext::default();
        let err = tool
            .call(&mut ctx, json!({ "patchText": patch }))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Failed to find expected lines"), "{err}");
        assert_eq!(std::fs::read_to_string("a.txt").unwrap(), "one\ntwo\n");
        assert!(ctx.result::<FileChange>().is_none());
    }

    #[test]
    fn derive_trims_and_appends_final_newline() {
        let original = "a\nb\nc";
        let chunks = vec![UpdateChunk {
            old_lines: vec!["b".into()],
            new_lines: vec!["B".into()],
            change_context: None,
            is_end_of_file: false,
        }];
        let new = derive_new_contents("f", &chunks, original).unwrap();
        assert_eq!(new, "a\nB\nc\n");
    }

    #[test]
    fn patch_files_expands_moves_for_undo_redo() {
        let change = FileChange::Patch {
            files: vec![
                PatchFileChange {
                    path: "old.txt".into(),
                    kind: PatchFileKind::Update,
                    moved_to: Some("new.txt".into()),
                    original: Some("orig".into()),
                    new: Some("updated".into()),
                    diff: vec![],
                },
                PatchFileChange {
                    path: "added.txt".into(),
                    kind: PatchFileKind::Add,
                    moved_to: None,
                    original: None,
                    new: Some("added".into()),
                    diff: vec![],
                },
                PatchFileChange {
                    path: "gone.txt".into(),
                    kind: PatchFileKind::Delete,
                    moved_to: None,
                    original: Some("gone".into()),
                    new: None,
                    diff: vec![],
                },
            ],
        };
        assert_eq!(
            change.patch_files(),
            vec![
                ("old.txt", Some("orig"), None),
                ("new.txt", None, Some("updated")),
                ("added.txt", None, Some("added")),
                ("gone.txt", Some("gone"), None),
            ]
        );
    }
}
