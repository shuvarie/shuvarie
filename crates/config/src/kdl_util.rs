use kdl::{KdlDocument, KdlNode};

use crate::ConfigError;
use crate::Result;
use crate::error::ConfigParseError;

pub(crate) fn span_to_line_column(
    input: &str,
    offset: usize,
    length: usize,
) -> (usize, usize, usize) {
    let before = input.chars().take(offset).collect::<String>();
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map(|l| l.chars().count())
        .unwrap_or(0)
        + 1;
    (line, column, length)
}

pub(crate) fn parse_document(contents: &str) -> Result<KdlDocument> {
    KdlDocument::parse(contents).map_err(|e| {
        let input: &str = &e.input;
        match e.diagnostics.first() {
            Some(d) => at(
                input,
                d.span.offset(),
                d.span.len(),
                d.message.clone().unwrap_or_else(|| e.to_string()),
                d.help.clone(),
            ),
            None => at(contents, 0, 0, e.to_string(), None),
        }
    })
}

pub(crate) fn at(
    input: &str,
    offset: usize,
    length: usize,
    message: impl Into<String>,
    help: Option<String>,
) -> ConfigError {
    let (line, column, length) = span_to_line_column(input, offset, length);
    ConfigError::Parse(ConfigParseError {
        message: message.into(),
        line,
        column,
        length,
        help,
    })
}

pub(crate) fn node_error(
    input: &str,
    node: &KdlNode,
    message: impl Into<String>,
    help: Option<String>,
) -> ConfigError {
    at(
        input,
        node.span().offset(),
        node.span().len(),
        message,
        help,
    )
}

pub(crate) fn child_nodes(node: &KdlNode) -> &[KdlNode] {
    node.children().map(KdlDocument::nodes).unwrap_or(&[])
}
