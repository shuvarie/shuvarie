use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use crate::CoreError;

pub(crate) fn scalar(name: &str, value: impl Into<KdlValue>) -> KdlNode {
    let mut node = KdlNode::new(name);
    node.entries_mut().push(KdlEntry::new(value));
    node
}

pub(crate) fn strings(name: &str, values: &[String]) -> Option<KdlNode> {
    (!values.is_empty()).then(|| {
        let mut node = KdlNode::new(name);
        for value in values {
            node.entries_mut().push(KdlEntry::new(value.as_str()));
        }
        node
    })
}

pub(crate) fn push_child(parent: &mut KdlNode, child: KdlNode) {
    parent.ensure_children().nodes_mut().push(child);
}

pub(crate) fn finish(mut doc: KdlDocument) -> String {
    doc.autoformat();
    doc.to_string()
}

pub(crate) struct NodeView<'a> {
    node: &'a KdlNode,
}

impl<'a> NodeView<'a> {
    pub(crate) fn new(node: &'a KdlNode) -> Self {
        Self { node }
    }

    fn child(&self, name: &str) -> Option<&'a KdlNode> {
        self.node
            .children()?
            .nodes()
            .iter()
            .find(|n| n.name().value() == name)
    }

    pub(crate) fn child_is_present(&self, name: &str) -> bool {
        self.child(name).is_some()
    }

    pub(crate) fn strings(&self, name: &str) -> Option<Vec<String>> {
        let child = self.child(name)?;
        Some(
            child
                .entries()
                .iter()
                .filter_map(|e| match e.value() {
                    KdlValue::String(s) => Some(s.to_string()),
                    KdlValue::Integer(i) => Some(i.to_string()),
                    KdlValue::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect(),
        )
    }

    pub(crate) fn string(&self, name: &str) -> Option<String> {
        let child = self.child(name)?;
        match child.entries().first()?.value() {
            KdlValue::String(s) => Some(s.to_string()),
            KdlValue::Integer(i) => Some(i.to_string()),
            KdlValue::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    pub(crate) fn boolean(&self, name: &str, default: bool) -> bool {
        let Some(child) = self.child(name) else {
            return default;
        };
        match child.entries().first().map(KdlEntry::value) {
            Some(KdlValue::Bool(b)) => *b,
            Some(KdlValue::String(s)) if s == "true" => true,
            Some(KdlValue::String(s)) if s == "false" => false,
            _ => default,
        }
    }

    pub(crate) fn integer<T: TryFrom<i128>>(&self, name: &str, default: T) -> T {
        let Some(child) = self.child(name) else {
            return default;
        };
        match child.entries().first().map(KdlEntry::value) {
            Some(KdlValue::Integer(v)) => T::try_from(*v).unwrap_or(default),
            _ => default,
        }
    }
}

pub(crate) trait NodeNamed {
    fn view(&self) -> NodeView<'_>;
}

impl NodeNamed for KdlNode {
    fn view(&self) -> NodeView<'_> {
        NodeView::new(self)
    }
}

pub(crate) fn parse(input: &str) -> Result<KdlDocument, CoreError> {
    input.parse().map_err(|e: kdl::KdlError| {
        let diagnostic = e
            .diagnostics
            .iter()
            .find(|d| d.severity == miette::Severity::Error)
            .or_else(|| e.diagnostics.first());
        let (line, column, length) = diagnostic
            .map(|d| (d.span.offset(), d.span.len()))
            .map(|(offset, len)| span_to_line_column(&e.input, offset, len))
            .unwrap_or((1, 1, 0));
        CoreError::ConfigParse(crate::error::ConfigParseError {
            message: diagnostic
                .and_then(|d| d.message.clone())
                .unwrap_or_else(|| "unexpected error".to_string()),
            line,
            column,
            length,
            help: diagnostic.and_then(|d| d.help.clone()),
        })
    })
}

/// Converts a char offset + length in `input` into a 1-based line and column.
fn span_to_line_column(input: &str, offset: usize, length: usize) -> (usize, usize, usize) {
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
