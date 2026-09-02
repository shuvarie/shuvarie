use std::time::Instant;

use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_core::todos::{TodoStatus, done_total, parse_items};
use shuvarie_llm::{DiffLine, DiffLineKind, FileChange, PatchFileKind};
use unicode_width::UnicodeWidthStr;

use super::format_duration_ms;
use crate::tui::session::blocks::{ChatEnv, Segment};
use crate::tui::session::segment::BLOCK_PADDING;
use crate::tui::session::virtualizer::{TurnEst, collapsed_rows, file_change_row_est};
use crate::tui::{spinner, theme};
use shuvarie_core::tool_record::ToolRecord;

const COLLAPSED_OUTPUT_LINES: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

pub enum ToolMessage {
    Output {
        stdout: String,
        stderr: String,
    },
    Finish {
        ok: bool,
        output: String,
        stderr: String,
        file_change: Option<FileChange>,
        duration_ms: u64,
    },
}

/// One agent tool call: status, human-readable header, collapsible output
/// rows, optional diff review, and LSP diagnostics. Owns its own collapse
/// state (`question` blocks default to expanded once finished).
pub struct ToolBlock {
    name: String,
    args: String,
    status: ToolStatus,
    output: String,
    stderr: String,
    expanded: bool,
    worker: Option<String>,
    file_change: Option<FileChange>,
    started_at: Option<Instant>,
    duration_ms: u64,
}

impl ToolBlock {
    pub fn new(name: impl Into<String>, args: String, worker: Option<String>) -> Self {
        Self {
            name: name.into(),
            args,
            status: ToolStatus::Running,
            output: String::new(),
            stderr: String::new(),
            expanded: false,
            worker,
            file_change: None,
            started_at: Some(Instant::now()),
            duration_ms: 0,
        }
    }

    pub fn from_record(record: &ToolRecord) -> Self {
        Self {
            name: record.name.clone(),
            args: record.args_json.clone(),
            status: if record.ok {
                ToolStatus::Ok
            } else {
                ToolStatus::Failed
            },
            output: record.output.clone(),
            stderr: record.stderr.clone(),
            expanded: false,
            worker: record.worker.clone(),
            file_change: record.file_change.clone(),
            started_at: None,
            duration_ms: record.duration_ms,
        }
    }

    pub fn matches(&self, name: &str, worker: &Option<String>) -> bool {
        self.name == name && self.worker == *worker
    }

    pub fn is_running(&self) -> bool {
        self.status == ToolStatus::Running
    }

    /// Flip the collapse state of the output rows.
    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub(super) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Estimated row counters mirroring [`Self::view`]: header, collapse state
    /// (which follows `expanded` — `question`/`todo` flip it on finish), file
    /// change rows, and the elapsed meta row. LSP diagnostics rows are
    /// environment-dependent and not estimated.
    pub(super) fn est(&self) -> TurnEst {
        let mut est = TurnEst {
            tool_count: 1,
            padding_rows: 2 * u32::from(BLOCK_PADDING.1),
            tool_header_width: (self.name.chars().count() + 1) as u32
                + UnicodeWidthStr::width(self.args.as_str()).min(120) as u32,
            ..TurnEst::default()
        };
        let is_shell = self.name == "run_shell";
        if self.name == "question" {
            if self.status == ToolStatus::Running {
                est.tool_rows += 1;
            } else if self.expanded {
                est.tool_rows += self.output.lines().count() as u32;
            }
        } else if self.name == "todo" {
            let rows = parse_items(&self.output).map_or(0, |items| items.len() as u32);
            est.tool_rows += if self.expanded {
                rows
            } else {
                collapsed_rows(rows)
            };
        } else {
            let (stdout_rows, stderr_rows) = if is_shell {
                (
                    self.output.lines().count() as u32,
                    self.stderr.lines().count() as u32,
                )
            } else {
                (self.output.lines().count() as u32, 0)
            };
            est.tool_rows += if self.expanded {
                stdout_rows
                    .saturating_add(stderr_rows)
                    .saturating_add(u32::from(is_shell && stdout_rows > 0 && stderr_rows > 0))
            } else if is_shell && stderr_rows > 0 {
                collapsed_rows(stderr_rows)
            } else {
                collapsed_rows(stdout_rows)
            };
        }
        if let Some(change) = &self.file_change {
            est.tool_rows += file_change_row_est(change);
        }
        if self.name != "question" {
            est.tool_rows += 1;
        }
        est
    }

