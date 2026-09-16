use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::QuestionPrompt;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::components::InputBuffer;
use super::theme;

pub enum QuestionMessage {
    Up,
    Down,
    Prev,
    Next,
    Number(usize),
    Toggle,
    Escape,
    CustomInput(char),
    CustomPaste(String),
    CustomBackspace,
    CustomDelete,
    CustomLeft,
    CustomRight,
    CustomHome,
    CustomEnd,
    CustomLeftWord,
    CustomRightWord,
}

pub enum QuestionEffect {
    Answer {
        id: u64,
        answers: Option<Vec<Vec<String>>>,
    },
}

pub struct QuestionUI {
    pub id: u64,
    pub open: bool,
    pub questions: Vec<QuestionPrompt>,
    pub current: usize,
    pub at_confirm: bool,
    pub cursor: usize,
    pub picked: Vec<std::collections::BTreeSet<usize>>,
    pub custom_answers: Vec<Option<String>>,
    pub typing_custom: bool,
    pub typing_buffer: InputBuffer,
}

const MAX_VIEW_ROWS: usize = 12;

impl QuestionUI {
    pub fn new() -> Self {
        Self {
            id: 0,
            open: false,
            questions: Vec::new(),
            current: 0,
            at_confirm: false,
            cursor: 0,
            picked: Vec::new(),
            custom_answers: Vec::new(),
            typing_custom: false,
            typing_buffer: InputBuffer::new(),
        }
    }

    pub fn open(&mut self, id: u64, questions: Vec<QuestionPrompt>) {
        self.id = id;
        self.open = true;
        self.questions = questions;
        self.current = 0;
        self.at_confirm = false;
        self.cursor = 0;
        self.picked = self.questions.iter().map(|_| Default::default()).collect();
        self.custom_answers = self.questions.iter().map(|_| None).collect();
        self.typing_custom = false;
        self.typing_buffer.clear();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.questions.clear();
    }

    fn row_count(&self, question_index: usize) -> usize {
        let q = &self.questions[question_index];
        let mut rows = q.options.len();
        if q.custom {
            rows += 1;
        }
        rows
    }

    fn has_confirm_view(&self) -> bool {
        self.questions.len() > 1
    }

    fn all_answered(&self) -> bool {
        self.questions
            .iter()
            .zip(&self.picked)
            .zip(&self.custom_answers)
            .all(|((_, picked), custom)| !picked.is_empty() || custom.is_some())
    }

