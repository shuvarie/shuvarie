use ratatui::prelude::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::md::{plain, render, render_pass};
use crate::{code, diff, syntax, theme};

fn styles_of(line: &Line<'static>) -> Vec<Style> {
    line.spans.iter().map(|s| s.style).collect()
}

fn texts(line: &Line<'static>) -> Vec<String> {
    line.spans.iter().map(|s| s.content.to_string()).collect()
}

fn joined(line: &Line<'static>) -> String {
    texts(line).join("")
}

fn is_blank(line: &Line<'static>) -> bool {
    line.spans.iter().all(|s| s.content.is_empty())
}

fn rows(lines: &[Line<'static>]) -> Vec<String> {
    lines.iter().map(|l| joined(l)).collect()
}

#[test]
fn unterminated_heading_and_rule_stay_uncommitted() {
    // pulldown-cmark ends a heading or rule range at the buffer's end when
    // the line break has not been seen: the block still grows with the next
    // delta, so committing its frozen half would split it in two.
    let pass = render_pass("# Ti");
    assert!(pass.boundaries.is_empty());
    let pass = render_pass("---");
    assert!(pass.boundaries.is_empty());
    let pass = render_pass("--- ");
    assert!(pass.boundaries.is_empty());
}

#[test]
fn terminated_heading_and_rule_commit() {
    let pass = render_pass("# Title\n");
    assert_eq!(pass.boundaries.last().copied(), Some((8, 2)));
    let pass = render_pass("a\n\n---\n");
    assert_eq!(pass.boundaries.last().copied(), Some((7, 4)));
}

#[test]
fn plain_preserves_lines() {
    let lines = plain("hello\nworld");
    assert_eq!(lines.len(), 2);
    assert_eq!(texts(&lines[0]), ["hello"]);
    assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT));
}

#[test]
fn paragraph_renders_one_line() {
    let lines = render("hello world");
    assert_eq!(lines.len(), 1);
    assert_eq!(texts(&lines[0]), ["hello world"]);
}

#[test]
fn soft_break_renders_line_break() {
    let lines = render("hello\nworld");
    assert_eq!(lines.len(), 2);
    assert_eq!(texts(&lines[0]), ["hello"]);
    assert_eq!(texts(&lines[1]), ["world"]);
}

#[test]
fn paragraph_break_renders_blank_line() {
    let lines = render("first\n\nsecond");
    assert_eq!(lines.len(), 3);
    assert_eq!(texts(&lines[0]), ["first"]);
    assert!(is_blank(&lines[1]));
    assert_eq!(texts(&lines[2]), ["second"]);
}

#[test]
fn bold_italic_strikethrough_styles() {
    let lines = render("a **b** c *d* e ~~f~~");
    assert_eq!(lines.len(), 1);
    let styles = styles_of(&lines[0]);
    let text_vals = texts(&lines[0]);
    let bold = text_vals.iter().position(|t| t == "b").unwrap();
    let ital = text_vals.iter().position(|t| t == "d").unwrap();
    let strike = text_vals.iter().position(|t| t == "f").unwrap();
    assert!(styles[bold].add_modifier.contains(Modifier::BOLD));
    assert!(styles[ital].add_modifier.contains(Modifier::ITALIC));
    assert!(styles[strike].add_modifier.contains(Modifier::CROSSED_OUT));
}

#[test]
fn inline_code_has_bg() {
    let lines = render("use `foo`");
    assert_eq!(lines.len(), 1);
    let code_idx = texts(&lines[0]).iter().position(|t| t == "foo").unwrap();
    assert_eq!(lines[0].spans[code_idx].style.bg, Some(theme::ACCENT_BG));
}

