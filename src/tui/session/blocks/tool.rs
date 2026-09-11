use std::cell::RefCell;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use ratatui::prelude::*;
use serde_json::Value;
use shuvarie_core::tools::todos::{TodoStatus, done_total, parse_items};
use shuvarie_llm::{FileChange, PatchFileKind};
use unicode_width::UnicodeWidthStr;

use super::{format_duration_ms, hides_output_when_collapsed, shows_elapsed};
use crate::tui::session::blocks::{ChatEnv, Segment};
use crate::tui::session::segment::{BLOCK_PADDING, BodyChunk, BodySource, TextRows};
use crate::tui::session::virtualizer::{TurnEst, collapsed_rows, file_change_row_est};
use crate::tui::{spinner, theme};
use shuvarie_core::tool_record::ToolRecord;

const COLLAPSED_OUTPUT_LINES: usize = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
    /// The call never finished: killed by the shell timeout or cut off when
    /// its turn was interrupted. Shown as a stop, not a failure — the tool
    /// did not get the chance to succeed or fail.
    Killed,
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
    /// The turn was cut while this call was still running: stop the spinner
    /// and mark the block killed, keeping whatever output streamed so far.
    Kill,
}

/// One agent tool call: status, human-readable header, collapsible output
/// rows, optional diff review, and LSP diagnostics. Owns its own collapse
/// state (`question` blocks default to expanded once finished). `call_id` is
/// the provider's per-call id (empty for persisted records) — finishes match
/// their block by it when a model batches several calls of one tool.
pub struct ToolBlock {
    name: String,
    args: String,
    parsed_args: OnceLock<Value>,
    call_id: String,
    status: ToolStatus,
    output: String,
    stderr: String,
    expanded: bool,
    worker: Option<String>,
    file_change: Option<FileChange>,
    started_at: Option<Instant>,
    duration_ms: u64,
    /// Bumped whenever output, stderr, status, or expansion change: the key
    /// for the cached body chunks and estimate.
    body_rev: u64,
    /// Body chunks (everything between the header and the elapsed row) keyed
    /// by `(width, env_rev, body_rev)`: the header and the elapsed row change
    /// per spinner frame, the body only on content/expansion/diagnostic
    /// changes.
    body: RefCell<Option<CachedBody>>,
    est_cache: RefCell<Option<(u64, TurnEst)>>,
}

#[derive(Clone)]
struct CachedBody {
    width: u16,
    env_rev: u64,
    body_rev: u64,
    chunks: Rc<[BodyChunk]>,
}

impl ToolBlock {
    pub fn new(
        name: impl Into<String>,
        args: String,
        worker: Option<String>,
        call_id: Option<String>,
    ) -> Self {
        Self {
            name: name.into(),
            args,
            parsed_args: OnceLock::new(),
            call_id: call_id.unwrap_or_default(),
            status: ToolStatus::Running,
            output: String::new(),
            stderr: String::new(),
            expanded: false,
            worker,
            file_change: None,
            started_at: Some(Instant::now()),
            duration_ms: 0,
            body_rev: 0,
            body: RefCell::new(None),
            est_cache: RefCell::new(None),
        }
    }

    pub fn from_record(record: &ToolRecord) -> Self {
        Self {
            name: record.name.clone(),
            args: record.args_json.clone(),
            parsed_args: OnceLock::new(),
            call_id: String::new(),
            status: if record.killed {
                ToolStatus::Killed
            } else {
                finish_status(record.ok, &record.name, &record.output)
            },
            output: record.output.clone(),
            stderr: record.stderr.clone(),
            expanded: false,
            worker: record.worker.clone(),
            file_change: record.file_change.clone(),
            started_at: None,
            duration_ms: record.duration_ms,
            body_rev: 0,
            body: RefCell::new(None),
            est_cache: RefCell::new(None),
        }
    }

    /// The call args parsed once: `args` never changes after construction.
    fn args_value(&self) -> &Value {
        self.parsed_args
            .get_or_init(|| serde_json::from_str(&self.args).unwrap_or(Value::Null))
    }

    pub fn matches(&self, name: &str, worker: &Option<String>) -> bool {
        self.name == name && self.worker == *worker
    }

    /// The provider call id this block was started with, if any.
    pub fn call_id(&self) -> Option<&str> {
        (!self.call_id.is_empty()).then_some(self.call_id.as_str())
    }

    pub fn is_running(&self) -> bool {
        self.status == ToolStatus::Running
    }

    /// Flip the collapse state of the output rows.
    pub(super) fn toggle(&mut self) {
        self.expanded = !self.expanded;
        self.body_rev += 1;
    }