    pub fn bg(&self) -> Color {
        match self.status {
            ToolStatus::Running => theme::RUNNING_BG,
            ToolStatus::Ok => theme::SUCCESS_BG,
            ToolStatus::Failed => theme::ERROR_BG,
        }
    }

    pub fn update(&mut self, msg: ToolMessage) -> bool {
        match msg {
            ToolMessage::Output { stdout, stderr } => {
                if self.status != ToolStatus::Running {
                    return false;
                }
                self.output = stdout;
                self.stderr = stderr;
                true
            }
            ToolMessage::Finish {
                ok,
                output,
                stderr,
                file_change,
                duration_ms,
            } => {
                if self.status != ToolStatus::Running {
                    return false;
                }
                self.status = if ok {
                    ToolStatus::Ok
                } else {
                    ToolStatus::Failed
                };
                self.output = output;
                self.stderr = stderr;
                self.file_change = file_change;
                self.started_at = None;
                self.duration_ms = duration_ms;
                self.expanded = self.name == "question" || self.name == "todo";
                true
            }
        }
    }

    pub(super) fn view(&self, width: u16, env: &ChatEnv) -> Segment {
        Segment {
            lines: self.block_lines(width, env),
            bg: Some(self.bg()),
            padding: BLOCK_PADDING,
            hit: None,
        }
    }