#[test]
fn heading_styles() {
    let lines = render("# Big\n\n### Small");
    assert_eq!(texts(&lines[0]), ["Big"]);
    assert_eq!(lines[0].spans[0].style.fg, Some(theme::ACCENT));
    assert!(
        lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(texts(&lines[2]), ["Small"]);
    assert_eq!(lines[2].spans[0].style.fg, Some(theme::TEXT));
    assert!(
        lines[2].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn code_block_fences_and_highlight() {
    let lines = render("```rust\nfn main() {}\n```");
    assert_eq!(rows(&lines)[0], "```rust");
    assert_eq!(rows(&lines).last().unwrap(), "```");
    assert_eq!(lines.len(), 3);
    assert!(joined(&lines[1]).contains("fn main"));
    assert_eq!(lines[1].style.bg, None);
    assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT_MUTED));
    let has_keyword = styles_of(&lines[1])
        .iter()
        .any(|s| s.fg == Some(theme::ACCENT));
    assert!(has_keyword, "expected a keyword-colored span");
}

#[test]
fn code_block_plain_fallback() {
    let lines = render("```nosuchlang123\narbitrary text\n```");
    let text = rows(&lines);
    assert_eq!(text, vec!["```nosuchlang123", "arbitrary text", "```"]);
    assert_eq!(lines[1].spans[0].style.fg, Some(theme::TEXT));
}

#[test]
fn code_block_keeps_indentation() {
    let lines = render("```rust\nfn main() {\n    let x = 1;\n}\n```");
    let text = rows(&lines);
    assert!(text[2].starts_with("\u{a0}\u{a0}\u{a0}\u{a0}"));
    assert!(text[2].contains("let x = 1;"));
}

#[test]
fn code_block_tabs_render_four_wide() {
    let lines = render("```\n\tlet x = 1;\n\t\tlet y = 2;\n```");
    let text = rows(&lines);
    assert!(text[1].starts_with("\u{a0}\u{a0}\u{a0}\u{a0}"));
    assert!(text[2].starts_with("\u{a0}".repeat(8).as_str()));
}

#[test]
fn code_block_diff_still_colored() {
    let lines = render("```diff\n+ added\n- removed\n@@ hunk @@\ncontext\n```");
    let text = rows(&lines);
    assert_eq!(text.len(), 6);
    assert_eq!(lines[1].spans[0].style.fg, Some(theme::SUCCESS));
    assert_eq!(lines[2].spans[0].style.fg, Some(theme::ERROR));
    assert_eq!(lines[3].spans[0].style.fg, Some(theme::ACCENT));
    assert_eq!(lines[4].spans[0].style.fg, Some(theme::TEXT_DIM));
    assert_eq!(text[0], "```diff");
    assert_eq!(text[5], "```");
}

#[test]
fn unterminated_fence_does_not_panic() {
    let lines = render("```rust\nfn main() {");
    let text = rows(&lines);
    assert_eq!(text[0], "```rust");
    assert!(joined(&lines[1]).contains("fn main()"));
}

#[test]
fn streaming_reparse_stable() {
    let base = "Here is some text\n\n```rust\nlet x = 1;";
    let a = render(base);
    let b = render(&format!("{base}\nx += 1;"));
    assert_eq!(texts(&a[0]), ["Here is some text"]);
    assert_eq!(texts(&b[0]), ["Here is some text"]);
}

#[test]
fn lists_get_markers() {
    let lines = render("- one\n- two");
    assert_eq!(lines.len(), 2);
    assert!(joined(&lines[0]).starts_with("• one"));
    assert!(joined(&lines[1]).starts_with("• two"));
}

#[test]
fn ordered_list_numbers() {
    let lines = render("1. first\n2. second");
    assert_eq!(lines.len(), 2);
    assert!(joined(&lines[0]).starts_with("1. first"));
    assert!(joined(&lines[1]).starts_with("2. second"));
}

#[test]
fn blockquote_gutter() {
    let lines = render("> quoted");
    assert_eq!(lines.len(), 1);
    assert!(joined(&lines[0]).starts_with("│ quoted"));
}

#[test]
fn rule_renders_divider() {
    let lines = render("a\n\n---\n\nb");
    assert_eq!(lines.len(), 5);
    assert!(texts(&lines[2])[0].starts_with('─'));
}

#[test]
fn table_renders_box_drawing() {
    let lines = render("| a | b |\n|---|---|\n| c | d |");
    assert_eq!(
        rows(&lines),
        vec![
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ c │ d │",
            "└───┴───┘",
        ]
    );
    let border = lines[0].spans[0].style.fg;
    assert_eq!(border, Some(theme::TEXT_MUTED));
    let header_bold = lines[1]
        .spans
        .iter()
        .any(|s| s.content == "a" && s.style.add_modifier.contains(Modifier::BOLD));
    assert!(header_bold, "header cells are bold");
    let body_plain = lines[3]
        .spans
        .iter()
        .all(|s| !s.style.add_modifier.contains(Modifier::BOLD));
    assert!(body_plain, "body cells are not bold");
}

#[test]
fn table_unifies_column_widths() {
    let lines = render("| one | two |\n|-----|-----|\n| x | a very long cell value |");
    let text = rows(&lines);
    assert_eq!(
        text,
        vec![
            "┌─────┬────────────────────────┐",
            "│ one │ two                    │",
            "├─────┼────────────────────────┤",
            "│ x   │ a very long cell value │",
            "└─────┴────────────────────────┘",
        ]
    );
}

#[test]
fn table_respects_alignment() {
    let lines = render("| left | right | center |\n|:--|--:|:-:|\n| L | R | C |");
    assert_eq!(
        rows(&lines),
        vec![
            "┌──────┬───────┬────────┐",
            "│ left │ right │ center │",
            "├──────┼───────┼────────┤",
            "│ L    │     R │   C    │",
            "└──────┴───────┴────────┘",
        ]
    );
}

#[test]
fn table_pads_missing_cells() {
    let lines = render("| a | b |\n|---|---|\n| x |\n| y | z | w | extra |");
    let text = rows(&lines);
    assert_eq!(
        text,
        vec![
            "┌───┬───┐",
            "│ a │ b │",
            "├───┼───┤",
            "│ x │   │",
            "│ y │ z │",
            "└───┴───┘",
        ]
    );
}

#[test]
fn table_wide_cells_count_columns() {
    let lines = render("| 日本語 | b |\n|------|---|\n| x | y |");
    let text = rows(&lines);
    assert_eq!(
        text,
        vec![
            "┌────────┬───┐",
            "│ 日本語 │ b │",
            "├────────┼───┤",
            "│ x      │ y │",
            "└────────┴───┘",
        ]
    );
}

#[test]
fn links_underline() {
    let lines = render("[text](https://example.com)");
    assert_eq!(lines.len(), 1);
    assert_eq!(texts(&lines[0]), ["text"]);
    assert!(
        lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn wide_chars_do_not_panic() {
    let lines = render("```rust\n// 日本語コメント\nlet 変数 = 1;\n```");
    assert_eq!(lines.len(), 4);
}

#[test]
fn diff_module_marks_lines() {
    let lines = diff::highlight_diff("+a\n-b\n@@c\n d\n\\e");
    assert_eq!(lines[0].spans[0].style.fg, Some(theme::SUCCESS));
    assert_eq!(lines[1].spans[0].style.fg, Some(theme::ERROR));
    assert_eq!(lines[2].spans[0].style.fg, Some(theme::ACCENT));
    assert_eq!(
        lines[3].spans.last().unwrap().style.fg,
        Some(theme::TEXT_DIM)
    );
    assert_eq!(lines[4].spans[0].style.fg, Some(theme::TEXT_MUTED));
    assert_eq!(
        lines[3].spans[0].content.to_string(),
        "\u{a0}",
        "the context line's leading space stays a visible-width no-break space"
    );
}

#[test]
fn expand_tabs_on_tab_stops() {
    assert_eq!(code::expand_tabs("\ta"), "    a");
    assert_eq!(code::expand_tabs("a\tb"), "a   b");
    assert_eq!(code::expand_tabs("ab\t"), "ab  ");
    assert_eq!(code::expand_tabs("no tabs"), "no tabs");
}

#[test]
fn keep_indent_converts_leading_whitespace() {
    let line = Line::from(Span::raw("  code".to_string()));
    let kept = code::keep_indent(line);
    assert_eq!(kept_text(&kept), "\u{a0}\u{a0}code");
    let untouched = code::keep_indent(Line::from(Span::raw("code".to_string())));
    assert_eq!(kept_text(&untouched), "code");
}

fn kept_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.to_string()).collect()
}

#[test]
fn syntax_lang_detection() {
    assert!(syntax::is_supported("rust"));
    assert!(syntax::is_supported("rs"));
    assert!(syntax::is_supported("json"));
    assert!(syntax::is_supported("python"));
    assert!(!syntax::is_supported("definitely-not-a-lang"));
    assert!(syntax::is_diff_lang("diff"));
    assert!(syntax::is_diff_lang(""));
    assert!(!syntax::is_diff_lang("rust"));
}

#[test]
fn highlight_code_rust_tokens() {
    let lines = syntax::highlight_code("rust", "let x = \"hi\";\n");
    assert_eq!(lines.len(), 1);
    assert!(joined(&lines[0]).contains("let"));
    assert!(joined(&lines[0]).contains("\"hi\""));
    let has_string = lines[0]
        .spans
        .iter()
        .any(|s| s.style.fg == Some(theme::SAGE));
    assert!(has_string, "expected a string-colored span");
}

#[test]
fn highlight_code_wide_chars() {
    let lines = syntax::highlight_code("rust", "// 日本語\nfn main() {}\n");
    assert_eq!(lines.len(), 2);
}
