#[cfg(test)]
mod tests {
    use ratatui::prelude::{Modifier, Style};
    use ratatui::text::Line;

    use crate::md::{plain, render};
    use crate::{diff, syntax, theme};

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
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
        assert_eq!(texts(&lines[2]), ["Small"]);
        assert_eq!(lines[2].spans[0].style.fg, Some(theme::TEXT));
        assert!(lines[2].spans[0]
            .style
            .add_modifier
            .contains(Modifier::BOLD));
    }

    #[test]
    fn code_block_highlighted() {
        let lines = render("```rust\nfn main() {}\n```");
        assert!(lines.len() >= 1);
        let code_line = &lines[0];
        assert!(joined(code_line).contains("fn main"));
        assert_eq!(code_line.style.bg, Some(theme::SURFACE));
        let has_keyword = styles_of(code_line)
            .iter()
            .any(|s| s.fg == Some(theme::ACCENT));
        assert!(has_keyword, "expected a keyword-colored span");
    }

    #[test]
    fn code_block_plain_fallback() {
        let lines = render("```nosuchlang123\narbitrary text\n```");
        assert_eq!(lines.len(), 1);
        assert_eq!(texts(&lines[0]), ["arbitrary text"]);
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::TEXT));
    }

    #[test]
    fn diff_block_colors() {
        let lines = render("```diff\n+ added\n- removed\n@@ hunk @@\ncontext\n```");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::SUCCESS));
        assert_eq!(lines[1].spans[0].style.fg, Some(theme::ERROR));
        assert_eq!(lines[2].spans[0].style.fg, Some(theme::ACCENT));
        assert_eq!(lines[3].spans[0].style.fg, Some(theme::TEXT_DIM));
    }

    #[test]
    fn unterminated_fence_does_not_panic() {
        let lines = render("```rust\nfn main() {");
        assert_eq!(lines.len(), 1);
        assert!(joined(&lines[0]).contains("fn main()"));
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
    fn table_renders_rows() {
        let lines = render("| a | b |\n|---|---|\n| c | d |");
        assert_eq!(lines.len(), 2);
        let header = joined(&lines[0]);
        assert!(header.contains("a"));
        assert!(header.contains("b"));
        assert!(header.contains("┃"));
        let body = joined(&lines[1]);
        assert!(body.contains("c"));
        assert!(body.contains("d"));
    }

    #[test]
    fn links_underline() {
        let lines = render("[text](https://example.com)");
        assert_eq!(lines.len(), 1);
        assert_eq!(texts(&lines[0]), ["text"]);
        assert!(lines[0].spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED));
    }

    #[test]
    fn wide_chars_do_not_panic() {
        let lines = render("```rust\n// 日本語コメント\nlet 変数 = 1;\n```");
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn diff_module_marks_lines() {
        let lines = diff::highlight_diff("+a\n-b\n@@c\n d\n\\e");
        assert_eq!(lines[0].spans[0].style.fg, Some(theme::SUCCESS));
        assert_eq!(lines[1].spans[0].style.fg, Some(theme::ERROR));
        assert_eq!(lines[2].spans[0].style.fg, Some(theme::ACCENT));
        assert_eq!(lines[3].spans[0].style.fg, Some(theme::TEXT_DIM));
        assert_eq!(lines[4].spans[0].style.fg, Some(theme::TEXT_MUTED));
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
}