    fn block_lines(&self, width: u16, env: &ChatEnv) -> Vec<Line<'static>> {
        let inner_w = width.saturating_sub(2 * BLOCK_PADDING.0).max(8) as usize;
        let is_shell = self.name == "run_shell";
        let mut lines: Vec<Line<'static>> = Vec::new();
        lines.push(self.header_line(inner_w));
        if self.name == "question" {
            push_question_block_lines(&mut lines, self);
        } else if self.name == "todo" {
            match parse_items(&self.output) {
                Some(items) => push_todo_rows(&mut lines, &items, self.expanded),
                None => push_output_rows(&mut lines, self, false),
            }
        } else {
            let before = lines.len();
            push_output_rows(&mut lines, self, is_shell);
            let has_output = lines.len() > before;
            if let Some(change) = &self.file_change {
                if has_output {
                    lines.push(Line::from(""));
                }
                push_file_change_lines(&mut lines, change);
                let path = match change {
                    FileChange::Edit { path, .. } | FileChange::Write { path, .. } => {
                        Some(path.as_str())
                    }
                    FileChange::Patch { files, .. } => {
                        (files.len() == 1).then(|| files[0].path.as_str())
                    }
                };
                if let Some(path) = path {
                    push_diagnostics_lines(&mut lines, env, path);
                }
            }
        }
        if self.name != "question" {
            lines.push(elapsed_line(self));
        }
        lines
    }

    fn header_line(&self, inner_w: usize) -> Line<'static> {
        let is_worker_call = self.worker.as_deref() == Some("");
        let mut header: Vec<Span<'static>> = Vec::new();
        match self.status {
            ToolStatus::Running => header.push(spinner::spinner()),
            ToolStatus::Ok => header.push(
                Span::raw(if is_worker_call { "❖" } else { "✓" })
                    .fg(if is_worker_call {
                        theme::ACCENT
                    } else {
                        theme::SUCCESS
                    })
                    .bold(),
            ),
            ToolStatus::Failed => header.push(
                Span::raw(if is_worker_call { "❖" } else { "✗" })
                    .fg(if is_worker_call {
                        theme::ACCENT
                    } else {
                        theme::ERROR
                    })
                    .bold(),
            ),
        }
        header.push(Span::raw(" "));

        let args: Value = serde_json::from_str(&self.args).unwrap_or(Value::Null);

        if is_worker_call {
            header.push(Span::raw(self.name.clone()).fg(theme::TEXT).bold());
            let task = args.get("task").and_then(Value::as_str).unwrap_or("");
            if !task.is_empty() {
                let avail = inner_w.saturating_sub(spans_width(&header) + 1);
                let preview: String = task
                    .chars()
                    .take(avail)
                    .map(|c| if c == '\n' { ' ' } else { c })
                    .collect();
                if !preview.is_empty() {
                    header.push(Span::raw(format!(" {preview}")).fg(theme::TEXT_MUTED));
                }
            }
            return Line::from(header);
        }

        match self.name.as_str() {
            "run_shell" => {
                let cmd = args.get("command").and_then(Value::as_str).unwrap_or("");
                let timeout = args.get("timeout_secs").and_then(Value::as_u64);
                let (status_label, _) = split_status_line(&self.output);
                let mut right: Vec<String> = Vec::new();
                if let Some(t) = timeout {
                    right.push(format!("timeout={t}s"));
                }
                if let Some(label) = status_label
                    && label != "exit 0"
                {
                    right.push(label);
                }
                let right_text = right.join(" · ");
                let right_w = UnicodeWidthStr::width(right_text.as_str());
                header.push(Span::raw("$ ".to_string() + cmd).fg(theme::TEXT).bold());
                if !right_text.is_empty() {
                    let pad = inner_w
                        .saturating_sub(spans_width(&header) + right_w)
                        .max(1);
                    header.push(Span::raw(" ".repeat(pad)));
                    header.push(Span::raw(right_text).fg(theme::TEXT_MUTED));
                }
            }
            "read_file" => {
                let path = args.get("path").and_then(Value::as_str).unwrap_or("");
                let range = (self.status == ToolStatus::Ok)
                    .then(|| read_file_range(&args, &self.output))
                    .flatten();
                let avail = inner_w
                    .saturating_sub(self.name.chars().count() + 1)
                    .saturating_sub(20);
                let path_display: String = path.chars().take(avail).collect();
                header.push(Span::raw(self.name.clone()).fg(theme::TEXT).bold());
                header.push(
                    Span::raw(format!(" {path_display}"))
                        .fg(theme::ACCENT)
                        .bold(),
                );
                if let Some((start, end)) = range {
                    header.push(Span::raw(format!(" · lines {start}–{end}")).fg(theme::TEXT_MUTED));
                }
            }
            "question" => {
                header.push(Span::raw("question").fg(theme::TEXT).bold());
            }
            "todo" => {
                header.push(Span::raw("todo").fg(theme::TEXT).bold());
                if let Some(summary) = todo_op_summary(&args) {
                    header.push(Span::raw(format!(" {summary}")).fg(theme::TEXT_MUTED));
                }
                if let Some(items) = parse_items(&self.output) {
                    let (done, total) = done_total(&items);
                    let right_text = format!("{done}/{total} done");
                    let right_w = UnicodeWidthStr::width(right_text.as_str());
                    let pad = inner_w
                        .saturating_sub(spans_width(&header) + right_w)
                        .max(1);
                    header.push(Span::raw(" ".repeat(pad)));
                    header.push(Span::raw(right_text).fg(theme::TEXT_MUTED));
                }
            }
            _ => {
                header.push(Span::raw(self.name.clone()).fg(theme::TEXT).bold());
                if !self.args.is_empty() {
                    let avail = inner_w.saturating_sub(spans_width(&header) + 1);
                    let args_display: String = self
                        .args
                        .chars()
                        .take(avail)
                        .map(|c| if c == '\n' { ' ' } else { c })
                        .collect();
                    if !args_display.is_empty() {
                        header.push(Span::raw(format!(" {args_display}")).fg(theme::TEXT_MUTED));
                    }
                }
            }
        }
        Line::from(header)
    }
}

