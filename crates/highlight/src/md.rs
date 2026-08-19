use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::prelude::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use crate::{diff, syntax, theme};

pub const OPTIONS: Options = Options::all()
    .difference(Options::ENABLE_SMART_PUNCTUATION)
    .difference(Options::ENABLE_HEADING_ATTRIBUTES)
    .difference(Options::ENABLE_MATH)
    .difference(Options::ENABLE_OLD_FOOTNOTES)
    .difference(Options::ENABLE_SUPERSCRIPT)
    .difference(Options::ENABLE_SUBSCRIPT)
    .difference(Options::ENABLE_WIKILINKS)
    .difference(Options::ENABLE_DEFINITION_LIST);

pub fn plain(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| Line::from(Span::raw(l.to_string()).style(theme::PLAIN)))
        .collect()
}

pub fn render(text: &str) -> Vec<Line<'static>> {
    let mut ctx = Ctx::default();
    for event in Parser::new_ext(text, OPTIONS) {
        match event {
            Event::Start(tag) => ctx.start(tag),
            Event::End(tag_end) => ctx.end(tag_end),
            Event::Text(t) => ctx.text(t.as_ref().to_string()),
            Event::Code(c) => ctx.inline_code(c.as_ref().to_string()),
            Event::InlineHtml(h) | Event::Html(h) => ctx.inline_html(h.as_ref().to_string()),
            Event::InlineMath(m) | Event::DisplayMath(m) => ctx.text(m.as_ref().to_string()),
            Event::SoftBreak => ctx.soft_break(),
            Event::HardBreak => ctx.hard_break(),
            Event::Rule => ctx.rule(),
            Event::TaskListMarker(checked) => ctx.task_marker(checked),
            Event::FootnoteReference(_) => {}
        }
    }
    ctx.finish()
}

#[derive(Clone)]
enum ItemKind {
    Bullet,
    Ordered { next: u64 },
}

#[derive(Default)]
struct TableCtx {
    rows: Vec<Vec<Vec<Span<'static>>>>,
    row: Vec<Vec<Span<'static>>>,
    cell: Vec<Span<'static>>,
}

#[derive(Default)]
struct Ctx {
    spans: Vec<Span<'static>>,
    lines: Vec<Line<'static>>,
    in_quote: bool,
    heading: Option<HeadingLevel>,
    emph: u32,
    strong: u32,
    strike: u32,
    link: u32,
    skip: u32,
    code: Option<(String, String)>,
    item_marker: Option<(String, Style)>,
    lists: Vec<ItemKind>,
    table: Option<TableCtx>,
}

