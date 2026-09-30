//! Shared GitHub-Flavored Markdown emission for all backends.

/// A logical block of converted markdown.
#[derive(Debug)]
pub(crate) enum MdBlock {
    Heading {
        level: usize,
        text: String,
    },
    /// A paragraph; kept line breaks render as soft breaks.
    Para {
        lines: Vec<String>,
    },
    Bullet {
        indent: usize,
        text: String,
    },
    Quote {
        lines: Vec<String>,
    },
    Table {
        rows: Vec<Vec<String>>,
    },
    /// Thematic break (page and slide separators, GFM hr).
    Rule,
    /// Pre-converted markdown (epub chapters); emitted without re-escaping.
    Verbatim(String),
}

/// An inline text run with its styling, collected by the XML walkers.
#[derive(Debug)]
pub(crate) struct Piece {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub link: Option<String>,
}

impl Piece {
    pub(crate) fn plain(text: impl Into<String>) -> Self {
        Piece {
            text: text.into(),
            bold: false,
            italic: false,
            link: None,
        }
    }
}

/// Renders blocks with a blank line between each (GFM block separation).
pub(crate) fn render(blocks: &[MdBlock]) -> String {
    let mut chunks: Vec<String> = Vec::with_capacity(blocks.len());
    for block in blocks {
        if let Some(rendered) = render_block(block) {
            chunks.push(rendered);
        }
    }
    if chunks.is_empty() {
        return String::new();
    }
    chunks.join("\n\n") + "\n"
}

fn render_block(block: &MdBlock) -> Option<String> {
    match block {
        MdBlock::Heading { level, text } if !text.trim().is_empty() => Some(format!(
            "{} {}",
            "#".repeat((*level).clamp(1, 6)),
            text.trim()
        )),
        MdBlock::Heading { .. } => None,
        MdBlock::Para { lines } => {
            let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
            let text = lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| escape_leading(line.trim()))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        MdBlock::Bullet { indent, text } if !text.trim().is_empty() => Some(format!(
            "{}- {}",
            "  ".repeat((*indent).min(4)),
            escape_leading(text.trim())
        )),
        MdBlock::Bullet { .. } => None,
        MdBlock::Quote { lines } => {
            let text = lines
                .iter()
                .filter(|line| !line.trim().is_empty())
                .map(|line| format!("> {}", escape_leading(line.trim())))
                .collect::<Vec<_>>()
                .join("\n");
            (!text.is_empty()).then_some(text)
        }
        MdBlock::Table { rows } => table(rows),
        MdBlock::Rule => Some("---".to_string()),
        MdBlock::Verbatim(markdown) => {
            (!markdown.trim().is_empty()).then(|| markdown.trim().to_string())
        }
    }
}

/// Formats a table as GFM, using the first row as the header. Short rows are
/// padded and rows of all-empty cells are dropped.
pub(crate) fn table(rows: &[Vec<String>]) -> Option<String> {
    let rows: Vec<&Vec<String>> = rows
        .iter()
        .filter(|row| row.iter().any(|cell| !cell.trim().is_empty()))
        .collect();
    let &first = rows.first()?;
    let width = rows.iter().map(|row| row.len()).max().unwrap_or(0);
    if width == 0 {
        return None;
    }
    let line = |row: &[String]| {
        let cells: Vec<String> = (0..width)
            .map(|index| cell(row.get(index).map(String::as_str).unwrap_or_default()))
            .collect();
        format!("| {} |", cells.join(" | "))
    };
    let mut out = String::with_capacity(rows.len() * width * 4);
    out.push_str(&line(first));
    out.push('\n');
    out.push_str(&format!("| {} |", vec!["---"; width].join(" | ")));
    for row in &rows[1..] {
        out.push('\n');
        out.push_str(&line(row));
    }
    Some(out)
}

/// Renders one GFM table cell: pipes escaped, line breaks as `<br>`.
pub(crate) fn cell(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('|', "\\|")
        .replace('\n', "<br>")
        .trim()
        .to_string()
}

/// Escapes markdown-significant leading markers so a text paragraph cannot
/// reenter markdown as a heading, list, quote, rule, or link.
pub(crate) fn escape_leading(line: &str) -> String {
    let marker = line.chars().next().is_some_and(|first| {
        matches!(
            first,
            '#' | '>' | '-' | '+' | '*' | '=' | '`' | '~' | '[' | '|'
        )
    });
    let numbered = numbered_marker(line);
    if marker || numbered {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

fn numbered_marker(line: &str) -> bool {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return false;
    }
    match line.chars().nth(digits) {
        Some('.') | Some(')') => line.chars().nth(digits + 1).is_none_or(|next| next == ' '),
        _ => false,
    }
}

/// Merges inline pieces into one string, folding runs that share styling and
/// emitting GFM emphasis and links. Whitespace stays outside emphasis
/// markers so `**` never wraps leading or trailing spaces directly.
pub(crate) fn style_pieces(pieces: Vec<Piece>) -> String {
    let mut merged: Vec<Piece> = Vec::new();
    for piece in pieces {
        if piece.text.is_empty() {
            continue;
        }
        if let Some(last) = merged.last_mut().filter(|last| {
            last.bold == piece.bold && last.italic == piece.italic && last.link == piece.link
        }) {
            last.text.push_str(&piece.text);
        } else {
            merged.push(piece);
        }
    }
    let mut out = String::new();
    for piece in merged {
        let trimmed = piece.text.trim();
        if trimmed.is_empty() {
            out.push_str(piece.text.as_str());
            continue;
        }
        let open = piece.text.len() - piece.text.trim_start().len();
        let close = piece.text.len() - piece.text.trim_end().len();
        let (lead, core, tail) = (
            &piece.text[..open],
            &piece.text[open..piece.text.len() - close],
            &piece.text[piece.text.len() - close..],
        );
        out.push_str(lead);
        if let Some(url) = piece.link {
            out.push_str(&format!("[{core}]({url})"));
        } else if piece.bold || piece.italic {
            let marker = if piece.bold && piece.italic {
                "***"
            } else if piece.bold {
                "**"
            } else {
                "*"
            };
            out.push_str(marker);
            out.push_str(core);
            out.push_str(marker);
        } else {
            out.push_str(core);
        }
        out.push_str(tail);
    }
    out
}