fn push_output_rows(lines: &mut Vec<Line<'static>>, tool: &ToolBlock, is_shell: bool) {
    let (stdout_body, stderr_body): (String, String) = if is_shell {
        let (_, body) = split_status_line(&tool.output);
        (body.to_string(), tool.stderr.clone())
    } else {
        (tool.output.clone(), String::new())
    };
    let stdout_rows: Vec<&str> = stdout_body.lines().collect();
    let stderr_rows: Vec<&str> = stderr_body.lines().collect();

    if tool.expanded {
        for row in &stdout_rows {
            lines.push(Line::from(
                Span::raw((*row).to_string()).fg(theme::TEXT_DIM),
            ));
        }
        if !stderr_rows.is_empty() {
            if !stdout_rows.is_empty() {
                lines.push(Line::from(""));
            }
            for row in &stderr_rows {
                lines.push(Line::from(Span::raw((*row).to_string()).fg(theme::ERROR)));
            }
        }
        return;
    }

    let (rows, fg) = if is_shell && !stderr_rows.is_empty() {
        (stderr_rows.as_slice(), theme::ERROR)
    } else {
        (stdout_rows.as_slice(), theme::TEXT_DIM)
    };
    let hidden = rows.len().saturating_sub(COLLAPSED_OUTPUT_LINES);
    for row in &rows[hidden..] {
        lines.push(Line::from(Span::raw((*row).to_string()).fg(fg)));
    }
    if hidden > 0 {
        lines.push(Line::from(
            Span::raw(format!("… +{hidden} more lines"))
                .fg(theme::TEXT_MUTED)
                .italic(),
        ));
    }
}

/// A short human summary of the todo operation for the block header, parsed
/// from the call args: `+ "text"`, `#2 → done`, `− #1`.
fn todo_op_summary(args: &Value) -> Option<String> {
    let truncate = |raw: &str| -> String { raw.chars().take(40).collect() };
    match args.get("op").and_then(Value::as_str)? {
        "add" => {
            let text = args.get("text").and_then(Value::as_str)?;
            Some(format!("+ \"{}\"", truncate(text)))
        }
        "update" => {
            let id = args.get("id").and_then(Value::as_u64)?;
            let text = args.get("text").and_then(Value::as_str);
            let status = args.get("status").and_then(Value::as_str);
            match (text, status) {
                (Some(t), Some(s)) => Some(format!("#{id} → \"{}\" → {s}", truncate(t))),
                (Some(t), None) => Some(format!("#{id} → \"{}\"", truncate(t))),
                (None, Some(s)) => Some(format!("#{id} → {s}")),
                (None, None) => Some(format!("#{id}")),
            }
        }
        "remove" => {
            let id = args.get("id").and_then(Value::as_u64)?;
            Some(format!("− #{id}"))
        }
        _ => None,
    }
}

/// The styled todo list rows: done items dimmed + struck through, the single
/// in-progress item highlighted, pending items dim. Collapsed like tool
/// output (last few rows + a hint), expanded shows everything.
fn push_todo_rows(
    lines: &mut Vec<Line<'static>>,
    items: &[shuvarie_core::todos::TodoItem],
    expanded: bool,
) {
    let hidden = if expanded {
        0
    } else {
        items.len().saturating_sub(COLLAPSED_OUTPUT_LINES)
    };
    for item in &items[hidden..] {
        let (marker, marker_fg, text_style) = match item.status {
            TodoStatus::Done => (
                "✓",
                theme::SUCCESS,
                Style::new()
                    .fg(theme::TEXT_DIM)
                    .add_modifier(Modifier::CROSSED_OUT),
            ),
            TodoStatus::InProgress => ("◐", theme::ACCENT, Style::new().fg(theme::TEXT).bold()),
            TodoStatus::Pending => ("○", theme::TEXT_MUTED, Style::new().fg(theme::TEXT_DIM)),
        };
        lines.push(Line::from(vec![
            Span::raw("  ").fg(theme::TEXT_MUTED),
            Span::raw(marker).fg(marker_fg).bold(),
            Span::raw(" ").fg(theme::TEXT_MUTED),
            Span::raw(item.text.clone()).style(text_style),
        ]));
    }
    if hidden > 0 {
        lines.push(Line::from(
            Span::raw(format!("… +{hidden} more items"))
                .fg(theme::TEXT_MUTED)
                .italic(),
        ));
    }
}

/// The bottom meta row of a tool block: `Elapsed 4.5s` while the call is
/// running (recomputed every frame from the wall clock), `Took 10.3s` once it
/// finishes — the finished value comes from the core-measured duration.
fn elapsed_line(tool: &ToolBlock) -> Line<'static> {
    if tool.status == ToolStatus::Running {
        let ms = tool
            .started_at
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or_default();
        Line::from(vec![
            Span::raw("  Elapsed ").fg(theme::TEXT_MUTED).italic(),
            Span::raw(format_duration_ms(ms)).fg(theme::ACCENT).italic(),
        ])
    } else {
        Line::from(vec![
            Span::raw("  Took ").fg(theme::TEXT_MUTED).italic(),
            Span::raw(format_duration_ms(tool.duration_ms))
                .fg(theme::TEXT_MUTED)
                .italic(),
        ])
    }
}

