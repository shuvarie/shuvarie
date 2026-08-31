pub(crate) fn truncate_output(text: &str, max_chars: usize, hint: &str) -> Option<String> {
    if max_chars == 0 {
        return None;
    }
    let total = text.chars().count();
    if total <= max_chars {
        return None;
    }
    let truncated: String = text.chars().take(max_chars).collect();
    let omitted = total - max_chars;
    Some(format!(
        "{truncated}… (output truncated: {omitted} chars omitted — {hint})"
    ))
}

pub(crate) fn truncate_bytes(text: &str, max_bytes: usize, hint: &str) -> Option<String> {
    if max_bytes == 0 {
        return None;
    }
    if text.len() <= max_bytes {
        return None;
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let kept = &text[..cut];
    let omitted = text.len() - kept.len();
    Some(format!(
        "{kept}… (output truncated: {omitted} bytes omitted — {hint})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_cap_returns_none() {
        assert_eq!(truncate_output("hello", 10, "hint"), None);
        assert_eq!(truncate_output("hello", 5, "hint"), None);
    }

    #[test]
    fn zero_cap_disables_truncation() {
        assert_eq!(truncate_output("hello world", 0, "hint"), None);
    }

    #[test]
    fn over_cap_returns_truncated_with_marker() {
        let text = "a".repeat(120);
        let out = truncate_output(&text, 100, "use offset/limit").unwrap();
        assert!(out.starts_with(&"a".repeat(100)));
        assert!(out.contains("…"));
        assert!(out.contains("20 chars omitted"));
        assert!(out.contains("use offset/limit"));
    }

    #[test]
    fn char_not_byte_boundary() {
        let text = "éééé";
        let out = truncate_output(text, 2, "hint").unwrap();
        assert_eq!(out, "éé… (output truncated: 2 chars omitted — hint)");
    }

    #[test]
    fn byte_truncation_under_cap_and_zero() {
        assert_eq!(truncate_bytes("hello", 10, "hint"), None);
        assert_eq!(truncate_bytes("hello world", 0, "hint"), None);
    }

    #[test]
    fn byte_truncation_splits_at_char_boundary() {
        let text = "éééé";
        let out = truncate_bytes(text, 5, "hint").unwrap();
        assert!(out.starts_with("éé"), "{out}");
        assert!(out.contains("4 bytes omitted"), "{out}");
    }

    #[test]
    fn byte_truncation_ascii() {
        let out = truncate_bytes("abcdef", 4, "hint").unwrap();
        assert!(out.starts_with("abcd"), "{out}");
        assert!(out.contains("2 bytes omitted"), "{out}");
    }
}