    pub(super) fn set_expanded(&mut self, expanded: bool) {
        if self.expanded != expanded {
            self.expanded = expanded;
            self.body_rev += 1;
        }
    }

    pub(super) fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Estimated row counters mirroring [`Self::view`]: header, collapse state
    /// (which follows `expanded` — `question`/`todo` flip it on finish), file
    /// change rows, and the elapsed meta row for `shows_elapsed` tools. LSP
    /// diagnostics rows are environment-dependent and not estimated. Cached
    /// per `body_rev` — streaming output updates are the only frequent bumps.
    pub(super) fn est(&self) -> TurnEst {
        if let Some((rev, est)) = self.est_cache.borrow().as_ref()
            && *rev == self.body_rev
        {
            return *est;
        }
        let est = self.compute_est();
        *self.est_cache.borrow_mut() = Some((self.body_rev, est));
        est
    }

    fn compute_est(&self) -> TurnEst {
        let mut est = TurnEst {
            tool_count: 1,
            padding_rows: 2 * u32::from(BLOCK_PADDING.1),
            tool_header_width: (self.name.chars().count() + 1) as u32
                + UnicodeWidthStr::width(self.args.as_str()).min(120) as u32,
            tool_rows: u32::from(shows_elapsed(&self.name)),
            ..TurnEst::default()
        };
        let output_rows = self.output.lines().count() as u32;

        match self.name.as_str() {
            "question" => {
                if self.expanded {
                    est.tool_rows += output_rows;
                }
            }
            "todo" => {
                let rows = parse_items(&self.output).map_or(0, |items| items.len() as u32);
                est.tool_rows += if self.expanded {
                    rows
                } else {
                    collapsed_rows(rows)
                };
            }
            "run_shell" => {
                let stderr_rows = self.stderr.lines().count() as u32;

                if self.expanded {
                    est.tool_rows += output_rows
                        .saturating_add(stderr_rows)
                        .saturating_add(u32::from(output_rows > 0 && stderr_rows > 0));
                } else if stderr_rows > 0 {
                    est.tool_rows += collapsed_rows(stderr_rows);
                } else {
                    est.tool_rows += collapsed_rows(output_rows);
                }
            }
            _ if self.expanded => {
                est.tool_rows += output_rows;
            }
            _ => {
                let hidden =
                    self.status == ToolStatus::Ok && hides_output_when_collapsed(&self.name);
                if !hidden {
                    est.tool_rows += collapsed_rows(output_rows);
                }
            }
        }
        if let Some(change) = &self.file_change {
            est.tool_rows += file_change_row_est(change);
        }
        est
    }