fn push_question_block_lines(lines: &mut Vec<Line<'static>>, tool: &ToolBlock) {
    if tool.status == ToolStatus::Running {
        lines.push(Line::from(
            Span::raw("  Asking...").fg(theme::TEXT_MUTED).italic(),
        ));
        return;
    }
    if !tool.expanded {
        return;
    }
    let questions = tool
        .args
        .is_empty()
        .then(Vec::new)
        .unwrap_or_else(|| parse_question_prompts(&tool.args));
    if tool.status == ToolStatus::Failed || questions.is_empty() {
        let first: String = tool
            .output
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        let note = if first.is_empty() {
            "dismissed".to_string()
        } else {
            first
        };
        lines.push(Line::from(
            Span::raw(format!("  {note}")).fg(theme::TEXT_DIM).italic(),
        ));
        return;
    }
    for q in &questions {
        let answer = answer_for_question(&tool.output, &q.question);
        lines.push(Line::from(vec![
            Span::raw("  ? ").fg(theme::ACCENT),
            Span::raw(q.question.clone()).fg(theme::TEXT),
        ]));
        let (marker, fg) = if answer.is_empty() {
            ("  ⚠ ", theme::WARNING)
        } else {
            ("  ⇒ ", theme::SUCCESS)
        };
        let shown = if answer.is_empty() {
            "unanswered".to_string()
        } else {
            answer
        };
        lines.push(Line::from(Span::raw(format!("{marker}{shown}")).fg(fg)));
    }
}

fn push_diagnostics_lines(lines: &mut Vec<Line<'static>>, env: &ChatEnv, path: &str) {
    let Some(diags) = env.lsp_diagnostics.get(path) else {
        return;
    };
    if diags.is_empty() {
        return;
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  ── diagnostics: ").fg(theme::TEXT_MUTED),
        Span::raw(path.to_string()).fg(theme::ACCENT),
    ]));
    for d in diags {
        let (sev_label, sev_color) = match d.severity {
            shuvarie_core::DiagnosticSeverity::Error => ("error", theme::ERROR),
            shuvarie_core::DiagnosticSeverity::Warning => ("warning", theme::WARNING),
            shuvarie_core::DiagnosticSeverity::Information => ("info", theme::ACCENT),
            shuvarie_core::DiagnosticSeverity::Hint => ("hint", theme::TEXT_DIM),
        };
        let loc = format!("{}:{}", d.line, d.col);
        let msg = d.message.chars().take(140).collect::<String>();
        lines.push(Line::from(vec![
            Span::raw(format!("    {loc:<10} ")).fg(theme::TEXT_MUTED),
            Span::raw(format!("{sev_label:<8} ")).fg(sev_color),
            Span::raw(msg).fg(theme::TEXT_DIM),
        ]));
    }
}