impl Ctx {
    fn start(&mut self, tag: Tag<'_>) {
        if self.skip > 0 {
            self.skip += 1;
            return;
        }
        match tag {
            Tag::Heading { level, .. } => self.heading = Some(level),
            Tag::BlockQuote(_) => self.in_quote = true,
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.as_ref().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                let kind = match start {
                    Some(n) => ItemKind::Ordered { next: n },
                    None => ItemKind::Bullet,
                };
                self.lists.push(kind);
            }
            Tag::Item => {
                let (text, style) = match self.lists.last_mut() {
                    Some(ItemKind::Bullet) => (
                        "• ".to_string(),
                        Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Some(ItemKind::Ordered { next }) => {
                        let label = format!("{next}. ");
                        *next += 1;
                        (
                            label,
                            Style::new().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
                        )
                    }
                    None => (String::new(), Style::new()),
                };
                self.item_marker = Some((text, style));
            }
            Tag::Table(_) => self.table = Some(TableCtx::default()),
            Tag::TableHead => {
                if let Some(t) = &mut self.table {
                    t.row = Vec::new();
                }
            }
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.row = Vec::new();
                }
            }
            Tag::TableCell => {
                if let Some(t) = &mut self.table {
                    t.cell = Vec::new();
                }
            }
            Tag::Emphasis => self.emph += 1,
            Tag::Strong => self.strong += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link += 1,
            Tag::Image { .. }
            | Tag::HtmlBlock
            | Tag::FootnoteDefinition(_)
            | Tag::MetadataBlock(_) => {
                self.skip = 1;
            }
            Tag::Paragraph
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition
            | Tag::Superscript
            | Tag::Subscript => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::CodeBlock => {
                if let Some((lang, code)) = self.code.take() {
                    let mut block = if syntax::is_diff_lang(&lang) {
                        diff::highlight_diff(&code)
                    } else {
                        syntax::highlight_code(&lang, &code)
                    };
                    self.lines.append(&mut block);
                    self.lines.push(Line::from(""));
                }
            }
            TagEnd::Paragraph | TagEnd::Item => self.flush_line(false),
            TagEnd::Heading(_) => {
                self.heading = None;
                self.flush_line(true);
            }
            TagEnd::List(_) => {
                self.lists.pop();
                self.flush_line(false);
            }
            TagEnd::BlockQuote(_) => self.in_quote = false,
            TagEnd::TableCell => {
                if let Some(t) = &mut self.table {
                    t.cell.append(&mut self.spans);
                    t.row.push(t.cell.clone());
                    t.cell.clear();
                }
            }
            TagEnd::TableHead => {
                if let Some(t) = &mut self.table {
                    t.rows.push(t.row.clone());
                    t.row.clear();
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = &mut self.table {
                    t.rows.push(t.row.clone());
                    t.row.clear();
                }
            }
            TagEnd::Table => {
                let t = self.table.take();
                if let Some(t) = t {
                    for (ri, row) in t.rows.iter().enumerate() {
                        let mut spans: Vec<Span<'static>> = Vec::new();
                        for (ci, cell) in row.iter().enumerate() {
                            if ci > 0 {
                                spans.push(Span::raw(" ┃ ").fg(theme::TEXT_MUTED));
                            }
                            let cell = if ri == 0 {
                                cell.iter().map(|s| s.clone().bold()).collect::<Vec<_>>()
                            } else {
                                cell.clone()
                            };
                            spans.extend(cell);
                        }
                        if !spans.is_empty() {
                            self.lines.push(Line::from(spans));
                        }
                    }
                    self.lines.push(Line::from(""));
                }
            }
            TagEnd::Emphasis => self.emph = self.emph.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::Image
            | TagEnd::HtmlBlock
            | TagEnd::FootnoteDefinition
            | TagEnd::MetadataBlock(_) => {
                self.skip = self.skip.saturating_sub(1);
            }
            TagEnd::DefinitionList
            | TagEnd::DefinitionListTitle
            | TagEnd::DefinitionListDefinition
            | TagEnd::Superscript
            | TagEnd::Subscript => {}
        }
    }

    fn text(&mut self, text: String) {
        if self.skip > 0 {
            return;
        }
        if let Some((_, buf)) = &mut self.code {
            buf.push_str(&text);
            return;
        }
        self.spans.push(Span::raw(text).style(self.inline_style()));
    }

    fn inline_code(&mut self, code: String) {
        if self.skip > 0 {
            return;
        }
        self.spans
            .push(Span::raw(code).style(Style::new().fg(theme::TEXT).bg(theme::ACCENT_BG)));
    }

    fn inline_html(&mut self, html: String) {
        if self.skip > 0 {
            return;
        }
        self.spans.push(Span::raw(html).fg(theme::TEXT_DIM));
    }

    fn soft_break(&mut self) {
        if self.skip > 0 || self.code.is_some() {
            return;
        }
        self.spans.push(Span::raw(" ").fg(theme::TEXT));
    }

    fn hard_break(&mut self) {
        if self.skip > 0 {
            return;
        }
        self.flush_line(false);
    }

    fn rule(&mut self) {
        if self.skip > 0 {
            return;
        }
        self.flush_line(false);
        self.lines
            .push(Line::from(Span::raw("─".repeat(60)).fg(theme::TEXT_MUTED)));
        self.lines.push(Line::from(""));
    }

    fn task_marker(&mut self, checked: bool) {
        self.item_marker = Some((
            if checked { "[x] " } else { "[ ] " }.to_string(),
            Style::new().fg(if checked {
                theme::SUCCESS
            } else {
                theme::TEXT_MUTED
            }),
        ));
    }

    fn inline_style(&self) -> Style {
        let mut style = Style::new().fg(theme::TEXT);
        if let Some(level) = self.heading {
            if level == HeadingLevel::H1 || level == HeadingLevel::H2 {
                style = style.fg(theme::ACCENT);
            }
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.link > 0 {
            style = style.fg(theme::STEEL).add_modifier(Modifier::UNDERLINED);
        }
        if self.emph > 0 {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strong > 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.strike > 0 {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    fn flush_line(&mut self, blank_after: bool) {
        if self.spans.is_empty() {
            return;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();
        if let Some((marker, style)) = self.item_marker.take() {
            spans.push(Span::styled(marker, style));
        }
        if self.in_quote {
            spans.push(Span::raw("│ ").fg(theme::TEXT_MUTED));
        }
        spans.append(&mut self.spans);
        self.lines.push(Line::from(spans));
        if blank_after {
            self.lines.push(Line::from(""));
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush_line(true);
        self.lines
    }
}
