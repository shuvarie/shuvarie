use std::cell::Cell;
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
    ScrollUp,
    ScrollDown,
    Dismiss,
}

/// Floating display-only window for the latest bash-mode (`!`) run, anchored
/// above the prompt input. The run's captured output is shown verbatim (raw
/// text, one row per line, no trimming, wrapping, or markdown) and stays
/// scrolled to the newest rows unless the user wheels up; Escape dismisses and
/// a new run reopens it. The content is never persisted nor sent to the model.
pub struct BashPopup {
    id: Option<u64>,
    command: String,
    running: bool,
    ok: bool,
    exit: Option<i32>,
    stdout: StreamText,
    stderr: StreamText,
    duration_ms: u64,
    started_at: Option<Instant>,
    open: bool,
    pub(crate) scroll: Cell<usize>,
    pub(crate) follow: Cell<bool>,
}

impl BashPopup {
    pub fn new() -> Self {
        Self {
            id: None,
            command: String::new(),
            running: false,
            ok: true,
            exit: None,
            stdout: StreamText::new(),
            stderr: StreamText::new(),
            duration_ms: 0,
            started_at: None,
            open: false,
            scroll: Cell::new(0),
            follow: Cell::new(true),
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
                self.stdout = StreamText::new();
                self.stderr = StreamText::new();
                self.duration_ms = 0;
                self.started_at = Some(Instant::now());
                self.open = true;
                self.scroll.set(0);
                self.follow.set(true);
            }
            BashMessage::Output { id, stdout, stderr } => {
                if self.id == Some(id) && self.running {
                    self.stdout.set(stdout);
                    self.stderr.set(stderr);
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
                    self.stdout.set(stdout);
                    self.stderr.set(stderr);
                    self.duration_ms = duration_ms;
                    self.started_at = None;
                }
            }
            BashMessage::ScrollUp => {
                self.follow.set(false);
                self.scroll.set(self.scroll.get().saturating_sub(1));
            }
            BashMessage::ScrollDown => {
                self.scroll.set(self.scroll.get().saturating_add(1));
            }
            BashMessage::Dismiss => self.open = false,
        }
    }

    fn body_total(&self) -> usize {
        self.stdout.rows() + self.stderr.rows()
    }

    fn body_row(&self, i: usize) -> (&str, Color) {
        let stdout_rows = self.stdout.rows();
        if i < stdout_rows {
            (self.stdout.row(i), theme::text_dim())
        } else {
            (self.stderr.row(i - stdout_rows), theme::error())
        }
    }

    /// The painted popup rect for the `(history, input)` layout areas, or
    /// `None` when closed or clipped away. Shared by `view` and mouse
    /// hit-testing in the session's zone router.
    pub fn rect_for(&self, history: Rect, input: Rect) -> Option<Rect> {
        if !self.open() || history.height == 0 || input.width < 4 {
            return None;
        }
        let width = input.width;
        let inner_budget = usize::from(history.height.saturating_sub(4).max(1));
        let content_rows = 1 + self.body_total().min(inner_budget.saturating_sub(1));
        let help_row = 1;
        let height = (content_rows as u16 + help_row + 3).min(history.height);
        let y = input.y.saturating_sub(height).max(history.y);
        let area = Rect::new(
            input.x,
            y,
            width,
            input.y.saturating_sub(y).max(height.min(1)),
        );
        (!area.is_empty()).then_some(area)
    }

    /// Paint the popup floating above the input area, clamped into the
    /// history pane. Rendered last so it floats over the chat content.
    pub fn view(&self, frame: &mut Frame<'_>, history: Rect, input: Rect) {
        let Some(area) = self.rect_for(history, input) else {
            return;
        };

        let block = theme::overlay_block("bash");
        let inner = block.inner(area);
        frame.render_widget(Clear, area);
        frame.render_widget(block, area);

        let inner_w = usize::from(inner.width);
        let viewport = usize::from(inner.height.saturating_sub(2));
        let mut lines = self.body_lines(inner_w, viewport);
        lines.push(theme::help_line(&[("Esc", "dismiss")]).fg(theme::text_muted()));
        frame.render_widget(ratatui::widgets::Paragraph::new(lines), inner);
    }

    /// Resolve the scroll offset against the body's row total and viewport,
    /// re-engaging the tail follow when the view reaches the bottom.
    fn window(&self, viewport: usize) -> usize {
        let max_offset = self.body_total().saturating_sub(viewport);
        let offset = if self.follow.get() {
            max_offset
        } else {
            self.scroll.get().min(max_offset)
        };
        self.scroll.set(offset);
        self.follow.set(offset >= max_offset);
        offset
    }

    /// The popup content: status header, then the body's viewport rows.
    fn body_lines(&self, inner_w: usize, viewport: usize) -> Vec<Line<'_>> {
        let total = self.body_total();
        let offset = self.window(viewport);
        let mut lines = Vec::with_capacity(viewport + 1);
        lines.push(self.header_line(inner_w));
        for i in offset..(offset + viewport).min(total) {
            let (row, fg) = self.body_row(i);
            let clipped: String = row.chars().take(inner_w).collect();
            lines.push(Line::from(clipped).fg(fg));
        }
        lines
    }

    fn header_line(&self, inner_w: usize) -> Line<'static> {
        let mut header: Vec<Span<'static>> = Vec::new();
        if self.running {
            header.push(spinner::spinner());
        } else if self.ok {
            header.push(Span::raw("✓").fg(theme::success()).bold());
        } else {
            header.push(Span::raw("✗").fg(theme::error()).bold());
        }
        header.push(Span::raw(" "));
        header.push(
            Span::raw(format!("$ {}", self.command))
                .fg(theme::text())
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
            if let Some(code) = self.exit.filter(|code| *code != 0) {
                parts.push(format!("exit {code}"));
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
            header.push(Span::raw(right).fg(theme::text_muted()));
        }
        Line::from(header)
    }
}

