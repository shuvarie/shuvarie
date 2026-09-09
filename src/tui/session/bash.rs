use std::time::Instant;

use ratatui::prelude::*;
use ratatui::widgets::Clear;
use termina::event::{KeyCode, KeyEvent, KeyEventKind};

use super::blocks::format_duration_ms;
use crate::tui::{spinner, theme};

pub enum BashMessage {
    Started {
        id: u64,
        command: String,
    },
    Output {
        id: u64,
        stdout: String,
        stderr: String,
    },
    Finished {
        id: u64,
        ok: bool,
        exit: Option<i32>,
        stdout: String,
        stderr: String,
        duration_ms: u64,
    },
    Dismiss,
}

/// Output rows kept visible in the popup; older rows collapse into a hint.
const MAX_OUTPUT_ROWS: usize = 10;

/// Floating display-only window for the latest bash-mode (`!`) run, anchored
/// above the prompt input. Always expanded, dismissed with Escape; a new run
/// reopens it. The content is never persisted nor sent to the model.
pub struct BashPopup {
    id: Option<u64>,
    command: String,
    running: bool,
    ok: bool,
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    duration_ms: u64,
    started_at: Option<Instant>,
    open: bool,
}

impl BashPopup {
    pub fn new() -> Self {
        Self {
            id: None,
            command: String::new(),
            running: false,
            ok: true,
            exit: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms: 0,
            started_at: None,
            open: false,
        }
    }

    pub fn open(&self) -> bool {
        self.open && self.id.is_some()
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<BashMessage> {
        if self.open() && key.kind == KeyEventKind::Press && key.code == KeyCode::Escape {
            return Some(BashMessage::Dismiss);
        }
        None
    }

    pub fn update(&mut self, msg: BashMessage) {
        match msg {
            BashMessage::Started { id, command } => {
                self.id = Some(id);
                self.command = command;
                self.running = true;
                self.ok = true;
                self.exit = None;
                self.stdout = String::new();
                self.stderr = String::new();
                self.duration_ms = 0;
                self.started_at = Some(Instant::now());
                self.open = true;
            }
            BashMessage::Output { id, stdout, stderr } => {
                if self.id == Some(id) && self.running {
                    self.stdout = stdout;
                    self.stderr = stderr;
                }
            }
            BashMessage::Finished {
                id,
                ok,
                exit,
                stdout,
                stderr,
                duration_ms,
            } => {
                if self.id == Some(id) {
                    self.running = false;
                    self.ok = ok;
                    self.exit = exit;
                    self.stdout = stdout;
                    self.stderr = stderr;
                    self.duration_ms = duration_ms;
                    self.started_at = None;
                }
            }
            BashMessage::Dismiss => self.open = false,
        }
    }

    /// The popup content: status header with the command, then the output
    /// tail (at most [`MAX_OUTPUT_ROWS`] rows, oldest collapsed into a hint).
    fn content_lines(&self, width: u16, max_rows: usize) -> Vec<Line<'static>> {
        let inner_w = usize::from(width);
        let mut lines = vec![self.header_line(inner_w)];
        let budget = max_rows.saturating_sub(1).min(MAX_OUTPUT_ROWS);

        let (_, stdout_body) = split_status_line(&self.stdout);
        let stdout_rows: Vec<&str> = stdout_body.lines().collect();
        let stderr_rows: Vec<&str> = self.stderr.lines().collect();
        let separator = usize::from(!stdout_rows.is_empty() && !stderr_rows.is_empty());
        let total = stdout_rows.len() + separator + stderr_rows.len();

        let skip = total.saturating_sub(budget);
        if skip > 0 {
            lines.push(
                Line::from(format!("… +{skip} earlier lines"))
                    .fg(theme::TEXT_MUTED)
                    .italic(),
            );
        }
        let mut pushed = 0usize;
        let mut push_row = |lines: &mut Vec<Line<'static>>, row: &str, fg: Color| {
            if pushed >= skip {
                let clipped: String = row.chars().take(inner_w).collect();
                lines.push(Line::from(clipped).fg(fg));
            }
            pushed += 1;
        };
        for row in &stdout_rows {
            push_row(&mut lines, row, theme::TEXT_DIM);
        }
        if separator == 1 && skip <= stdout_rows.len() {
            lines.push(Line::from(""));
        }
        for row in &stderr_rows {
            push_row(&mut lines, row, theme::ERROR);
        }
        lines
    }

    fn header_line(&self, inner_w: usize) -> Line<'static> {
        let mut header: Vec<Span<'static>> = Vec::new();
        if self.running {
            header.push(spinner::spinner());
        } else if self.ok {
            header.push(Span::raw("✓").fg(theme::SUCCESS).bold());
        } else {
            header.push(Span::raw("✗").fg(theme::ERROR).bold());
        }
        header.push(Span::raw(" "));
        header.push(
            Span::raw(format!("$ {}", self.command))
                .fg(theme::TEXT)
                .bold(),
        );

        let right = if self.running {
            let ms = self
                .started_at
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or_default();
            format_duration_ms(ms)
        } else {
            let mut parts: Vec<String> = Vec::new();
            let (label, _) = split_status_line(&self.stdout);
            match (&label, self.exit) {
                (Some(label), _) if label != "exit 0" => parts.push(label.clone()),
                (None, Some(code)) if code != 0 => parts.push(format!("exit {code}")),
                _ => {}
            }
            parts.push(format_duration_ms(self.duration_ms));
            parts.join(" · ")
        };
        if !right.is_empty() {
            let right_w = right.chars().count();
            let pad = inner_w
                .saturating_sub(header.iter().map(|s| s.width()).sum::<usize>())
                .saturating_sub(right_w)
                .max(1);
            header.push(Span::raw(" ".repeat(pad)));
            header.push(Span::raw(right).fg(theme::TEXT_MUTED));
        }
        Line::from(header)
    }

    /// Paint the popup floating above the input area, clamped into the
    /// history pane. Rendered last so it floats over the chat content.
    pub fn view(&self, frame: &mut Frame<'_>, history: Rect, input: Rect) {
        if !self.open() || history.height == 0 || input.width < 4 {
            return;
        }
        let width = input.width;
        // Title row + uniform(1) padding bound the content: `inner` insets
        // the top by 2 (title + padding) and the bottom by 1.
        let inner_budget = history.height.saturating_sub(4).max(1) as usize;
        let content = self.content_lines(width.saturating_sub(2), inner_budget);
        let help_row = 1;
        let height = (content.len() as u16 + help_row + 3).min(history.height);
        let y = input.y.saturating_sub(height).max(history.y);
        let area = Rect::new(
            input.x,
            y,
            width,
            input.y.saturating_sub(y).max(height.min(1)),
        );
        if area.is_empty() {
            return;
        }

        frame.render_widget(Clear, area);
        let block = theme::overlay_block("bash");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let mut lines = content;
        lines.push(theme::help_line(&[("Esc", "dismiss")]).fg(theme::TEXT_MUTED));
        frame.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
    }
}

