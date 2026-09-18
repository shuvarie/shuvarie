use ratatui::layout::Alignment;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Padding, Paragraph};
use shuvarie_core::QuestionPrompt;
use termina::event::{KeyCode, KeyEvent, Modifiers};

use super::components::InputBuffer;
use super::theme;
use super::utils::text::wrap_text;

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

/// Left indent of the description rows under an option label.
const DESC_INDENT: usize = 5;

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
                // The picked marker moves to the custom answer: drop any
                // picked options so the saved text is the only marked row.
                self.picked[self.current].clear();
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
            // Re-editing a saved custom answer: preload it so it can be
            // edited instead of retyped.
            let saved = self.custom_answers[self.current].clone();
            if let Some(text) = saved {
                self.typing_buffer.insert_str(&text);
            }
            return None;
        }
        if q.multiple {
            if !self.picked[self.current].remove(&self.cursor) {
                self.picked[self.current].insert(self.cursor);
            }
            if !self.picked[self.current].is_empty() {
                self.custom_answers[self.current] = None;
            }
            return None;
        }
        self.picked[self.current].clear();
        self.picked[self.current].insert(self.cursor);
        self.custom_answers[self.current] = None;
        self.advance_after_answer()
    }

    /// Height the question UI wants at the given content width: measured at
    /// the inner width the symmetric(2, 1) padded block paints at.
    pub fn desired_height(&self, width: usize) -> u16 {
        let inner = (width.max(10) as u16).saturating_sub(4);
        let lines = self.build_lines(inner, usize::MAX).len();
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
            if !option.description.is_empty() {
                let desc_width = (width as usize).saturating_sub(DESC_INDENT);
                for row in wrap_text(&option.description, desc_width) {
                    if lines.len() >= max_rows {
                        break;
                    }
                    lines.push(Line::from(
                        Span::raw(format!("{}{row}", " ".repeat(DESC_INDENT)))
                            .fg(theme::text_muted()),
                    ));
                }
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
                    // A saved custom answer is the picked row: the green dot
                    // (or checkbox) lives here instead of on any option.
                    let marker = if q.multiple { "[x] " } else { "● " };
                    let used = 4 + marker.chars().count();
                    spans.push(Span::raw(marker).fg(theme::success()));
                    spans.push(truncate_span(
                        &text,
                        width.saturating_sub(used as u16),
                        theme::text(),
                    ));
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

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_core::question::QuestionOption;

    fn ui(descriptions: &[&str]) -> QuestionUI {
        let mut ui = QuestionUI::new();
        ui.open(
            1,
            vec![QuestionPrompt {
                question: "Which one?".into(),
                header: "Pick".into(),
                options: descriptions
                    .iter()
                    .map(|d| QuestionOption {
                        label: "label".into(),
                        description: (*d).into(),
                    })
                    .collect(),
                multiple: false,
                custom: false,
            }],
        );
        ui
    }

    fn custom_ui(multiple: bool, count: usize) -> QuestionUI {
        let mut ui = QuestionUI::new();
        ui.open(
            1,
            (0..count)
                .map(|_| QuestionPrompt {
                    question: "Which one?".into(),
                    header: "Pick".into(),
                    options: vec![QuestionOption {
                        label: "label".into(),
                        description: String::new(),
                    }],
                    multiple,
                    custom: true,
                })
                .collect(),
        );
        ui
    }

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    #[test]
    fn long_description_wraps_under_the_option() {
        let ui = ui(&["first row here second row here"]);
        let lines = ui.build_lines(30, usize::MAX);
        let texts = texts(&lines);
        assert_eq!(texts[4], "     first row here second row");
        assert_eq!(texts[5], "     here");
        assert_eq!(lines[4].spans[0].style.fg, Some(theme::text_muted()));
    }

    #[test]
    fn short_description_stays_one_row() {
        let ui = ui(&["a short note"]);
        let lines = ui.build_lines(40, usize::MAX);
        let texts = texts(&lines);
        assert_eq!(texts[4], "     a short note");
        assert_eq!(lines.len(), 6);
    }

    #[test]
    fn wrapped_rows_respect_max_rows() {
        let ui = ui(&["aaaa bbbb cccc dddd eeee ffff gggg hhhh"]);
        let lines = ui.build_lines(30, 6);
        let texts = texts(&lines);
        assert_eq!(lines.len(), 6);
        assert_eq!(texts[4], "     aaaa bbbb cccc dddd eeee");
        assert_eq!(texts[5], "     ffff gggg hhhh");
    }

    #[test]
    fn desired_height_measures_at_the_painted_inner_width() {
        let desc = format!("{} zzzz", "w".repeat(20));
        let ui = ui(&[&desc]);
        let inner_lines = ui.build_lines(26, usize::MAX).len();
        assert_eq!(inner_lines, 7);
        assert_eq!(
            ui.desired_height(30),
            (inner_lines.min(MAX_VIEW_ROWS) + 2) as u16
        );
    }

    #[test]
    fn newline_in_description_starts_a_new_row() {
        let ui = ui(&["one\ntwo"]);
        let texts = texts(&ui.build_lines(40, usize::MAX));
        assert_eq!(texts[4], "     one");
        assert_eq!(texts[5], "     two");
    }

    fn type_custom(ui: &mut QuestionUI, text: &str) {
        ui.update(QuestionMessage::Down);
        ui.update(QuestionMessage::Toggle);
        assert!(ui.typing_custom);
        for c in text.chars() {
            ui.update(QuestionMessage::CustomInput(c));
        }
        ui.update(QuestionMessage::Toggle);
        assert!(!ui.typing_custom);
    }

    #[test]
    fn saved_custom_answer_takes_the_picked_marker() {
        let mut ui = custom_ui(true, 1);
        ui.update(QuestionMessage::Toggle); // pick option 0
        type_custom(&mut ui, "hi");
        assert!(ui.picked[0].is_empty());
        assert_eq!(ui.custom_answers[0].as_deref(), Some("hi"));
        let texts = texts(&ui.build_lines(40, usize::MAX));
        assert_eq!(texts[3], "  [ ] 1 label");
        assert_eq!(texts[4], "▶ ✎ [x] hi");
    }

    #[test]
    fn saved_custom_answer_shows_the_green_dot() {
        let mut ui = custom_ui(false, 2);
        type_custom(&mut ui, "hi");
        let texts = texts(&ui.build_lines(40, usize::MAX));
        assert_eq!(texts[3], "    1 label");
        assert_eq!(texts[4], "▶ ✎ ● hi");
    }

    #[test]
    fn reselecting_custom_row_preloads_the_saved_answer() {
        let mut ui = custom_ui(true, 1);
        type_custom(&mut ui, "draft answer");
        ui.update(QuestionMessage::Toggle); // select the custom row again
        assert!(ui.typing_custom);
        assert_eq!(ui.typing_buffer.value, "draft answer");
        ui.update(QuestionMessage::Escape); // cancel keeps the save
        assert!(!ui.typing_custom);
        assert_eq!(ui.typing_buffer.value, "");
        assert_eq!(ui.custom_answers[0].as_deref(), Some("draft answer"));
    }

    #[test]
    fn picking_an_option_after_custom_clears_the_custom_answer() {
        let mut ui = custom_ui(false, 2);
        type_custom(&mut ui, "hi");
        ui.update(QuestionMessage::Up); // back to option 0
        ui.update(QuestionMessage::Toggle);
        assert!(ui.custom_answers[0].is_none());
        assert!(ui.picked[0].contains(&0));
        assert_eq!(ui.build_answers()[0], vec!["label".to_string()]);
        let texts = texts(&ui.build_lines(40, usize::MAX));
        assert_eq!(texts[3], "▶ ● 1 label");
        assert_eq!(texts[4], "  ✎ Type your own answer");
    }
}
