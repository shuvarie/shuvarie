use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::{Connections, CoreError, ProviderConfig, span_to_line_column};
use crate::Result as CoreResult;
use crate::error::ConfigParseError;

const OPENAI_KIND: &str = "openai";

pub(crate) fn from_kdl(contents: &str) -> CoreResult<Connections> {
    from_document(&parse_document(contents)?, contents)
}

fn parse_document(contents: &str) -> CoreResult<KdlDocument> {
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

fn at(
    input: &str,
    offset: usize,
    length: usize,
    message: impl Into<String>,
    help: Option<String>,
) -> CoreError {
    let (line, column, length) = span_to_line_column(input, offset, length);
    CoreError::ConfigParse(ConfigParseError {
        message: message.into(),
        line,
        column,
        length,
        help,
    })
}

fn node_error(
    input: &str,
    node: &KdlNode,
    message: impl Into<String>,
    help: Option<String>,
) -> CoreError {
    at(
        input,
        node.span().offset(),
        node.span().len(),
        message,
        help,
    )
}

fn from_document(doc: &KdlDocument, input: &str) -> CoreResult<Connections> {
    let mut connections = Connections::default();
    let mut had_active = false;
    for node in doc.nodes() {
        match node.name().value() {
            "active" => {
                if had_active {
                    return Err(node_error(input, node, "duplicate `active` node", None));
                }
                had_active = true;
                let args: Vec<&KdlValue> = node.entries().iter().map(|e| e.value()).collect();
                match args.as_slice() {
                    [KdlValue::String(provider)] => {
                        connections.active_provider = Some(provider.clone());
                    }
                    [KdlValue::String(provider), KdlValue::String(model)] => {
                        connections.active_provider = Some(provider.clone());
                        connections.active_model = Some(model.clone());
                    }
                    [] => {
                        return Err(node_error(
                            input,
                            node,
                            "`active` requires a provider name argument",
                            Some("expected: active \"provider\" (\"model\")".into()),
                        ));
                    }
                    _ => {
                        return Err(node_error(
                            input,
                            node,
                            "`active` accepts at most two arguments (provider, model)",
                            Some("both arguments must be strings".into()),
                        ));
                    }
                }
            }
            "providers" => {
                let children = node
                    .children()
                    .ok_or_else(|| node_error(input, node, "`providers` has no children", None))?;
                for provider_node in children.nodes() {
                    parse_provider(provider_node, input, &mut connections.providers)?;
                }
            }
            _ => {
                return Err(node_error(
                    input,
                    node,
                    format!("unknown node `{}`", node.name().value()),
                    Some(
                        "expected `active` or `providers` (the legacy format is no longer \
                         supported)"
                            .into(),
                    ),
                ));
            }
        }
    }
    Ok(connections)
}

fn parse_provider(
    node: &KdlNode,
    input: &str,
    providers: &mut BTreeMap<String, ProviderConfig>,
) -> CoreResult<()> {
    if node.name().value() != "provider" {
        return Err(node_error(
            input,
            node,
            format!("expected `provider`, found `{}`", node.name().value()),
            None,
        ));
    }
    let name = match node.get(0) {
        Some(KdlValue::String(name)) => name.clone(),
        Some(_) => {
            return Err(node_error(
                input,
                node,
                "provider name must be a string",
                None,
            ));
        }
        None => {
            return Err(node_error(
                input,
                node,
                "`provider` requires a name argument",
                Some("expected: provider \"name\" (kind=\"kind\") { ... }".into()),
            ));
        }
    };
    let mut kind = OPENAI_KIND.to_string();
    let mut kind_omitted = true;
    let mut api_key = None;
    let mut base_url = None;
    for entry in node.entries().iter().skip(1) {
        let Some(prop) = entry.name() else {
            return Err(node_error(
                input,
                node,
                "provider takes only one positional argument (the name)",
                None,
            ));
        };
        match prop.value() {
            "kind" => {
                let KdlValue::String(value) = entry.value() else {
                    return Err(node_error(
                        input,
                        node,
                        "provider `kind` must be a string",
                        None,
                    ));
                };
                if value.trim().is_empty() {
                    return Err(node_error(
                        input,
                        node,
                        "provider `kind` must not be empty",
                        None,
                    ));
                }
                kind = value.clone();
                kind_omitted = false;
            }
            other => {
                return Err(node_error(
                    input,
                    node,
                    format!("unknown provider property `{other}`"),
                    Some("expected `kind`".into()),
                ));
            }
        }
    }
    if let Some(children) = node.children() {
        for child in children.nodes() {
            let value = child.get(0);
            let as_string = |v: Option<&KdlValue>| match v {
                Some(KdlValue::String(s)) => Ok(Some(s.clone())),
                Some(_) => Err(node_error(
                    input,
                    child,
                    format!("`{}` must be a string", child.name().value()),
                    None,
                )),
                None => Err(node_error(
                    input,
                    child,
                    format!("`{}` requires a string argument", child.name().value()),
                    None,
                )),
            };
            match child.name().value() {
                "api-key" => api_key = as_string(value)?,
                "base-url" => base_url = as_string(value)?,
                _ => {}
            }
        }
    }
    let config = ProviderConfig {
        kind,
        kind_omitted,
        api_key,
        base_url,
    };
    if providers.insert(name, config).is_some() {
        return Err(node_error(input, node, "duplicate provider name", None));
    }
    Ok(())
}

pub(crate) fn to_kdl(connections: &Connections) -> CoreResult<String> {
    let mut doc = KdlDocument::new();
    if let Some(provider) = &connections.active_provider {
        let mut node = KdlNode::new("active");
        node.push(KdlEntry::new(provider.as_str()));
        if let Some(model) = &connections.active_model {
            node.push(KdlEntry::new(model.as_str()));
        }
        doc.nodes_mut().push(node);
    }
    let mut providers = KdlNode::new("providers");
    let mut body = KdlDocument::new();
    for (name, config) in &connections.providers {
        let mut node = KdlNode::new("provider");
        node.push(KdlEntry::new(name.as_str()));
        if !config.kind_omitted {
            node.push(KdlEntry::new_prop(
                "kind",
                KdlValue::String(config.kind.clone()),
            ));
        }
        let mut children = KdlDocument::new();
        if let Some(key) = &config.api_key {
            let mut child = KdlNode::new("api-key");
            child.push(KdlEntry::new(key.as_str()));
            children.nodes_mut().push(child);
        }
        if let Some(url) = &config.base_url {
            let mut child = KdlNode::new("base-url");
            child.push(KdlEntry::new(url.as_str()));
            children.nodes_mut().push(child);
        }
        if !children.nodes().is_empty() {
            node.set_children(children);
        }
        body.nodes_mut().push(node);
    }
    providers.set_children(body);
    doc.nodes_mut().push(providers);
    doc.autoformat();
    Ok(doc.to_string())
}
