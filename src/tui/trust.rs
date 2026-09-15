use std::io::{self, Write};
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use ratatui::prelude::*;
use ratatui::widgets::{Clear, List, ListItem, Paragraph};
use termina::event::{Event, KeyCode, KeyEventKind, Modifiers};
use termina::{EventStream, PlatformTerminal, Terminal};

use shuvarie_core::{Category, TrustGrants, WorkspaceScan};

use super::add_provider::centered_rect;
use super::{escape, list, theme};

/// How the trust prompt ended.
pub enum TrustPromptOutcome {
    /// Confirm the checked categories (recorded by the caller).
    Grant(TrustGrants),
    /// Reject for this session only: nothing is recorded.
    Skip,
    /// Quit the program.
    Quit,
}

/// The one-screen workspace trust prompt, drawn directly on the terminal
/// before the TUI starts. All categories start checked; `Enter` confirms the
/// checked ones, `Esc` rejects for this session only. With `new_items` the
/// prompt only presents the candidates that appeared after the workspace's
/// recorded decision.
pub async fn run_prompt(
    scan: &WorkspaceScan,
    cwd: &Path,
    new_items: bool,
) -> io::Result<TrustPromptOutcome> {
    let mut model = TrustPrompt::new(scan, cwd, new_items);
    let mut term = PlatformTerminal::new()?;
    term.enter_raw_mode()?;

    let reader = term.event_reader();
    let mut stream = EventStream::new(reader, |_| true);
    let mut rat = ratatui::Terminal::new(ratatui::prelude::TerminaBackend::new(term))?;

    write!(
        rat.backend_mut().terminal_mut(),
        "{}",
        escape::ENTER_ALTERNATE_SCREEN
    )?;
    rat.backend_mut().terminal_mut().flush()?;

    let outcome = prompt_loop(&mut model, &mut rat, &mut stream).await;

    let term = rat.backend_mut().terminal_mut();
    write!(term, "{}", escape::EXIT_ALTERNATE_SCREEN)?;
    term.flush()?;
    term.enter_cooked_mode()?;

    outcome
}

async fn prompt_loop(
    model: &mut TrustPrompt,
    rat: &mut ratatui::Terminal<ratatui::prelude::TerminaBackend<termina::PlatformTerminal>>,
    stream: &mut EventStream,
) -> io::Result<TrustPromptOutcome> {
    loop {
        rat.draw(|frame| model.view(frame, frame.area()))?;
        let Some(result) = stream.next().await else {
            return Ok(TrustPromptOutcome::Quit);
        };
        let Event::Key(key) = result? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('d')
                if key.modifiers.contains(Modifiers::CONTROL) =>
            {
                return Ok(TrustPromptOutcome::Quit);
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Left => model.move_up(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Right => model.move_down(),
            KeyCode::Char(' ') => model.toggle(),
            KeyCode::Char('a') => model.check_all(),
            KeyCode::Char('n') => model.check_none(),
            KeyCode::Enter => return Ok(TrustPromptOutcome::Grant(model.grants())),
            KeyCode::Escape => return Ok(TrustPromptOutcome::Skip),
            KeyCode::Char('q') => return Ok(TrustPromptOutcome::Quit),
            _ => {}
        }
    }
}

pub struct TrustPrompt {
    cwd: PathBuf,
    items: Vec<(Category, String)>,
    checked: Vec<bool>,
    selected: usize,
    /// A follow-up prompt: only the candidates that appeared after the
    /// workspace's recorded decision.
    new_items: bool,
}

impl TrustPrompt {
    pub fn new(scan: &WorkspaceScan, cwd: &Path, new_items: bool) -> Self {
        let items: Vec<(Category, String)> = scan
            .items
            .iter()
            .map(|item| (item.category, item.detail.clone()))
            .collect();
        let checked = vec![true; items.len()];
        Self {
            cwd: cwd.to_path_buf(),
            items,
            checked,
            selected: 0,
            new_items,
        }
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn move_down(&mut self) {
        self.selected = (self.selected + 1).min(self.items.len().saturating_sub(1));
    }

    fn toggle(&mut self) {
        if let Some(checked) = self.checked.get_mut(self.selected) {
            *checked = !*checked;
        }
    }

    fn check_all(&mut self) {
        self.checked.iter_mut().for_each(|c| *c = true);
    }

    fn check_none(&mut self) {
        self.checked.iter_mut().for_each(|c| *c = false);
    }

    fn grants(&self) -> TrustGrants {
        TrustGrants::from_categories(
            self.items
                .iter()
                .zip(&self.checked)
                .filter(|(_, checked)| **checked)
                .map(|((category, _), _)| *category),
        )
    }

    fn view(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.items.is_empty() {
            return;
        }
        let popup = centered_rect(64, 44, area);
        frame.render_widget(Clear, popup);
        let title = if self.new_items {
            "Workspace trust — new files"
        } else {
            "Workspace trust"
        };
        let block = theme::overlay_block(title);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let label_width = self
            .items
            .iter()
            .map(|(category, _)| label(*category).len())
            .max()
            .unwrap_or(0);

        let mut lines: Vec<ListItem> = vec![
            ListItem::new(Line::from(
                Span::raw(self.cwd.to_string_lossy().into_owned()).fg(theme::TEXT_MUTED),
            )),
            ListItem::new(Line::from("")),
        ];
        for (i, (category, detail)) in self.items.iter().enumerate() {
            let checked = self.checked[i];
            let box_glyph: &str = if checked { "[x]" } else { "[ ]" };
            let line = Line::from(vec![
                Span::raw(format!("{box_glyph} ")).fg(if checked {
                    theme::ACCENT
                } else {
                    theme::TEXT_MUTED
                }),
                Span::raw(format!("{:<label_width$}", label(*category))).fg(theme::TEXT),
                Span::raw("  ").fg(theme::TEXT),
                Span::raw(detail.clone()).fg(theme::TEXT_MUTED),
            ]);
            lines.push(list::render_list_item_line(line, i == self.selected));
        }
        lines.push(ListItem::new(Line::from("")));
        let list = List::new(lines);
        frame.render_widget(list, inner);

        let help = theme::help_line(&[
            ("↑↓", "select"),
            ("Space", "toggle"),
            ("a", "trust all"),
            ("n", "none"),
            ("Enter", "confirm"),
            ("Esc", "skip (session only)"),
        ]);
        frame.render_widget(
            Paragraph::new(help).fg(theme::TEXT_MUTED),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
    }
}

fn label(category: Category) -> &'static str {
    match category {
        Category::Contexts => "Context files",
        Category::Skills => "Skills",
        Category::Configs => "Config",
    }
}