impl Default for BashPopup {
    fn default() -> Self {
        Self::new()
    }
}

/// Split a leading `exit N:` status line off the finished stdout, mirroring
/// the run_shell tool block header. Text without a newline (a live tail) is
/// all body — a status label only exists when the core prepended one.
fn split_status_line(text: &str) -> (Option<String>, &str) {
    let Some((first, rest)) = text.split_once('\n') else {
        return (None, text);
    };
    let label = first
        .strip_prefix("exit ")
        .and_then(|s| s.strip_suffix(':'))
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .map(|n| format!("exit {n}"));
    (label, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_tracks_the_latest_run() {
        let mut popup = BashPopup::new();
        assert!(!popup.open());

        popup.update(BashMessage::Started {
            id: 3,
            command: "cargo build".into(),
        });
        assert!(popup.open());
        assert!(popup.running());

        popup.update(BashMessage::Output {
            id: 3,
            stdout: "Compiling…".into(),
            stderr: String::new(),
        });
        assert!(popup.running());

        popup.update(BashMessage::Finished {
            id: 3,
            ok: false,
            exit: Some(1),
            stdout: "exit 1:\nerror: could not compile".into(),
            stderr: String::new(),
            duration_ms: 1500,
        });
        assert!(popup.open(), "a finished run stays open until dismissed");
        assert!(!popup.running());

        popup.update(BashMessage::Dismiss);
        assert!(!popup.open());

        popup.update(BashMessage::Output {
            id: 3,
            stdout: "late".into(),
            stderr: String::new(),
        });
        assert_eq!(popup.stdout, "exit 1:\nerror: could not compile");

        popup.update(BashMessage::Started {
            id: 4,
            command: "ls".into(),
        });
        assert!(popup.open(), "a new run reopens the popup");
        assert_eq!(popup.command, "ls");
        assert!(popup.stdout.is_empty());
    }

    #[test]
    fn escape_dismisses_only_while_open() {
        let mut popup = BashPopup::new();
        let esc = KeyEvent::from(KeyCode::Escape);
        assert!(popup.map_event(&esc).is_none());

        popup.update(BashMessage::Started {
            id: 0,
            command: "ls".into(),
        });
        assert!(matches!(popup.map_event(&esc), Some(BashMessage::Dismiss)));
        popup.update(BashMessage::Dismiss);
        assert!(popup.map_event(&esc).is_none());
    }

    #[test]
    fn header_shows_status_and_exit_label() {
        let mut popup = BashPopup::new();
        popup.update(BashMessage::Started {
            id: 1,
            command: "make test".into(),
        });
        let header = popup.content_lines(80, 12);
        assert_eq!(header.len(), 1);
        let text: String = header[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("$ make test"), "header shows the command");

        popup.update(BashMessage::Finished {
            id: 1,
            ok: false,
            exit: Some(2),
            stdout: "exit 2:\nno rule".into(),
            stderr: String::new(),
            duration_ms: 700,
        });
        let text: String = popup.content_lines(80, 12)[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("exit 2"), "failure label in the header");
        assert!(text.contains("0.7s"), "duration in the header");
    }

    #[test]
    fn output_shows_tail_with_overflow_hint() {
        let mut popup = BashPopup::new();
        popup.update(BashMessage::Started {
            id: 1,
            command: "seq 30".into(),
        });
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        popup.update(BashMessage::Finished {
            id: 1,
            ok: true,
            exit: Some(0),
            stdout: format!("exit 0:\n{body}"),
            stderr: String::new(),
            duration_ms: 10,
        });

        let lines = popup.content_lines(80, 6);
        assert_eq!(lines.len(), 7, "header + hint + 5 tail rows");
        let hint = lines[1].spans[0].content.to_string();
        assert_eq!(hint, "… +25 earlier lines");
        let last = lines[6].spans[0].content.to_string();
        assert_eq!(last, "line 30", "the newest rows stay visible");
    }
}
