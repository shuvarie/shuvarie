use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::prelude::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use crate::{diff, syntax, table, theme};

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
        .map(|l| Line::from(Span::raw(l.to_string()).style(theme::plain())))
        .collect()
}

pub fn render(text: &str) -> Vec<Line<'static>> {
    trim_trailing_blanks(render_pass(text).lines)
}

/// The dimmed flavor for thinking text: prose renders in the dim italic
/// reasoning style, markers and inline code stay quiet, and code blocks keep
/// their normal syntax colors.
pub fn render_dim(text: &str) -> Vec<Line<'static>> {
    trim_trailing_blanks(render_pass_dim(text).lines)
}

fn trim_trailing_blanks(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while lines.last().is_some_and(is_blank_line) {
        lines.pop();
    }
    lines
}

/// A resumable markdown render: the rendered lines plus, for every safe
/// top-level block boundary, the byte offset in the input where the block
/// ends and the line count reached at that point. A boundary is safe when a
/// fresh parse of `text[boundary..]` renders the remaining text identically
/// to the whole-input parse — guaranteed once a blank line separates the
/// boundary from whatever follows, or once a single-line block (heading,
/// rule) has consumed its own line break; a list never commits alone, since
/// a later item can still loosen it, and only rides along with a following
/// block's boundary. Streaming blocks commit at the last boundary and
/// re-render only the open region after it on each append.
pub struct MdPass {
    pub lines: Vec<Line<'static>>,
    pub boundaries: Vec<(usize, usize)>,
}

pub fn render_pass(text: &str) -> MdPass {
    pass_with(text, false)
}

pub fn render_pass_dim(text: &str) -> MdPass {
    pass_with(text, true)
}

fn pass_with(text: &str, dim: bool) -> MdPass {
    let mut ctx = Ctx {
        dim,
        ..Ctx::default()
    };
    let mut boundaries: Vec<(usize, usize)> = Vec::new();
    let mut depth = 0usize;
    for (event, range) in Parser::new_ext(text, OPTIONS).into_offset_iter() {
        match event {
            Event::Start(tag) => {
                ctx.start(tag);
                depth += 1;
            }
            Event::End(tag_end) => {
                ctx.end(tag_end);
                depth -= 1;
                if depth == 0 && boundary_is_stable(text, tag_end, range.end) {
                    boundaries.push((range.end, ctx.lines.len()));
                }
            }
            Event::Text(t) => ctx.text(t.as_ref().to_string()),
            Event::Code(c) => ctx.inline_code(c.as_ref().to_string()),
            Event::InlineHtml(h) | Event::Html(h) => ctx.inline_html(h.as_ref().to_string()),
            Event::InlineMath(m) | Event::DisplayMath(m) => ctx.text(m.as_ref().to_string()),
            Event::SoftBreak => ctx.soft_break(),
            Event::HardBreak => ctx.hard_break(),
            Event::Rule => {
                ctx.rule();
                if depth == 0 && line_terminated(&text[..range.end]) {
                    boundaries.push((range.end, ctx.lines.len()));
                }
            }
            Event::TaskListMarker(checked) => ctx.task_marker(checked),
            Event::FootnoteReference(_) => {}
        }
    }
    ctx.flush_final();
    MdPass {
        lines: ctx.lines,
        boundaries,
    }
}

/// Whether a top-level block ending at `end` cannot be changed retroactively
/// by more text: a heading only once its own line break has been consumed —
/// an unterminated heading line still grows with the next delta and its
/// frozen half would re-parse as a paragraph — every other block only once a
/// blank line follows, for the same reason a rule must not commit before its
/// line break (`---` can still degrade into a paragraph) and an unterminated
/// paragraph can still grow a setext underline or a table separator, a list
/// or quote can lazily continue, an unclosed fence closes only at EOF. A
/// genuinely closed fence is stable even without a blank line. A list is
/// never stable on its own: any later same-marker item or indented
/// continuation line joins it across the blank and loosens it (items become
/// paragraph-wrapped, gaining blank lines), so it only commits implicitly
/// when a following block's boundary takes the prefix.
fn boundary_is_stable(text: &str, tag: TagEnd, end: usize) -> bool {
    match tag {
        TagEnd::Heading(_) => line_terminated(&text[..end]),
        TagEnd::CodeBlock => fence_closed(&text[..end]) || followed_by_blank(text, end),
        TagEnd::List(_) => false,
        _ => followed_by_blank(text, end),
    }
}

/// Whether a block's range includes its own terminating line break:
/// pulldown-cmark ends a heading or rule range after the line break when it
/// has been seen and at the buffer's end when it has not, so a range that
/// stops mid-line still grows with more input.
fn line_terminated(head: &str) -> bool {
    head.ends_with('\n')
}