fn push_file_change_lines(lines: &mut Vec<Line<'static>>, change: &FileChange) {
    match change {
        FileChange::Edit { path, diff, .. } => {
            lines.push(Line::from(vec![
                Span::raw("  ── diff: ").fg(theme::TEXT_MUTED),
                Span::raw(path.clone()).fg(theme::ACCENT),
            ]));
            for line in diff {
                push_diff_line(lines, line);
            }
        }
        FileChange::Write { path, content, .. } => {
            lines.push(Line::from(vec![
                Span::raw("  ── new file: ").fg(theme::TEXT_MUTED),
                Span::raw(path.clone()).fg(theme::ACCENT),
            ]));
            for (i, text) in content.lines().enumerate() {
                let num = format!("{:>4}", i + 1);
                lines.push(Line::from(vec![
                    Span::raw(format!("    {num} ")).fg(theme::TEXT_MUTED),
                    Span::raw(text.to_string()).fg(theme::TEXT_DIM),
                ]));
            }
        }
        FileChange::Patch { files, .. } => {
            for file in files {
                match (&file.kind, &file.moved_to) {
                    (PatchFileKind::Delete, _) => {
                        lines.push(Line::from(vec![
                            Span::raw("  ── deleted: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ERROR),
                        ]));
                    }
                    (PatchFileKind::Add, _) => {
                        lines.push(Line::from(vec![
                            Span::raw("  ── new file: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                        ]));
                        for (i, text) in file.new.as_deref().unwrap_or_default().lines().enumerate()
                        {
                            let num = format!("{:>4}", i + 1);
                            lines.push(Line::from(vec![
                                Span::raw(format!("    {num} ")).fg(theme::TEXT_MUTED),
                                Span::raw(text.to_string()).fg(theme::TEXT_DIM),
                            ]));
                        }
                    }
                    (PatchFileKind::Update, Some(target)) => {
                        lines.push(Line::from(vec![
                            Span::raw("  ── moved: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                            Span::raw(" → ").fg(theme::TEXT_MUTED),
                            Span::raw(target.clone()).fg(theme::ACCENT),
                        ]));
                        for line in &file.diff {
                            push_diff_line(lines, line);
                        }
                    }
                    (PatchFileKind::Update, None) => {
                        lines.push(Line::from(vec![
                            Span::raw("  ── diff: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                        ]));
                        for line in &file.diff {
                            push_diff_line(lines, line);
                        }
                    }
                }
            }
        }
    }
}

fn push_diff_line(lines: &mut Vec<Line<'static>>, line: &DiffLine) {
    if line.kind == DiffLineKind::Ellipsis {
        lines.push(Line::from(Span::raw("    …").fg(theme::TEXT_MUTED)));
        return;
    }
    let (marker, fg) = match line.kind {
        DiffLineKind::Add => ("+", theme::SUCCESS),
        DiffLineKind::Remove => ("-", theme::ERROR),
        DiffLineKind::Context => (" ", theme::TEXT_DIM),
        DiffLineKind::Ellipsis => unreachable!(),
    };
    let old_num = line
        .old_line
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_string());
    let new_num = line
        .new_line
        .map(|n| format!("{n:>4}"))
        .unwrap_or_else(|| "    ".to_string());
    let text = line.text.trim_end_matches('\n');
    lines.push(Line::from(vec![
        Span::raw(format!("  {old_num} {new_num} ")).fg(theme::TEXT_MUTED),
        Span::raw(marker).fg(fg).bold(),
        Span::raw(text.to_string()).fg(fg),
    ]));
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum()
}

/// Parse the `question` tool's args JSON into question prompts (defensive:
/// returns empty when the args are malformed).
fn parse_question_prompts(args_json: &str) -> Vec<shuvarie_core::QuestionPrompt> {
    serde_json::from_str::<serde_json::Value>(args_json)
        .ok()
        .and_then(|v| serde_json::from_value(v.get("questions").cloned()?).ok())
        .unwrap_or_default()
}

/// Extract the answer recorded in the tool output for a question whose text
/// starts with `question`. Output format: `... "q"="a, b", "q2"="c"`.
fn answer_for_question(output: &str, question: &str) -> String {
    let pairs = match output.split_once(": ") {
        Some((_, rest)) => rest,
        None => return String::new(),
    };
    for pair in pairs.split("\", \"") {
        let pair = pair.trim_start_matches('"');
        if let Some((q, a)) = pair.split_once("\"=\"")
            && q == question
        {
            return a.trim_end_matches('"').to_string();
        }
    }
    String::new()
}

/// Split a leading shell status line (`exit N:`, `timeout Ns:` or the legacy
/// `shell exited with ...:`) off the persisted stdout text, returning the
/// normalized label and the remaining body.
fn split_status_line(text: &str) -> (Option<String>, &str) {
    let (first, rest) = match text.split_once('\n') {
        Some((first, rest)) => (first, rest),
        None => (text, ""),
    };
    let label = if let Some(n) = first
        .strip_prefix("exit ")
        .and_then(|s| s.strip_suffix(':'))
        && !n.is_empty()
        && n.chars().all(|c| c.is_ascii_digit())
    {
        Some(format!("exit {n}"))
    } else if let Some(s) = first
        .strip_prefix("timeout ")
        .and_then(|s| s.strip_suffix(':'))
    {
        s.ends_with('s').then(|| format!("timeout {s}"))
    } else {
        first
            .strip_prefix("shell exited with ")
            .and_then(|s| s.strip_suffix(':'))
            .map(|s| s.to_string())
    };
    (label, rest)
}

/// The line range a `read_file` call showed: exact from the tool's footer
/// markers when present, otherwise derived from the args and output length.
fn read_file_range(args: &Value, output: &str) -> Option<(u64, u64)> {
    let start = args
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1);
    if let Some(pos) = output.find("(Showing lines ") {
        let rest = &output[pos + "(Showing lines ".len()..];
        if let Some(end) = rest.find(" of ")
            && let Some((s, e)) = rest[..end].split_once('-')
            && let (Ok(s), Ok(e)) = (s.trim().parse::<u64>(), e.trim().parse::<u64>())
        {
            return Some((s, e));
        }
    }
    if let Some(pos) = output.find("(End of file - total ") {
        let rest = &output[pos + "(End of file - total ".len()..];
        if let Some((n, _)) = rest.split_once(' ')
            && let Ok(total) = n.parse::<u64>()
        {
            return Some((start, total.max(start)));
        }
    }
    let count = output
        .lines()
        .filter(|l| !l.starts_with('(') && !l.starts_with("use "))
        .count() as u64;
    (count > 0).then_some((start, start + count - 1))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn block_text(block: &ToolBlock, env: &ChatEnv) -> String {
        block
            .block_lines(80, env)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.clone())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn running_block_shows_live_elapsed() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.to_string(), None);
        let text = block_text(&block, &env);
        assert!(text.contains("Elapsed "), "header/body: {text}");
        assert!(!text.contains("Took "));
    }

    #[test]
    fn finished_block_shows_took_with_core_duration() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.to_string(), None);
        block.update(ToolMessage::Finish {
            ok: true,
            output: "done".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 10_300,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("Took 10.3s"), "header/body: {text}");
        assert!(!text.contains("Elapsed "));
    }

    #[test]
    fn reloaded_block_shows_persisted_duration() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::from_record(&ToolRecord {
            name: "grep".to_string(),
            args_json: "{}".to_string(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            worker: None,
            message_id: 1,
            message_seq: 1,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 65_400,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("Took 1m 05s"), "header/body: {text}");
    }

    fn todo_output() -> String {
        "Added #3 \"update UI\"\n\nTodos (1/3 done)\n  #1 [x] set up schema\n  #2 [~] write migration\n  #3 [ ] update UI".to_string()
    }

    #[test]
    fn finished_todo_block_renders_status_list() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "todo",
            r#"{"op":"add","text":"update UI"}"#.to_string(),
            None,
        );
        block.update(ToolMessage::Finish {
            ok: true,
            output: todo_output(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 120,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("todo + \"update UI\""), "header/body: {text}");
        assert!(text.contains("1/3 done"), "header/body: {text}");
        assert!(text.contains("✓ set up schema"), "header/body: {text}");
        assert!(text.contains("◐ write migration"), "header/body: {text}");
        assert!(text.contains("○ update UI"), "header/body: {text}");
        assert!(
            !text.contains("Todos (1/3 done)"),
            "raw output rows leaked: {text}"
        );
        assert!(!text.contains("Added #3"), "model summary leaked: {text}");
    }

    #[test]
    fn reloaded_todo_block_renders_from_persisted_output() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::from_record(&ToolRecord {
            name: "todo".to_string(),
            args_json: r#"{"op":"list"}"#.to_string(),
            output: "Todos (1/3 done)\n  #1 [x] set up schema\n  #2 [~] write migration\n  #3 [ ] update UI".to_string(),
            stderr: String::new(),
            ok: true,
            worker: None,
            message_id: 1,
            message_seq: 1,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 90,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("✓ set up schema"), "header/body: {text}");
        assert!(text.contains("◐ write migration"), "header/body: {text}");
        assert!(text.contains("○ update UI"), "header/body: {text}");
        assert!(text.contains("1/3 done"), "header/body: {text}");
    }

    #[test]
    fn failed_todo_call_falls_back_to_output_rows() {
        let env = ChatEnv {
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "todo",
            r#"{"op":"update","id":9,"status":"done"}"#.to_string(),
            None,
        );
        block.update(ToolMessage::Finish {
            ok: false,
            output: "unknown todo id 9".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("unknown todo id 9"), "header/body: {text}");
    }
}