    fn build_answers(&self) -> Vec<Vec<String>> {
        self.questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                if let Some(text) = &self.custom_answers[i] {
                    return vec![text.clone()];
                }
                self.picked[i]
                    .iter()
                    .filter_map(|&idx| q.options.get(idx).map(|o| o.label.clone()))
                    .collect()
            })
            .collect()
    }

    fn advance_after_answer(&mut self) -> Option<QuestionEffect> {
        if self.questions.len() == 1 && !self.questions[0].multiple {
            let id = self.id;
            let answers = self.build_answers();
            self.close();
            return Some(QuestionEffect::Answer {
                id,
                answers: Some(answers),
            });
        }
        None
    }

    pub fn map_event(&self, key: &KeyEvent) -> Option<QuestionMessage> {
        if !self.open {
            return None;
        }
        if self.typing_custom {
            if ctrl_mod(key) {
                return match key.code {
                    KeyCode::Char('b') => Some(QuestionMessage::CustomLeft),
                    KeyCode::Char('f') => Some(QuestionMessage::CustomRight),
                    KeyCode::Char('a') => Some(QuestionMessage::CustomHome),
                    KeyCode::Char('e') => Some(QuestionMessage::CustomEnd),
                    KeyCode::Char('d') => Some(QuestionMessage::CustomDelete),
                    KeyCode::Char('h') => Some(QuestionMessage::CustomBackspace),
                    _ => None,
                };
            }
            if key.modifiers.contains(Modifiers::ALT) {
                return match key.code {
                    KeyCode::Char('b') => Some(QuestionMessage::CustomLeftWord),
                    KeyCode::Char('f') => Some(QuestionMessage::CustomRightWord),
                    _ => None,
                };
            }
            return match key.code {
                KeyCode::Enter => Some(QuestionMessage::Toggle),
                KeyCode::Backspace => Some(QuestionMessage::CustomBackspace),
                KeyCode::Delete => Some(QuestionMessage::CustomDelete),
                KeyCode::Left => Some(QuestionMessage::CustomLeft),
                KeyCode::Right => Some(QuestionMessage::CustomRight),
                KeyCode::Home => Some(QuestionMessage::CustomHome),
                KeyCode::End => Some(QuestionMessage::CustomEnd),
                KeyCode::Escape => Some(QuestionMessage::Escape),
                KeyCode::Char(c) => Some(QuestionMessage::CustomInput(c)),
                _ => None,
            };
        }
        if ctrl_mod(key) {
            return match key.code {
                KeyCode::Char('n') => Some(QuestionMessage::Down),
                KeyCode::Char('p') => Some(QuestionMessage::Up),
                _ => None,
            };
        }
        match key.code {
            KeyCode::Up => Some(QuestionMessage::Up),
            KeyCode::Down => Some(QuestionMessage::Down),
            KeyCode::Left => Some(QuestionMessage::Prev),
            KeyCode::Right | KeyCode::Tab => Some(QuestionMessage::Next),
            KeyCode::Enter => Some(QuestionMessage::Toggle),
            KeyCode::Escape => Some(QuestionMessage::Escape),
            KeyCode::Char(c @ '1'..='9') => Some(QuestionMessage::Number(
                c.to_digit(10).unwrap() as usize - 1,
            )),
            _ => None,
        }
    }

    pub fn update(&mut self, msg: QuestionMessage) -> Option<QuestionEffect> {
        if !self.open {
            return None;
        }
        match msg {
            QuestionMessage::CustomInput(c) => {
                if self.typing_custom {
                    self.typing_buffer.push(c);
                }
                None
            }
            QuestionMessage::CustomPaste(text) => {
                if self.typing_custom {
                    self.typing_buffer
                        .insert_str(&super::components::flatten_newlines(&text));
                }
                None
            }
            QuestionMessage::CustomBackspace => self.edit_custom(InputBuffer::backspace),
            QuestionMessage::CustomDelete => self.edit_custom(InputBuffer::delete),
            QuestionMessage::CustomLeft => self.edit_custom(InputBuffer::left),
            QuestionMessage::CustomRight => self.edit_custom(InputBuffer::right),
            QuestionMessage::CustomHome => self.edit_custom(InputBuffer::home),
            QuestionMessage::CustomEnd => self.edit_custom(InputBuffer::end),
            QuestionMessage::CustomLeftWord => self.edit_custom(InputBuffer::left_word),
            QuestionMessage::CustomRightWord => self.edit_custom(InputBuffer::right_word),
            QuestionMessage::Escape => {
                if self.typing_custom {
                    self.typing_custom = false;
                    self.typing_buffer.clear();
                    return None;
                }
                let id = self.id;
                self.close();
                Some(QuestionEffect::Answer { id, answers: None })
            }
            QuestionMessage::Up => {
                if self.typing_custom || self.at_confirm {
                    return None;
                }
                self.cursor = self.cursor.saturating_sub(1);
                None
            }
            QuestionMessage::Down => {
                if self.typing_custom || self.at_confirm {
                    return None;
                }
                let rows = self.row_count(self.current);
                self.cursor = (self.cursor + 1).min(rows.saturating_sub(1));
                None
            }
            QuestionMessage::Prev => {
                if self.typing_custom {
                    return None;
                }
                if self.at_confirm {
                    self.at_confirm = false;
                    self.current = self.questions.len().saturating_sub(1);
                    self.cursor = 0;
                    return None;
                }
                if self.current > 0 {
                    self.current -= 1;
                    self.cursor = 0;
                }
                None
            }
            QuestionMessage::Next => self.toggle(true),
            QuestionMessage::Toggle => self.toggle(false),
            QuestionMessage::Number(n) => {
                if self.typing_custom || self.at_confirm {
                    return None;
                }
                let rows = self.row_count(self.current);
                if n < rows {
                    self.cursor = n;
                    return self.toggle(false);
                }
                None
            }
        }
    }

    fn edit_custom(&mut self, edit: fn(&mut InputBuffer)) -> Option<QuestionEffect> {
        if self.typing_custom {
            edit(&mut self.typing_buffer);
        }
        None
    }

    fn toggle(&mut self, from_nav: bool) -> Option<QuestionEffect> {
        if self.typing_custom {
            if from_nav {
                return None;
            }
            let text = self.typing_buffer.value.trim().to_string();
            if !text.is_empty() {
                self.custom_answers[self.current] = Some(text);
                self.typing_custom = false;
                self.typing_buffer.clear();
                return self.advance_after_answer();
            }
            return None;
        }
        if self.at_confirm {
            if from_nav {
                return None;
            }
            if self.all_answered() {
                let id = self.id;
                let answers = self.build_answers();
                self.close();
                return Some(QuestionEffect::Answer {
                    id,
                    answers: Some(answers),
                });
            }
            return None;
        }
        if from_nav {
            if self.current + 1 < self.questions.len() {
                self.current += 1;
                self.cursor = 0;
                return None;
            }
            if self.all_answered() {
                if self.has_confirm_view() {
                    self.at_confirm = true;
                } else {
                    let id = self.id;
                    let answers = self.build_answers();
                    self.close();
                    return Some(QuestionEffect::Answer {
                        id,
                        answers: Some(answers),
                    });
                }
            }
            return None;
        }
        let q = &self.questions[self.current];
        let rows = self.row_count(self.current);
        if self.cursor >= rows {
            return None;
        }
        let custom_idx = q.options.len();
        if self.cursor == custom_idx && q.custom {
            self.typing_custom = true;
            self.typing_buffer.clear();
            return None;
        }
        if q.multiple {
            if !self.picked[self.current].remove(&self.cursor) {
                self.picked[self.current].insert(self.cursor);
            }
            return None;
        }
        self.picked[self.current].clear();
        self.picked[self.current].insert(self.cursor);
        self.advance_after_answer()
    }

    /// Height the question UI wants at the given content width (matches the
    /// TextArea's symmetric(2, 1) padding contract).
    pub fn desired_height(&self, width: usize) -> u16 {
        let lines = self.build_lines(width.max(10) as u16, usize::MAX).len();
        (lines.min(MAX_VIEW_ROWS) as u16) + 2
    }

    pub fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if !self.open {
            return;
        }
        let block = Block::new()
            .bg(theme::surface())
            .padding(Padding::symmetric(2, 1));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let lines = self.build_lines(inner.width, inner.height as usize);
        frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), inner);
    }

    fn build_lines(&self, width: u16, max_rows: usize) -> Vec<Line<'static>> {
        let width = width.max(10);
        if self.at_confirm {
            return self.confirm_lines();
        }
        let Some(q) = self.questions.get(self.current) else {
            return Vec::new();
        };
        let progress = if self.questions.len() > 1 {
            format!("{} / {}  ", self.current + 1, self.questions.len())
        } else {
            String::new()
        };
        let mut lines = vec![
            Line::from(vec![
                Span::raw("▸ ").fg(theme::accent()),
                Span::raw(format!("{progress}{}", q.header))
                    .fg(theme::accent())
                    .bold(),
            ]),
            truncate_line(&q.question, width).style(Style::new().fg(theme::text()).italic()),
            Line::from(""),
        ];
        if self.typing_custom {
            lines.push(
                self.typing_buffer
                    .cursor_line(theme::text(), theme::accent()),
            );
            lines.push(theme::help_line(&[
                ("←/→", "move"),
                ("Enter", "save"),
                ("Esc", "cancel"),
            ]));
            return lines;
        }
        for (i, option) in q.options.iter().enumerate() {
            if lines.len() + 2 > max_rows {
                break;
            }
            let picked = self.picked[self.current].contains(&i);
            let marker = if q.multiple {
                if picked { "[x] " } else { "[ ] " }
            } else if picked {
                "● "
            } else {
                "  "
            };
            let number = format!("{} ", i + 1);
            let mut spans = Vec::new();
            if self.cursor == i {
                spans.push(Span::raw("▶ ").fg(theme::accent()));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::raw(marker).fg(if picked {
                theme::success()
            } else {
                theme::text_muted()
            }));
            spans.push(Span::raw(number).fg(theme::text_muted()));
            spans.push(Span::raw(option.label.clone()).fg(theme::text()));
            lines.push(Line::from(spans));
            if !option.description.is_empty() && lines.len() < max_rows {
                lines.push(
                    truncate_line(&format!("     {}", option.description), width)
                        .style(Style::new().fg(theme::text_muted())),
                );
            }
        }
        if q.custom && lines.len() < max_rows {
            let saved = self.custom_answers[self.current].clone();
            let selected = self.cursor == q.options.len();
            let mut spans = Vec::new();
            if selected {
                spans.push(Span::raw("▶ ").fg(theme::accent()));
            } else {
                spans.push(Span::raw("  "));
            }
            spans.push(Span::raw("✎ ").fg(theme::text_muted()));
            match saved {
                Some(text) => {
                    spans.push(Span::raw("✓ ").fg(theme::success()));
                    spans.push(truncate_span(&text, width.saturating_sub(6), theme::text()));
                }
                None => {
                    spans.push(Span::raw("Type your own answer").fg(theme::text_dim()));
                }
            }
            lines.push(Line::from(spans));
        }
        if lines.len() < max_rows {
            let hints: &[(&str, &str)] = if self.questions.len() > 1 {
                &[
                    ("↑/↓", "choose"),
                    ("←/→", "question"),
                    ("Enter", "pick/toggle"),
                    ("Esc", "dismiss"),
                ]
            } else if q.multiple {
                &[
                    ("↑/↓", "choose"),
                    ("Enter", "toggle"),
                    ("→", "done"),
                    ("Esc", "dismiss"),
                ]
            } else {
                &[("↑/↓", "choose"), ("Enter", "pick"), ("Esc", "dismiss")]
            };
            lines.push(theme::help_line(hints));
        }
        lines
    }

    fn confirm_lines(&self) -> Vec<Line<'static>> {
        let ready = self.all_answered();
        let mut lines = vec![Line::from(
            Span::raw("Confirm answers").fg(theme::accent()).bold(),
        )];
        for (i, q) in self.questions.iter().enumerate() {
            let answer = if let Some(text) = &self.custom_answers[i] {
                text.clone()
            } else {
                let labels: Vec<String> = self.picked[i]
                    .iter()
                    .filter_map(|&idx| q.options.get(idx).map(|o| o.label.clone()))
                    .collect();
                labels.join(", ")
            };
            let answered = !answer.is_empty();
            lines.push(Line::from(vec![
                Span::raw(if answered { "  ✓ " } else { "  ⚠ " }).fg(if answered {
                    theme::success()
                } else {
                    theme::warning()
                }),
                Span::raw(format!("{}: ", q.header)).fg(theme::text()),
                Span::raw(if answered {
                    answer
                } else {
                    "unanswered".into()
                })
                .fg(if answered {
                    theme::text_dim()
                } else {
                    theme::warning()
                }),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(if ready { "▶ " } else { "  " }).fg(theme::accent()),
            Span::raw("Confirm").fg(if ready {
                theme::text()
            } else {
                theme::text_muted()
            }),
        ]));
        lines
    }
}

fn ctrl_mod(key: &KeyEvent) -> bool {
    key.modifiers.contains(Modifiers::CONTROL)
}

fn truncate_line(text: &str, width: u16) -> Line<'static> {
    Line::from(truncate_span(text, width.saturating_sub(1), theme::text()))
}

fn truncate_span(text: &str, width: u16, color: Color) -> Span<'static> {
    let limited: String = take_chars(text, width as usize);
    Span::raw(limited).fg(color)
}

fn take_chars(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    text.chars().take(max).collect()
}