/// Whether `head` ends with a genuinely closed code fence: walking its
/// lines, the last fence opener must have been closed by a line carrying at
/// least as many of the opener's fence character. This mirrors CommonMark's
/// fence rules (up to three spaces of indent, a closing line may not be
/// shorter, info strings open, an unclosed fence closes only at EOF).
fn fence_closed(head: &str) -> bool {
    let mut opener: Option<(u8, usize)> = None;
    for line in head.lines() {
        let bytes = line.as_bytes();
        let mut indent = 0usize;
        while indent < bytes.len() && matches!(bytes[indent], b' ' | b'\t') {
            indent += 1;
        }
        if indent > 3 {
            continue;
        }
        let mut end = bytes.len();
        while end > indent && matches!(bytes[end - 1], b' ' | b'\t' | b'\r') {
            end -= 1;
        }
        let body = &bytes[indent..end];
        let Some(&first) = body.first() else {
            continue;
        };
        if first != b'`' && first != b'~' {
            continue;
        }
        let count = body.iter().take_while(|&&c| c == first).count();
        if count < 3 {
            continue;
        }
        let rest_ws_only = body[count..].iter().all(|&c| c == b' ' || c == b'\t');
        opener = match opener {
            None => {
                if first == b'`' && !rest_ws_only && body[count..].contains(&b'`') {
                    continue;
                }
                Some((first, count))
            }
            Some((oc, ocount)) if first == oc && count >= ocount && rest_ws_only => None,
            Some(open) => Some(open),
        };
    }
    opener.is_none()
}

/// Whether a complete blank line (ws-only, with its own line break) follows
/// the byte offset `end`: either right after it, or inside the trailing gap
/// the block's range already swallowed.
fn followed_by_blank(text: &str, end: usize) -> bool {
    blank_ahead(&text[end..]) || blank_behind(&text[..end])
}

/// Whether the first line of `rest` (everything after the block's range,
/// which already includes the block's own line break) is blank and carries
/// its own line break: a ws-only line before the next `\n`.
fn blank_ahead(rest: &str) -> bool {
    match rest.find('\n') {
        Some(k) => rest[..k].bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r')),
        None => false,
    }
}

/// Whether the whitespace tail of `head` (between the last non-ws byte and
/// the end) contains a complete blank line: a `\n`, then only whitespace,
/// then another `\n`.
fn blank_behind(head: &str) -> bool {
    let gap = &head[head.trim_end().len()..];
    let bytes = gap.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if byte != b'\n' {
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\r') {
            j += 1;
        }
        if bytes.get(j) == Some(&b'\n') {
            return true;
        }
    }
    false
}

/// An in-flight `![alt](url)`: the alt text is buffered (its events arrive
/// between the image tag's start and end), and the URL is the fallback when
/// the alt is empty.
struct ImageChip {
    buf: String,
    url: String,
}

#[derive(Clone)]
enum ItemKind {
    Bullet,
    Ordered { next: u64 },
}

#[derive(Default)]
struct TableCtx {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    row: Vec<Vec<Span<'static>>>,
    cell: Vec<Span<'static>>,
}