    pub fn bg(&self) -> Color {
        match self.status {
            ToolStatus::Running => theme::RUNNING_BG,
            ToolStatus::Ok => theme::SUCCESS_BG,
            ToolStatus::Killed => theme::WARNING_BG,
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
                self.body_rev += 1;
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
                self.status = finish_status(ok, &self.name, &output);
                self.output = output;
                self.stderr = stderr;
                self.file_change = file_change;
                self.started_at = None;
                self.duration_ms = duration_ms;
                self.expanded = self.status != ToolStatus::Killed
                    && (self.name == "question" || self.name == "todo");
                self.body_rev += 1;
                true
            }
            ToolMessage::Kill => {
                if self.status != ToolStatus::Running {
                    return false;
                }
                self.status = ToolStatus::Killed;
                self.duration_ms = self
                    .started_at
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64)
                    .unwrap_or(self.duration_ms);
                self.body_rev += 1;
                true
            }
        }
    }

    pub(super) fn view(&self, width: u16, env: &ChatEnv) -> Segment {
        let body = self.cached_body(width, env);
        let inner_w = width.saturating_sub(2 * BLOCK_PADDING.0).max(8) as usize;
        let mut chunks = Vec::with_capacity(body.len() + 2);
        chunks.push(BodyChunk::fixed(vec![self.header_line(inner_w)]));
        chunks.extend(body.iter().cloned());
        if shows_elapsed(&self.name) {
            chunks.push(BodyChunk::fixed(vec![elapsed_line(self)]));
        }
        Segment {
            chunks,
            bg: Some(self.bg()),
            padding: BLOCK_PADDING,
            hit: None,
            trim: false,
        }
    }

    /// The body chunks between the header and the elapsed row, keyed by
    /// `(width, env_rev, body_rev)` so spinner frames reuse them unchanged.
    fn cached_body(&self, width: u16, env: &ChatEnv) -> Rc<[BodyChunk]> {
        if let Some(cached) = self.body.borrow().as_ref()
            && cached.width == width
            && cached.env_rev == env.rev
            && cached.body_rev == self.body_rev
        {
            return cached.chunks.clone();
        }
        let chunks: Rc<[BodyChunk]> = self.build_body(width, env).into();
        *self.body.borrow_mut() = Some(CachedBody {
            width,
            env_rev: env.rev,
            body_rev: self.body_rev,
            chunks: chunks.clone(),
        });
        chunks
    }

    fn build_body(&self, width: u16, env: &ChatEnv) -> Vec<BodyChunk> {
        let count_width = width.saturating_sub(2 * BLOCK_PADDING.0).max(1);
        let is_shell = self.name == "run_shell";
        let mut body = BodyBuilder::new(count_width);
        if self.name == "question" {
            push_question_block_lines(&mut body, self);
        } else if self.name == "todo" {
            match parse_items(&self.output) {
                Some(items) => push_todo_rows(&mut body, &items, self.expanded),
                None => {
                    push_output_rows(self, &mut body, false);
                }
            }
        } else {
            let has_output = push_output_rows(self, &mut body, is_shell);
            if let Some(change) = &self.file_change {
                if has_output {
                    body.fixed(Line::from(""));
                }
                push_file_change_rows(&mut body, change);
                let path = match change {
                    FileChange::Edit { path, .. } | FileChange::Write { path, .. } => {
                        Some(path.as_str())
                    }
                    FileChange::Patch { files, .. } => {
                        (files.len() == 1).then(|| files[0].path.as_str())
                    }
                };
                if let Some(path) = path {
                    push_diagnostics_lines(&mut body, env, path);
                }
            }
        }
        body.chunks
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
            ToolStatus::Killed => header.push(
                Span::raw(if is_worker_call { "❖" } else { "⏹" })
                    .fg(theme::WARNING)
                    .bold(),
            ),
        }
        header.push(Span::raw(" "));

        let args = self.args_value();

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
                match status_label {
                    Some(label) if label != "exit 0" => right.push(label),
                    None if self.status == ToolStatus::Killed => right.push("killed".into()),
                    _ => {}
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
                    .then(|| read_file_range(args, &self.output))
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
                if let Some(summary) = todo_op_summary(args) {
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

/// Accumulates a tool body as chunks: structural lines merge into a running
/// fixed chunk, long source runs render as sliced chunks.
struct BodyBuilder {
    width: u16,
    chunks: Vec<BodyChunk>,
}

impl BodyBuilder {
    fn new(width: u16) -> Self {
        Self {
            width,
            chunks: Vec::new(),
        }
    }

    fn fixed(&mut self, line: Line<'static>) {
        if let Some(BodyChunk::Fixed(chunk)) = self.chunks.last_mut() {
            chunk.lines.push(line);
        } else {
            self.chunks.push(BodyChunk::fixed(vec![line]));
        }
    }

    fn rows(&mut self, source: BodySource) {
        if source.row_count() == 0 {
            return;
        }
        self.chunks
            .push(BodyChunk::rows(source, 0, self.width, false));
    }
}

fn push_output_rows(tool: &ToolBlock, body: &mut BodyBuilder, is_shell: bool) -> bool {
    let (stdout_body, stderr_body): (&str, &str) = if is_shell {
        let (_, body) = split_status_line(&tool.output);
        (body, tool.stderr.as_str())
    } else {
        (tool.output.as_str(), "")
    };
    let stdout_count = stdout_body.lines().count() as u32;
    let stderr_count = stderr_body.lines().count() as u32;

    if tool.expanded {
        let mut has_output = false;
        if stdout_count > 0 {
            body.rows(BodySource::Text {
                rows: TextRows::new(stdout_body),
                prefix: "",
                style: Style::new().fg(theme::TEXT_DIM),
            });
            has_output = true;
        }
        if stderr_count > 0 {
            if has_output {
                body.fixed(Line::from(""));
            }
            body.rows(BodySource::Text {
                rows: TextRows::new(stderr_body),
                prefix: "",
                style: Style::new().fg(theme::ERROR),
            });
            has_output = true;
        }
        return has_output;
    }

    let (display, fg) = if is_shell && stderr_count > 0 {
        (stderr_body, theme::ERROR)
    } else {
        (stdout_body, theme::TEXT_DIM)
    };
    if tool.status == ToolStatus::Ok && hides_output_when_collapsed(&tool.name) {
        return false;
    }
    push_tail_rows(body, display, fg)
}

/// The collapsed preview: the last [`COLLAPSED_OUTPUT_LINES`] rows plus a
/// `… +N more lines` hint when rows are hidden.
fn push_tail_rows(body: &mut BodyBuilder, text: &str, fg: Color) -> bool {
    let count = text.lines().count() as u32;
    let hidden = count.saturating_sub(COLLAPSED_OUTPUT_LINES as u32);
    let mut pushed = false;
    for row in text.lines().skip(hidden as usize) {
        body.fixed(Line::from(Span::raw(row.to_string()).fg(fg)));
        pushed = true;
    }
    if hidden > 0 {
        body.fixed(Line::from(
            Span::raw(format!("… +{hidden} more lines"))
                .fg(theme::TEXT_MUTED)
                .italic(),
        ));
        pushed = true;
    }
    pushed
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

fn push_todo_rows(
    body: &mut BodyBuilder,
    items: &[shuvarie_core::tools::todos::TodoItem],
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
        body.fixed(Line::from(vec![
            Span::raw("  ").fg(theme::TEXT_MUTED),
            Span::raw(marker).fg(marker_fg).bold(),
            Span::raw(" ").fg(theme::TEXT_MUTED),
            Span::raw(item.text.clone()).style(text_style),
        ]));
    }
    if hidden > 0 {
        body.fixed(Line::from(
            Span::raw(format!("… +{hidden} more items"))
                .fg(theme::TEXT_MUTED)
                .italic(),
        ));
    }
}

/// The bottom meta row of a tool block: `Elapsed 4.5s` while the call is
/// running (recomputed every frame from the wall clock), `Took 10.3s` once it
/// finishes — the finished value comes from the core-measured duration. Only
/// shown for `shows_elapsed` blocks, with the value dimmed like the label so
/// timing never competes with the block's content.
fn elapsed_line(tool: &ToolBlock) -> Line<'static> {
    if tool.status == ToolStatus::Running {
        let ms = tool
            .started_at
            .map(|started| started.elapsed().as_millis() as u64)
            .unwrap_or_default();
        Line::from(vec![
            Span::raw("  Elapsed ").fg(theme::TEXT_MUTED).italic(),
            Span::raw(format_duration_ms(ms))
                .fg(theme::TEXT_MUTED)
                .italic(),
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

fn push_question_block_lines(body: &mut BodyBuilder, tool: &ToolBlock) {
    if tool.status == ToolStatus::Running {
        body.fixed(
            Span::raw("  Asking...")
                .fg(theme::TEXT_MUTED)
                .italic()
                .into(),
        );
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
        body.fixed(
            Span::raw(format!("  {note}"))
                .fg(theme::TEXT_DIM)
                .italic()
                .into(),
        );
        return;
    }
    for q in &questions {
        let answer = answer_for_question(&tool.output, &q.question);
        body.fixed(Line::from(vec![
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
        body.fixed(Line::from(Span::raw(format!("{marker}{shown}")).fg(fg)));
    }
}

fn push_diagnostics_lines(body: &mut BodyBuilder, env: &ChatEnv, path: &str) {
    let Some(diags) = env.lsp_diagnostics.get(path) else {
        return;
    };
    if diags.is_empty() {
        return;
    }
    body.fixed(Line::from(""));
    body.fixed(Line::from(vec![
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
        body.fixed(Line::from(vec![
            Span::raw(format!("    {loc:<10} ")).fg(theme::TEXT_MUTED),
            Span::raw(format!("{sev_label:<8} ")).fg(sev_color),
            Span::raw(msg).fg(theme::TEXT_DIM),
        ]));
    }
}

fn push_file_change_rows(body: &mut BodyBuilder, change: &FileChange) {
    match change {
        FileChange::Edit { path, diff, .. } => {
            body.fixed(Line::from(vec![
                Span::raw("  ── diff: ").fg(theme::TEXT_MUTED),
                Span::raw(path.clone()).fg(theme::ACCENT),
            ]));
            body.rows(BodySource::Diff {
                lines: Rc::from(diff.as_slice()),
            });
        }
        FileChange::Write { path, content, .. } => {
            body.fixed(Line::from(vec![
                Span::raw("  ── new file: ").fg(theme::TEXT_MUTED),
                Span::raw(path.clone()).fg(theme::ACCENT),
            ]));
            body.rows(BodySource::Numbered {
                rows: TextRows::new(content.as_str()),
            });
        }
        FileChange::Patch { files, .. } => {
            for file in files {
                match (&file.kind, &file.moved_to) {
                    (PatchFileKind::Delete, _) => {
                        body.fixed(Line::from(vec![
                            Span::raw("  ── deleted: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ERROR),
                        ]));
                    }
                    (PatchFileKind::Add, _) => {
                        body.fixed(Line::from(vec![
                            Span::raw("  ── new file: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                        ]));
                        body.rows(BodySource::Numbered {
                            rows: TextRows::new(file.new.as_deref().unwrap_or_default()),
                        });
                    }
                    (PatchFileKind::Update, Some(target)) => {
                        body.fixed(Line::from(vec![
                            Span::raw("  ── moved: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                            Span::raw(" → ").fg(theme::TEXT_MUTED),
                            Span::raw(target.clone()).fg(theme::ACCENT),
                        ]));
                        body.rows(BodySource::Diff {
                            lines: Rc::from(file.diff.as_slice()),
                        });
                    }
                    (PatchFileKind::Update, None) => {
                        body.fixed(Line::from(vec![
                            Span::raw("  ── diff: ").fg(theme::TEXT_MUTED),
                            Span::raw(file.path.clone()).fg(theme::ACCENT),
                        ]));
                        body.rows(BodySource::Diff {
                            lines: Rc::from(file.diff.as_slice()),
                        });
                    }
                }
            }
        }
    }
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

/// The terminal status of a tool result: `ok` succeeds; a `run_shell` call
/// whose stdout leads with a `timeout Ns:` status line was killed by the
/// shell timeout rather than failing on its own merits.
fn finish_status(ok: bool, name: &str, output: &str) -> ToolStatus {
    if ok {
        return ToolStatus::Ok;
    }
    if name == "run_shell"
        && split_status_line(output)
            .0
            .is_some_and(|label| label.starts_with("timeout"))
    {
        return ToolStatus::Killed;
    }
    ToolStatus::Failed
}

/// Split a leading shell status line (`exit N:`, `timeout Ns:` or the legacy
/// `shell exited with ...:`) off the persisted stdout text, returning the
/// normalized label and the remaining body. Text whose first line does not
/// parse as a status line — e.g. a streamed tail — is all body.
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
    match label {
        Some(label) => (Some(label), rest),
        None => (None, text),
    }
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

    use shuvarie_llm::{DiffLine, DiffLineKind};

    use super::*;

    fn block_text(block: &ToolBlock, env: &ChatEnv) -> String {
        block
            .view(80, env)
            .flattened()
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
    fn body_chunks_reuse_across_spinner_frames() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.into(), None, None);
        let first = block.cached_body(80, &env);
        let again = block.cached_body(80, &env);
        assert!(
            Rc::ptr_eq(&first, &again),
            "unchanged state must reuse chunks"
        );

        assert!(
            block.update(ToolMessage::Output {
                stdout: "file one\nfile two".into(),
                stderr: String::new(),
            }),
            "output update must apply"
        );
        let after_output = block.cached_body(80, &env);
        assert!(!Rc::ptr_eq(&first, &after_output), "output must rebuild");

        block.toggle();
        let after_toggle = block.cached_body(80, &env);
        assert!(
            !Rc::ptr_eq(&after_output, &after_toggle),
            "toggle must rebuild"
        );

        let env2 = ChatEnv {
            rev: 1,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let after_env = block.cached_body(80, &env2);
        assert!(
            !Rc::ptr_eq(&after_toggle, &after_env),
            "env change must rebuild"
        );
    }

    #[test]
    fn est_is_cached_until_body_changes() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.into(), None, None);
        let est = block.est();
        assert_eq!(
            est,
            block.est(),
            "unchanged body_rev must reuse the estimate"
        );
        assert!(
            block.update(ToolMessage::Output {
                stdout: "row\n".repeat(30),
                stderr: String::new(),
            }),
            "output update must apply"
        );
        let grown = block.est();
        assert!(
            grown.tool_rows > est.tool_rows,
            "output growth must be reflected: {} -> {}",
            est.tool_rows,
            grown.tool_rows
        );
        assert_eq!(grown, block.est());
        let _ = block.view(80, &env);
    }

    fn finished_edit_block() -> ToolBlock {
        let mut block = ToolBlock::new(
            "edit_file",
            r#"{"path":"src/main.rs"}"#.to_string(),
            None,
            None,
        );
        block.update(ToolMessage::Finish {
            ok: true,
            output: String::new(),
            stderr: String::new(),
            file_change: Some(FileChange::Edit {
                path: "src/main.rs".to_string(),
                diff: vec![
                    DiffLine {
                        kind: DiffLineKind::Remove,
                        old_line: Some(3),
                        new_line: None,
                        text: "        let value = old();\n".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Add,
                        old_line: None,
                        new_line: Some(3),
                        text: "        let value = new();\n".to_string(),
                    },
                ],
                original: String::new(),
                new: String::new(),
            }),
            duration_ms: 10,
        });
        block
    }

    #[test]
    fn edit_diff_rows_align_and_keep_indentation() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = finished_edit_block();
        let width = 80u16;
        let seg = block.view(width, &env);
        assert!(!seg.trim);
        let content_width = width.saturating_sub(2 * BLOCK_PADDING.0);
        let h = seg.measure(content_width);
        let mut buf = Buffer::empty(Rect::new(0, 0, width, h as u16));
        seg.paint(buf.area, 0, 0, h, content_width, &mut buf);
        let rows: Vec<String> = (0..h as u16)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        let minus = rows
            .iter()
            .find(|r| r.contains("-        let value = old();"))
            .unwrap_or_else(|| panic!("minus row missing: {rows:?}"));
        let plus = rows
            .iter()
            .find(|r| r.contains("+        let value = new();"))
            .unwrap_or_else(|| panic!("plus row missing: {rows:?}"));
        assert_eq!(minus.find('-').unwrap(), plus.find('+').unwrap());
    }

    #[test]
    fn running_block_shows_live_elapsed() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.to_string(), None, None);
        let text = block_text(&block, &env);
        assert!(text.contains("Elapsed "), "header/body: {text}");
        assert!(!text.contains("Took "));
    }

    #[test]
    fn finished_block_shows_took_with_core_duration() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.to_string(), None, None);
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
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::from_record(&ToolRecord {
            name: "run_shell".to_string(),
            args_json: r#"{"command":"ls"}"#.to_string(),
            output: String::new(),
            stderr: String::new(),
            ok: true,
            killed: false,
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
        assert_eq!(block.est().tool_rows, 1);
    }

    #[test]
    fn reloaded_killed_record_shows_killed() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let record = ToolRecord {
            name: "run_shell".to_string(),
            args_json: r#"{"command":"sleep 30"}"#.to_string(),
            output: String::new(),
            stderr: String::new(),
            ok: false,
            killed: true,
            worker: None,
            message_id: 1,
            message_seq: 1,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 4_200,
        };
        let block = ToolBlock::from_record(&record);
        assert!(!block.is_running(), "a killed record does not animate");
        let text = block_text(&block, &env);
        assert!(text.contains("⏹"), "killed marker: {text}");
        assert!(text.contains("killed"), "killed label: {text}");
        assert!(text.contains("Took 4.2s"), "killed duration: {text}");
        assert!(!text.contains("✗"), "killed is not failed: {text}");
    }

    #[test]
    fn reloaded_timeout_record_shows_killed() {
        // A timeout kill persists as a finished-but-killed run: the record is
        // not flagged, so the status comes from the shell status line.
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let record = ToolRecord {
            name: "run_shell".to_string(),
            args_json: r#"{"command":"cargo build"}"#.to_string(),
            output: "timeout 30s:\npartial".to_string(),
            stderr: String::new(),
            ok: false,
            killed: false,
            worker: None,
            message_id: 1,
            message_seq: 1,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 30_100,
        };
        let block = ToolBlock::from_record(&record);
        assert!(!block.is_running());
        let text = block_text(&block, &env);
        assert!(text.contains("⏹"), "header/body: {text}");
        assert!(text.contains("timeout 30s"), "header/body: {text}");
        assert!(!text.contains("✗"), "header/body: {text}");
    }

    #[test]
    fn other_tool_blocks_omit_elapsed_row() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("grep", r#"{"pattern":"x"}"#.to_string(), None, None);
        assert_eq!(block.est().tool_rows, 0);
        let running = block_text(&block, &env);
        assert!(
            !running.contains("Elapsed "),
            "running header/body: {running}"
        );
        assert!(!running.contains("Took "));
        block.update(ToolMessage::Finish {
            ok: true,
            output: "match".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 10_300,
        });
        let finished = block_text(&block, &env);
        assert!(
            !finished.contains("Elapsed "),
            "finished header/body: {finished}"
        );
        assert!(!finished.contains("Took "));
    }

    #[test]
    fn worker_block_shows_elapsed_and_took() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "explore_workspace",
            r#"{"task":"find the config loader"}"#.to_string(),
            Some(String::new()),
            None,
        );
        assert_eq!(block.est().tool_rows, 1);
        let running = block_text(&block, &env);
        assert!(
            running.contains("Elapsed "),
            "running header/body: {running}"
        );
        block.update(ToolMessage::Finish {
            ok: true,
            output: "report".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 12_000,
        });
        let finished = block_text(&block, &env);
        assert!(
            finished.contains("Took 12.0s"),
            "finished header/body: {finished}"
        );
        assert!(!finished.contains("Elapsed "));
    }

    fn todo_output() -> String {
        "Added #3 \"update UI\"\n\nTodos (1/3 done)\n  #1 [x] set up schema\n  #2 [~] write migration\n  #3 [ ] update UI".to_string()
    }

    #[test]
    fn finished_todo_block_renders_status_list() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "todo",
            r#"{"op":"add","text":"update UI"}"#.to_string(),
            None,
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
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = ToolBlock::from_record(&ToolRecord {
            name: "todo".to_string(),
            args_json: r#"{"op":"list"}"#.to_string(),
            output: "Todos (1/3 done)\n  #1 [x] set up schema\n  #2 [~] write migration\n  #3 [ ] update UI".to_string(),
            stderr: String::new(),
            ok: true,
            killed: false,
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
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "todo",
            r#"{"op":"update","id":9,"status":"done"}"#.to_string(),
            None,
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

    #[test]
    fn collapsed_read_file_hides_successful_output() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "read_file",
            r#"{"path":"src/main.rs"}"#.to_string(),
            None,
            None,
        );
        block.update(ToolMessage::Finish {
            ok: true,
            output: "fn main() {}\nfn second() {}\nfn third() {}\nfn fourth() {}".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        let text = block_text(&block, &env);
        assert!(
            text.contains("read_file src/main.rs"),
            "header/body: {text}"
        );
        assert!(text.contains("· lines 1–4"), "header/body: {text}");
        assert!(!text.contains("fn main()"), "output leaked: {text}");
        assert!(!text.contains("more lines"), "hint leaked: {text}");
        assert_eq!(block.est().tool_rows, 0);
    }

    #[test]
    fn read_file_toggle_reveals_output() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "read_file",
            r#"{"path":"src/main.rs"}"#.to_string(),
            None,
            None,
        );
        block.update(ToolMessage::Finish {
            ok: true,
            output: "fn main() {}\nfn second() {}".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        block.toggle();
        let text = block_text(&block, &env);
        assert!(text.contains("fn main() {}"), "header/body: {text}");
        assert_eq!(block.est().tool_rows, 2);
    }

    #[test]
    fn collapsed_list_dir_hides_successful_output() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("list_dir", r#"{"path":"crates"}"#.to_string(), None, None);
        block.update(ToolMessage::Finish {
            ok: true,
            output: "core/\nllm/\nmain.rs".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("list_dir"), "header/body: {text}");
        assert!(!text.contains("core/"), "output leaked: {text}");
        assert!(!text.contains("more lines"), "hint leaked: {text}");
        assert_eq!(block.est().tool_rows, 0);
    }

    #[test]
    fn failed_read_file_keeps_error_visible() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new(
            "read_file",
            r#"{"path":"src/main.rs"}"#.to_string(),
            None,
            None,
        );
        block.update(ToolMessage::Finish {
            ok: false,
            output: "'src/main.rs' is a directory, not a file".to_string(),
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        let text = block_text(&block, &env);
        assert!(text.contains("is a directory"), "header/body: {text}");
    }

    fn big_output(count: usize) -> String {
        (0..count)
            .map(|i| {
                if i % 9 == 0 {
                    format!("out {i}: a long tail that will certainly wrap past the pane edge {i}")
                } else {
                    format!("out {i}: payload {i} with filler text")
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn finished_block(name: &str, args: &str, output: String) -> ToolBlock {
        let mut block = ToolBlock::new(name, args.to_string(), None, None);
        block.update(ToolMessage::Finish {
            ok: true,
            output,
            stderr: String::new(),
            file_change: None,
            duration_ms: 5,
        });
        block
    }

    #[test]
    fn expanded_big_output_renders_sliced_and_matches_materialized() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = finished_block("grep", r#"{"pattern":"x"}"#, big_output(300));
        block.toggle();
        let width = 80u16;
        let seg = block.view(width, &env);
        assert_eq!(block.est().tool_rows, 300);
        let count_width = width.saturating_sub(2 * BLOCK_PADDING.0);
        assert!(matches!(
            seg.chunks.as_slice(),
            [BodyChunk::Fixed(_), BodyChunk::Sliced(_)]
        ));
        let materialized_lines: Vec<Line<'static>> = big_output(300)
            .lines()
            .map(|row| Line::from(Span::raw(row.to_string()).fg(theme::TEXT_DIM)))
            .collect();
        let materialized = Segment::materialized(
            std::iter::once(block.header_line(count_width as usize))
                .chain(materialized_lines)
                .collect(),
            Some(theme::SUCCESS_BG),
            BLOCK_PADDING,
            false,
        );
        assert_eq!(seg.measure(width), materialized.measure(width));
        let full = crate::tui::session::segment::tests::full_render(&materialized, width);
        crate::tui::session::segment::tests::assert_windows_match(
            &seg,
            width,
            &full,
            &[0, 13, 60, 155, 299, u32::from(full.area().height) - 1],
        );
    }

    #[test]
    fn collapsed_big_output_stays_cheap_and_windows_correctly() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let block = finished_block("grep", r#"{"pattern":"x"}"#, big_output(300));
        let width = 80u16;
        let seg = block.view(width, &env);
        assert!(
            !seg.chunks
                .iter()
                .any(|chunk| matches!(chunk, BodyChunk::Sliced(_))),
            "collapsed preview must stay materialized"
        );
        let text = block_text(&block, &env);
        assert!(text.contains("out 299:"), "tail: {text}");
        assert!(text.contains("… +295 more lines"), "hint: {text}");
        assert_eq!(block.est().tool_rows, collapsed_rows(300));
    }

    #[test]
    fn big_write_content_renders_windowed() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let content: String = (0..200)
            .map(|i| format!("fn generated_{i}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut block =
            ToolBlock::new("write_file", r#"{"path":"gen.rs"}"#.to_string(), None, None);
        block.update(ToolMessage::Finish {
            ok: true,
            output: String::new(),
            stderr: String::new(),
            file_change: Some(FileChange::Write {
                path: "gen.rs".to_string(),
                content,
                original: None,
            }),
            duration_ms: 5,
        });
        let width = 80u16;
        let seg = block.view(width, &env);
        assert!(
            seg.chunks
                .iter()
                .any(|chunk| matches!(chunk, BodyChunk::Sliced(_))),
            "200-row write content must render sliced"
        );
        let text = block_text(&block, &env);
        assert!(text.contains("fn generated_199() {}"), "tail: {text}");
        assert_eq!(block.est().tool_rows, 1 + 200);
        let full = crate::tui::session::segment::tests::full_render(&seg, width);
        crate::tui::session::segment::tests::assert_windows_match(
            &seg,
            width,
            &full,
            &[0, 7, 90, 190, u32::from(full.area().height) - 1],
        );
    }

    #[test]
    fn big_diff_renders_windowed() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let diff = (0..200)
            .map(|i| DiffLine {
                kind: if i % 2 == 0 {
                    DiffLineKind::Remove
                } else {
                    DiffLineKind::Add
                },
                old_line: (i % 2 == 0).then_some(i as u64),
                new_line: (i % 2 == 1).then_some(i as u64),
                text: format!("        let value_{i} = compute({i});"),
            })
            .collect();
        let mut block = ToolBlock::new("edit_file", r#"{"path":"a.rs"}"#.to_string(), None, None);
        block.update(ToolMessage::Finish {
            ok: true,
            output: String::new(),
            stderr: String::new(),
            file_change: Some(FileChange::Edit {
                path: "a.rs".to_string(),
                diff,
                original: String::new(),
                new: String::new(),
            }),
            duration_ms: 5,
        });
        let width = 80u16;
        let seg = block.view(width, &env);
        assert!(
            seg.chunks
                .iter()
                .any(|chunk| matches!(chunk, BodyChunk::Sliced(_))),
            "200-row diff must render sliced"
        );
        let full = crate::tui::session::segment::tests::full_render(&seg, width);
        crate::tui::session::segment::tests::assert_windows_match(
            &seg,
            width,
            &full,
            &[0, 5, 70, 150, 199, u32::from(full.area().height) - 1],
        );
    }

    #[test]
    fn expanded_shell_output_keeps_gap_and_stderr_order() {
        let env = ChatEnv {
            rev: 0,
            lsp_diagnostics: &BTreeMap::new(),
        };
        let mut block = ToolBlock::new("run_shell", r#"{"command":"ls"}"#.to_string(), None, None);
        block.update(ToolMessage::Finish {
            ok: false,
            output: "exit 1:\n".to_string() + &big_output(300),
            stderr: "boom".to_string(),
            file_change: None,
            duration_ms: 5,
        });
        block.toggle();
        let width = 80u16;
        let seg = block.view(width, &env);
        let text = block_text(&block, &env);
        let stdout_pos = text.find("out 0:").expect("stdout rows");
        let gap = text.find("\n\n").expect("blank gap");
        let boom = text.find("boom").expect("stderr row");
        assert!(stdout_pos < gap && gap < boom, "order: {text}");
        let full = crate::tui::session::segment::tests::full_render(&seg, width);
        crate::tui::session::segment::tests::assert_windows_match(
            &seg,
            width,
            &full,
            &[0, 300, u32::from(full.area().height) - 1],
        );
    }
}
