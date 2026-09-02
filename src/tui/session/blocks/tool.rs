use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_llm::{DiffLine, DiffLineKind, FileChange, PatchFileKind};
use unicode_width::UnicodeWidthStr;

use crate::tui::session::blocks::{ChatEnv, Segment};
use crate::tui::session::segment::BLOCK_PADDING;
use crate::tui::{spinner, theme};

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
        }
    }

    pub fn from_record(
        name: String,
        args: String,
        output: String,
        stderr: String,
        ok: bool,
        worker: Option<String>,
        file_change: Option<FileChange>,
    ) -> Self {
        Self {
            name,
            args,
            status: if ok {
                ToolStatus::Ok
            } else {
                ToolStatus::Failed
            },
            output,
            stderr,
            expanded: false,
            worker,
            file_change,
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
                self.expanded = self.name == "question";
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
            return lines;
        }
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
                    right.push(format!("{t}s"));
                }
                if let Some(label) = status_label
                    && label != "exit 0"
                {
                    right.push(label);
                }
                let right_text = right.join(" · ");
                let right_w = UnicodeWidthStr::width(right_text.as_str());
                let avail = inner_w.saturating_sub(right_w + 2);
                let cmd_display: String = cmd
                    .chars()
                    .take(avail)
                    .map(|c| if c == '\n' { ' ' } else { c })
                    .collect();
                header.push(Span::raw(cmd_display).fg(theme::TEXT).bold());
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
                let path_display: String = path.chars().take(inner_w.saturating_sub(20)).collect();
                header.push(Span::raw(path_display).fg(theme::ACCENT).bold());
                if let Some((start, end)) = range {
                    header.push(Span::raw(format!(" · lines {start}–{end}")).fg(theme::TEXT_MUTED));
                }
            }
            "question" => {
                header.push(Span::raw("question").fg(theme::TEXT).bold());
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

fn push_question_block_lines(lines: &mut Vec<Line<'static>>, tool: &ToolBlock) {
    if tool.status == ToolStatus::Running {
        lines.push(Line::from(
            Span::raw("  asking…").fg(theme::TEXT_MUTED).italic(),
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