struct CodeCtx {
    lang: String,
    buf: String,
    fenced: bool,
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
    image: Option<ImageChip>,
    code: Option<CodeCtx>,
    item_marker: Option<(String, Style)>,
    lists: Vec<ItemKind>,
    table: Option<TableCtx>,
    dim: bool,
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
                let fenced = matches!(kind, CodeBlockKind::Fenced(_));
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.as_ref().to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                if fenced {
                    let mut fence = Line::from(Span::raw("```").style(theme::fence()));
                    if !lang.is_empty() {
                        fence.push_span(Span::raw(lang.clone()).style(theme::fence_lang()));
                    }
                    self.lines.push(fence);
                }
                self.code = Some(CodeCtx {
                    lang,
                    buf: String::new(),
                    fenced,
                });
            }
            Tag::List(start) => {
                let kind = match start {
                    Some(n) => ItemKind::Ordered { next: n },
                    None => ItemKind::Bullet,
                };
                self.lists.push(kind);
            }
            Tag::Item => {
                let text = match self.lists.last_mut() {
                    Some(ItemKind::Bullet) => "• ".to_string(),
                    Some(ItemKind::Ordered { next }) => {
                        let label = format!("{next}. ");
                        *next += 1;
                        label
                    }
                    None => String::new(),
                };
                let style = if self.dim {
                    Style::new().fg(theme::text_dim())
                } else {
                    Style::new()
                        .fg(theme::accent())
                        .add_modifier(Modifier::BOLD)
                };
                self.item_marker = Some((text, style));
            }
            Tag::Table(alignments) => {
                self.table = Some(TableCtx {
                    alignments,
                    ..TableCtx::default()
                });
            }
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
            Tag::Image { dest_url, .. } => {
                self.image = Some(ImageChip {
                    buf: String::new(),
                    url: dest_url.as_ref().to_string(),
                });
            }
            Tag::HtmlBlock | Tag::FootnoteDefinition(_) | Tag::MetadataBlock(_) => {
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
                if let Some(code) = self.code.take() {
                    let mut block = if syntax::is_diff_lang(&code.lang) {
                        diff::highlight_diff(&code.buf)
                    } else {
                        syntax::highlight_code(&code.lang, &code.buf)
                    };
                    self.lines.append(&mut block);
                    if code.fenced {
                        self.lines
                            .push(Line::from(Span::raw("```").style(theme::fence())));
                    }
                    self.lines.push(Line::from(""));
                }
            }
            TagEnd::Paragraph => self.flush_line(true),
            TagEnd::Item => self.flush_line(false),
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
                if let Some(t) = self.table.take() {
                    self.lines
                        .append(&mut table::render(&t.alignments, &t.rows));
                    self.lines.push(Line::from(""));
                }
            }
            TagEnd::Emphasis => self.emph = self.emph.saturating_sub(1),
            TagEnd::Strong => self.strong = self.strong.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::Image => {
                if self.skip > 0 {
                    self.skip = self.skip.saturating_sub(1);
                    return;
                }
                if let Some(chip) = self.image.take() {
                    self.image_chip(chip);
                }
            }
            TagEnd::HtmlBlock | TagEnd::FootnoteDefinition | TagEnd::MetadataBlock(_) => {
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
        if let Some(code) = &mut self.code {
            code.buf.push_str(&text);
            return;
        }
        if let Some(chip) = &mut self.image {
            chip.buf.push_str(&text);
            return;
        }
        self.spans.push(Span::raw(text).style(self.inline_style()));
    }

    fn inline_code(&mut self, code: String) {
        if self.skip > 0 {
            return;
        }
        if let Some(chip) = &mut self.image {
            chip.buf.push_str(&code);
            return;
        }
        let style = if self.dim {
            theme::reasoning()
        } else {
            Style::new().fg(theme::text()).bg(theme::accent_bg())
        };
        self.spans.push(Span::raw(code).style(style));
    }

    fn inline_html(&mut self, html: String) {
        if self.skip > 0 {
            return;
        }
        self.spans.push(Span::raw(html).fg(theme::text_dim()));
    }

    fn soft_break(&mut self) {
        if self.skip > 0 {
            return;
        }
        if let Some(chip) = &mut self.image {
            chip.buf.push(' ');
            return;
        }
        self.flush_line(false);
    }

    fn hard_break(&mut self) {
        if self.skip > 0 {
            return;
        }
        if let Some(chip) = &mut self.image {
            chip.buf.push(' ');
            return;
        }
        self.flush_line(false);
    }

    fn rule(&mut self) {
        if self.skip > 0 {
            return;
        }
        self.flush_line(false);
        self.lines.push(Line::from(
            Span::raw("─".repeat(60)).fg(theme::text_muted()),
        ));
        self.lines.push(Line::from(""));
    }

    fn task_marker(&mut self, checked: bool) {
        let text = if checked { "[x] " } else { "[ ] " }.to_string();
        let style = if self.dim {
            Style::new().fg(theme::text_dim())
        } else {
            Style::new().fg(if checked {
                theme::success()
            } else {
                theme::text_muted()
            })
        };
        self.item_marker = Some((text, style));
    }

    fn inline_style(&self) -> Style {
        if self.dim {
            let mut style = theme::reasoning();
            if self.heading.is_some() || self.strong > 0 {
                style = style.add_modifier(Modifier::BOLD);
            }
            if self.link > 0 {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            if self.strike > 0 {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            return style;
        }
        let mut style = Style::new().fg(theme::text());
        if let Some(level) = self.heading {
            if level == HeadingLevel::H1 || level == HeadingLevel::H2 {
                style = style.fg(theme::accent());
            }
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.link > 0 {
            style = style
                .fg(theme::code_type())
                .add_modifier(Modifier::UNDERLINED);
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

    /// Renders the buffered `![alt](url)` as a chip: a muted glyph, then the
    /// alt text — or, when the alt is empty, the URL. A linked image keeps
    /// the link styling and dim prose keeps the reasoning style via
    /// [`Self::inline_style`]; the glyph itself stays muted in both.
    fn image_chip(&mut self, chip: ImageChip) {
        self.spans.push(Span::raw("🖼 ").fg(theme::text_muted()));
        let alt = chip.buf.trim();
        if !alt.is_empty() {
            self.spans
                .push(Span::raw(alt.to_string()).style(self.inline_style()));
            return;
        }
        let url = chip.url.trim();
        if !url.is_empty() {
            self.spans
                .push(Span::raw(url.to_string()).fg(theme::text_muted()));
        }
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
            spans.push(Span::raw("│ ").fg(theme::text_muted()));
        }
        spans.append(&mut self.spans);
        self.lines.push(Line::from(spans));
        if blank_after {
            self.lines.push(Line::from(""));
        }
    }

    fn flush_final(&mut self) {
        self.flush_line(true);
    }
}

fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.is_empty())
}
