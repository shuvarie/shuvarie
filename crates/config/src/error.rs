use std::fmt;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, ConfigError>;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("config parse error: {0}")]
    Parse(ConfigParseError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigParseError {
    pub message: String,
    pub line: usize,
    pub column: usize,
    pub length: usize,
    pub help: Option<String>,
}

impl ConfigParseError {
    pub fn location(&self) -> String {
        format!("{}:{}", self.line, self.column)
    }

    pub fn snippet(&self, source: &str) -> Option<String> {
        let line_text = source.lines().nth(self.line.checked_sub(1)?)?;
        let gutter_width = self.line.to_string().len();
        let mut out = String::new();
        let width: usize = gutter_width;
        out.push_str(&format!("{:>width$} | {line_text}\n", self.line));
        let pad = " ".repeat(gutter_width);
        let caret_start = self.column.saturating_sub(1);
        let caret_len = self
            .length
            .max(1)
            .min(line_text.chars().count().saturating_sub(caret_start) + 1);
        out.push_str(&format!(
            "{pad} | {}{}",
            " ".repeat(caret_start),
            "^".repeat(caret_len)
        ));
        Some(out)
    }
}

impl fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}", self.message, self.location())?;
        if let Some(help) = &self.help {
            write!(f, "\nhelp: {help}")?;
        }
        Ok(())
    }
}