impl Default for BashPopup {
    fn default() -> Self {
        Self::new()
    }
}

/// A captured stream held as raw text plus precomputed row-start offsets, so
/// only the viewport's rows materialize when the popup paints.
struct StreamText {
    text: String,
    starts: Vec<usize>,
}

impl StreamText {
    fn new() -> Self {
        Self {
            text: String::new(),
            starts: Vec::new(),
        }
    }

    fn set(&mut self, text: String) {
        self.starts.clear();
        let bytes = text.as_bytes();
        if !bytes.is_empty() {
            self.starts.push(0);
            for (i, byte) in bytes.iter().enumerate() {
                if *byte == b'\n' && i + 1 < bytes.len() {
                    self.starts.push(i + 1);
                }
            }
        }
        self.text = text;
    }

    fn rows(&self) -> usize {
        self.starts.len()
    }

    fn row(&self, i: usize) -> &str {
        let start = self.starts[i];
        let end = match self.starts.get(i + 1) {
            Some(&next) => next - 1,
            None => self.text.len() - usize::from(self.text.ends_with('\n')),
        };
        self.text[start..end]
            .strip_suffix('\r')
            .unwrap_or(&self.text[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finish(popup: &mut BashPopup, stdout: &str) {
        popup.update(BashMessage::Finished {
            id: 1,
            ok: true,
            exit: Some(0),
            stdout: stdout.into(),
            stderr: String::new(),
            duration_ms: 10,
        });
    }

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
        assert!(popup.follow.get());

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
            stdout: "error: could not compile".into(),
            stderr: String::new(),
            duration_ms: 1500,
        });
        assert!(popup.open(), "a finished run stays open until dismissed");
        assert!(!popup.running());

        popup.update(BashMessage::Output {
            id: 3,
            stdout: "late".into(),
            stderr: String::new(),
        });
        assert_eq!(popup.stdout.text, "error: could not compile");

        popup.update(BashMessage::Started {
            id: 4,
            command: "ls".into(),
        });
        assert!(popup.open(), "a new run reopens the popup");
        assert_eq!(popup.command, "ls");
        assert_eq!(popup.stdout.text, "");
        assert_eq!(popup.scroll.get(), 0, "a new run resets the scroll");
        assert!(popup.follow.get());
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
        let lines = popup.body_lines(80, 12);
        assert_eq!(lines.len(), 1);
        let text: String = lines[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("$ make test"), "header shows the command");

        popup.update(BashMessage::Finished {
            id: 1,
            ok: false,
            exit: Some(2),
            stdout: "no rule".into(),
            stderr: String::new(),
            duration_ms: 700,
        });
        let text: String = popup.body_lines(80, 12)[0]
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert!(text.contains("exit 2"), "failure label in the header");
        assert!(text.contains("0.7s"), "duration in the header");
    }

