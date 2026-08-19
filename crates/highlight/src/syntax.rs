use std::str::FromStr;
use std::sync::LazyLock;

use ratatui::prelude::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::highlighting::{
    Color as SyntectColor, FontStyle, HighlightIterator, HighlightState, Highlighter,
    ScopeSelectors, Style as SyntectStyle, StyleModifier, Theme as SyntectTheme, ThemeItem,
    ThemeSettings,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxSet};

use crate::theme;

fn to_syntect_color(c: Color) -> SyntectColor {
    match c {
        Color::Rgb(r, g, b) => SyntectColor { r, g, b, a: 255 },
        _ => SyntectColor {
            r: 224,
            g: 216,
            b: 196,
            a: 255,
        },
    }
}

fn to_ratatui_color(c: SyntectColor) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

fn to_ratatui_style(fg: Color, font: FontStyle) -> Style {
    let mut style = Style::new().fg(fg);
    if font.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if font.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if font.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}

fn fg(color: Color) -> StyleModifier {
    StyleModifier {
        foreground: Some(to_syntect_color(color)),
        font_style: None,
        background: None,
    }
}

fn fg_italic(color: Color) -> StyleModifier {
    StyleModifier {
        foreground: Some(to_syntect_color(color)),
        font_style: Some(FontStyle::ITALIC),
        background: None,
    }
}

fn fg_bold(color: Color) -> StyleModifier {
    StyleModifier {
        foreground: Some(to_syntect_color(color)),
        font_style: Some(FontStyle::BOLD),
        background: None,
    }
}

fn rule(scopes: &str, style: StyleModifier) -> ThemeItem {
    ThemeItem {
        scope: ScopeSelectors::from_str(scopes).unwrap_or_default(),
        style,
    }
}

fn heraldic_theme() -> SyntectTheme {
    SyntectTheme {
        name: Some("shuvarie".to_string()),
        author: None,
        settings: ThemeSettings {
            foreground: Some(to_syntect_color(theme::TEXT)),
            background: None,
            caret: None,
            line_highlight: None,
            misspelling: None,
            minimap_border: None,
            accent: None,
            popup_css: None,
            phantom_css: None,
            bracket_contents_foreground: None,
            bracket_contents_options: None,
            brackets_foreground: None,
            brackets_background: None,
            brackets_options: None,
            tags_foreground: None,
            tags_options: None,
            highlight: None,
            find_highlight: None,
            find_highlight_foreground: None,
            gutter: None,
            gutter_foreground: None,
            selection: None,
            selection_foreground: None,
            selection_border: None,
            inactive_selection: None,
            inactive_selection_foreground: None,
            guide: None,
            active_guide: None,
            stack_guide: None,
            shadow: None,
        },
        scopes: vec![
            rule(
                "keyword, keyword.control, keyword.operator, keyword.other, storage.type.function, storage.modifier",
                fg(theme::ACCENT),
            ),
            rule(
                "constant.language, constant.numeric, constant.other.color, variable.language",
                fg(theme::AMBER),
            ),
            rule(
                "string, string.quoted, punctuation.definition.string, string.regexp, string.escape",
                fg(theme::SAGE),
            ),
            rule(
                "comment, comment.block.documentation",
                fg_italic(theme::TEXT_DIM),
            ),
            rule(
                "entity.name.type, entity.name.class, entity.name.struct, entity.name.namespace, support.type",
                fg(theme::STEEL),
            ),
            rule(
                "entity.name.function, support.function, meta.function-call, support.type.property-name",
                fg(theme::BRONZE),
            ),
            rule(
                "variable, variable.other, variable.parameter, entity.name.variable, meta.block",
                fg(theme::TEXT),
            ),
            rule(
                "entity.other.attribute-name, support.other.variable",
                fg(theme::WARNING),
            ),
            rule(
                "punctuation, punctuation.definition, punctuation.section, punctuation.separator, meta.delimiter, delimiter",
                fg(theme::TEXT_DIM),
            ),
            rule(
                "meta.tag, tag, tag.name, tag.structure, markup.tag",
                fg(theme::ACCENT),
            ),
            rule(
                "markup.heading, entity.name.section",
                fg_bold(theme::ACCENT),
            ),
            rule(
                "markup.quote, markup.underline.link, markup.raw.inline",
                fg(theme::TEXT_DIM),
            ),
            rule("invalid, invalid.illegal", fg(theme::ERROR)),
        ],
    }
}

fn shuvarie_theme() -> &'static SyntectTheme {
    static THEME: LazyLock<SyntectTheme> = LazyLock::new(heraldic_theme);
    &THEME
}

fn syntax_set() -> &'static SyntaxSet {
    static SET: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
    &SET
}

fn highlighter() -> &'static Highlighter<'static> {
    static HIGHLIGHTER: LazyLock<Highlighter<'static>> =
        LazyLock::new(|| Highlighter::new(shuvarie_theme()));
    &HIGHLIGHTER
}

fn highlight_line<'a>(
    state: &mut HighlightState,
    parse_state: &mut ParseState,
    set: &SyntaxSet,
    highlighter: &Highlighter<'_>,
    line: &'a str,
) -> Option<Vec<(SyntectStyle, &'a str)>> {
    let ops = parse_state.parse_line(line, set).ok()?;
    Some(HighlightIterator::new(state, &ops, line, highlighter).collect())
}

pub fn is_diff_lang(lang: &str) -> bool {
    let l = lang.trim().to_ascii_lowercase();
    l.is_empty() || l == "diff"
}

pub fn is_supported(lang: &str) -> bool {
    syntax_set().find_syntax_by_token(lang).is_some()
}

pub fn highlight_code(lang: &str, code: &str) -> Vec<Line<'static>> {
    let set = syntax_set();
    let highlighter = highlighter();
    let syntax = set
        .find_syntax_by_token(lang)
        .or_else(|| set.find_syntax_by_extension(lang))
        .unwrap_or_else(|| set.find_syntax_plain_text());

    let mut highlight_state = HighlightState::new(highlighter, ScopeStack::new());
    let mut parse_state = ParseState::new(syntax);
    let block_bg = theme::SURFACE;

    let mut lines = Vec::new();
    for line in code.lines() {
        let line = if line.is_empty() { " " } else { line };
        let range = match highlight_line(
            &mut highlight_state,
            &mut parse_state,
            set,
            highlighter,
            line,
        ) {
            Some(r) => r,
            None => {
                lines.push(Line::from(Span::raw(line.to_string()).style(theme::PLAIN)));
                continue;
            }
        };
        let mut spans = Vec::with_capacity(range.len());
        for (style, text) in range {
            let style = to_ratatui_style(to_ratatui_color(style.foreground), style.font_style);
            spans.push(Span::styled(text.to_string(), style));
        }
        if spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans).style(Style::new().bg(block_bg)));
    }

    lines
}