    #[test]
    fn output_renders_raw_and_verbatim() {
        let mut popup = BashPopup::new();
        popup.update(BashMessage::Started {
            id: 1,
            command: "printf".into(),
        });
        finish(&mut popup, "  leading spaces stay  \n\n\ttab too\n");
        let lines = popup.body_lines(80, 12);
        assert_eq!(lines.len(), 4, "header + 3 rows, trailing newline dropped");
        let text = |line: &Line<'_>| {
            line.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(&lines[1]), "  leading spaces stay  ");
        assert_eq!(text(&lines[2]), "");
        assert_eq!(text(&lines[3]), "\ttab too");
    }

    #[test]
    fn full_output_fits_when_below_the_viewport() {
        let mut popup = BashPopup::new();
        popup.update(BashMessage::Started {
            id: 1,
            command: "seq 3".into(),
        });
        finish(&mut popup, "1\n2\n3");
        let lines = popup.body_lines(80, 12);
        assert_eq!(lines.len(), 4, "header + every row");
        let last = lines[3].spans[0].content.to_string();
        assert_eq!(last, "3");
    }

    #[test]
    fn overflow_follows_the_tail_and_scrolls() {
        let mut popup = BashPopup::new();
        popup.update(BashMessage::Started {
            id: 1,
            command: "seq 30".into(),
        });
        popup.update(BashMessage::Output {
            id: 1,
            stdout: (1..=30).map(|i| format!("line {i}\n")).collect(),
            stderr: String::new(),
        });

        let text = |line: &Line<'_>| line.spans[0].content.to_string();
        let lines = popup.body_lines(80, 4);
        assert_eq!(lines.len(), 5, "header + viewport rows, no overflow hint");
        assert_eq!(text(&lines[1]), "line 27");
        assert_eq!(text(&lines[4]), "line 30", "the tail stays visible");

        for _ in 0..26 {
            popup.update(BashMessage::ScrollUp);
        }
        let lines = popup.body_lines(80, 4);
        assert_eq!(text(&lines[1]), "line 1", "scrolled all the way to the top");
        assert!(
            !popup.follow.get(),
            "scrolling up disengages the tail follow"
        );

        popup.update(BashMessage::Output {
            id: 1,
            stdout: (1..=31).map(|i| format!("line {i}\n")).collect(),
            stderr: String::new(),
        });
        let lines = popup.body_lines(80, 4);
        assert_eq!(
            text(&lines[1]),
            "line 1",
            "new output does not move a scrolled-away view"
        );
        assert!(!popup.follow.get());

        for _ in 0..40 {
            popup.update(BashMessage::ScrollDown);
        }
        let lines = popup.body_lines(80, 4);
        assert_eq!(text(&lines[4]), "line 31", "clamped at the newest rows");
        assert!(
            popup.follow.get(),
            "reaching the bottom re-engages the follow"
        );
    }
}
