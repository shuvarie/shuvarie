use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::kdl_util::{autoformat, node_error, parse_document};
use super::{Active, Connections, DecisionActive, DecisionProviderConfig, ProviderConfig};
use crate::Result;

pub(crate) fn from_kdl(contents: &str) -> Result<Connections> {
    from_document(&parse_document(contents)?, contents)
}

fn from_document(doc: &KdlDocument, input: &str) -> Result<Connections> {
    let mut connections = Connections::default();
    for node in doc.nodes() {
        match node.name().value() {
            "active" => {
                if connections.active.is_some() {
                    return Err(node_error(input, node, "duplicate `active` node", None));
                }
                let children = node.children().ok_or_else(|| {
                    node_error(
                        input,
                        node,
                        "`active` requires a block with a `provider` child",
                        Some(
                            "expected: active { provider \"id\" (model \"id\") (variant \
                             \"id\") }"
                                .into(),
                        ),
                    )
                })?;
                let mut provider = None;
                let mut model = None;
                let mut variant = None;
                for child in children.nodes() {
                    let field = match child.name().value() {
                        "provider" => {
                            if provider.is_some() {
                                return Err(node_error(
                                    input,
                                    child,
                                    "duplicate `provider` in `active` block",
                                    None,
                                ));
                            }
                            &mut provider
                        }
                        "model" => {
                            if model.is_some() {
                                return Err(node_error(
                                    input,
                                    child,
                                    "duplicate `model` in `active` block",
                                    None,
                                ));
                            }
                            &mut model
                        }
                        "variant" => {
                            if variant.is_some() {
                                return Err(node_error(
                                    input,
                                    child,
                                    "duplicate `variant` in `active` block",
                                    None,
                                ));
                            }
                            &mut variant
                        }
                        _ => continue,
                    };
                    match child.get(0) {
                        Some(KdlValue::String(value)) => *field = Some(value.clone()),
                        Some(_) => {
                            return Err(node_error(
                                input,
                                child,
                                format!("`{}` must be a string", child.name().value()),
                                None,
                            ));
                        }
                        None => {
                            return Err(node_error(
                                input,
                                child,
                                format!("`{}` requires a string argument", child.name().value()),
                                None,
                            ));
                        }
                    }
                }
                let Some(provider) = provider else {
                    return Err(node_error(
                        input,
                        node,
                        "`active` block requires a `provider` child",
                        Some("expected: active { provider \"id\" }".into()),
                    ));
                };
                connections.active = Some(Active {
                    provider,
                    model,
                    variant,
                });
            }
            "providers" => {
                let children = node
                    .children()
                    .ok_or_else(|| node_error(input, node, "`providers` has no children", None))?;
                for provider_node in children.nodes() {
                    parse_provider(provider_node, input, &mut connections.providers)?;
                }
            }
            "decision-providers" => {
                let children = node.children().ok_or_else(|| {
                    node_error(input, node, "`decision-providers` has no children", None)
                })?;
                for provider_node in children.nodes() {
                    parse_decision_provider(
                        provider_node,
                        input,
                        &mut connections.decision_providers,
                    )?;
                }
            }
            "decision" => {
                if connections.decision.is_some() {
                    return Err(node_error(input, node, "duplicate `decision` node", None));
                }
                connections.decision = Some(parse_decision_active(node, input)?);
            }
            _ => {
                return Err(node_error(
                    input,
                    node,
                    format!("unknown node `{}`", node.name().value()),
                    Some(
                        "expected `active`, `providers`, `decision-providers`, or `decision` \
                         (the legacy format is no longer supported)"
                            .into(),
                    ),
                ));
            }
        }
    }
    Ok(connections)
}

/// One `provider "name" { … }` inside `decision-providers`. Children are
/// strict: an unrecognized child or property is an error rather than a silent
/// drop, since a mistyped endpoint would otherwise look configured.
fn parse_decision_provider(
    node: &KdlNode,
    input: &str,
    providers: &mut BTreeMap<String, DecisionProviderConfig>,
) -> Result<()> {
    if node.name().value() != "provider" {
        return Err(node_error(
            input,
            node,
            format!("expected `provider`, found `{}`", node.name().value()),
            None,
        ));
    }
    let mut name = None;
    for entry in node.entries() {
        if entry.name().is_some() {
            return Err(node_error(
                input,
                node,
                "`provider` takes a positional name, not properties",
                Some("expected: provider \"name\" { ... }".into()),
            ));
        }
        match entry.value() {
            KdlValue::String(value) if !value.trim().is_empty() => {
                if name.is_some() {
                    return Err(node_error(
                        input,
                        node,
                        "`provider` takes exactly one name",
                        None,
                    ));
                }
                name = Some(value.clone());
            }
            KdlValue::String(_) => {
                return Err(node_error(
                    input,
                    node,
                    "decision provider name must not be empty",
                    None,
                ));
            }
            _ => {
                return Err(node_error(
                    input,
                    node,
                    "decision provider name must be a string",
                    None,
                ));
            }
        }
    }
    let Some(name) = name else {
        return Err(node_error(
            input,
            node,
            "`provider` requires a name argument",
            Some("expected: provider \"name\" { type \"systemone\" }".into()),
        ));
    };
    let children = node.children().ok_or_else(|| {
        node_error(
            input,
            node,
            "`provider` requires a block with a `type` child",
            Some(
                "expected: provider \"name\" { type \"systemone\" (base-url \"…\") (api-key \
                 \"…\") }"
                    .into(),
            ),
        )
    })?;
    let mut kind = None;
    let mut api_key = None;
    let mut base_url = None;
    for child in children.nodes() {
        let field = match child.name().value() {
            "type" => &mut kind,
            "api-key" => &mut api_key,
            "base-url" => &mut base_url,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in a decision provider (expected `type`, \
                         `api-key`, or `base-url`)"
                    ),
                    None,
                ));
            }
        };
        if field.is_some() {
            return Err(node_error(
                input,
                child,
                format!(
                    "duplicate `{}` in a decision provider",
                    child.name().value()
                ),
                None,
            ));
        }
        *field = Some(scalar_child(input, child)?);
    }
    let Some(kind) = kind else {
        return Err(node_error(
            input,
            node,
            "`provider` requires a `type` child",
            Some("expected: type \"systemone\"".into()),
        ));
    };
    if kind != "systemone" {
        return Err(node_error(
            input,
            node,
            format!("unknown decision provider type `{kind}`"),
            Some("expected: type \"systemone\"".into()),
        ));
    }
    let config = DecisionProviderConfig {
        kind,
        api_key,
        base_url,
    };
    if providers.insert(name, config).is_some() {
        return Err(node_error(
            input,
            node,
            "duplicate decision provider name",
            None,
        ));
    }
    Ok(())
}

/// The active decision connection: both children are required, since a
/// decision connection without a model cannot evaluate anything.
fn parse_decision_active(node: &KdlNode, input: &str) -> Result<DecisionActive> {
    if !node.entries().is_empty() {
        return Err(node_error(
            input,
            node,
            "`decision` takes no arguments",
            Some("expected: decision { provider \"name\"; model \"id\" }".into()),
        ));
    }
    let children = node.children().ok_or_else(|| {
        node_error(
            input,
            node,
            "`decision` requires a block",
            Some("expected: decision { provider \"name\"; model \"id\" }".into()),
        )
    })?;
    let mut provider = None;
    let mut model = None;
    for child in children.nodes() {
        let field = match child.name().value() {
            "provider" => &mut provider,
            "model" => &mut model,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `decision` (expected `provider` or `model`)"
                    ),
                    None,
                ));
            }
        };
        if field.is_some() {
            return Err(node_error(
                input,
                child,
                format!("duplicate `{}` in `decision`", child.name().value()),
                None,
            ));
        }
        *field = Some(scalar_child(input, child)?);
    }
    let (Some(provider), Some(model)) = (provider, model) else {
        return Err(node_error(
            input,
            node,
            "`decision` requires both a `provider` and a `model` child",
            Some("expected: decision { provider \"name\"; model \"id\" }".into()),
        ));
    };
    Ok(DecisionActive { provider, model })
}

/// One node's single string argument, with no properties and no block.
fn scalar_child(input: &str, node: &KdlNode) -> Result<String> {
    if node.children().is_some() {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no children", node.name().value()),
            None,
        ));
    }
    if node.entries().iter().any(|entry| entry.name().is_some()) {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no properties", node.name().value()),
            None,
        ));
    }
    if node.entries().len() != 1 {
        return Err(node_error(
            input,
            node,
            format!(
                "`{}` requires exactly one string argument",
                node.name().value()
            ),
            None,
        ));
    }
    match node.get(0) {
        Some(KdlValue::String(value)) => Ok(value.clone()),
        Some(_) => Err(node_error(
            input,
            node,
            format!("`{}` must be a string", node.name().value()),
            None,
        )),
        None => Err(node_error(
            input,
            node,
            format!("`{}` requires a string argument", node.name().value()),
            None,
        )),
    }
}

fn parse_provider(
    node: &KdlNode,
    input: &str,
    providers: &mut BTreeMap<String, ProviderConfig>,
) -> Result<()> {
    if node.name().value() != "provider" {
        return Err(node_error(
            input,
            node,
            format!("expected `provider`, found `{}`", node.name().value()),
            None,
        ));
    }
    let mut id = None;
    let mut name = None;
    for entry in node.entries() {
        let Some(prop) = entry.name() else {
            return Err(node_error(
                input,
                node,
                "`provider` does not take positional arguments",
                Some("expected: provider id=\"id\" name=\"name\" { ... }".into()),
            ));
        };
        let KdlValue::String(value) = entry.value() else {
            return Err(node_error(
                input,
                node,
                format!("provider `{}` must be a string", prop.value()),
                None,
            ));
        };
        match prop.value() {
            "id" => {
                if id.is_some() {
                    return Err(node_error(input, node, "duplicate `id` property", None));
                }
                if value.trim().is_empty() {
                    return Err(node_error(
                        input,
                        node,
                        "provider `id` must not be empty",
                        None,
                    ));
                }
                id = Some(value.clone());
            }
            "name" => {
                if name.is_some() {
                    return Err(node_error(input, node, "duplicate `name` property", None));
                }
                if value.trim().is_empty() {
                    return Err(node_error(
                        input,
                        node,
                        "provider `name` must not be empty",
                        None,
                    ));
                }
                name = Some(value.clone());
            }
            other => {
                return Err(node_error(
                    input,
                    node,
                    format!("unknown provider property `{other}`"),
                    Some("expected `id` or `name`".into()),
                ));
            }
        }
    }
    let Some(id) = id else {
        return Err(node_error(
            input,
            node,
            "`provider` requires an `id` property",
            Some("expected: provider id=\"id\" name=\"name\" { ... }".into()),
        ));
    };
    let Some(name) = name else {
        return Err(node_error(
            input,
            node,
            "`provider` requires a `name` property",
            Some("expected: provider id=\"id\" name=\"name\" { ... }".into()),
        ));
    };
    let mut kind = None;
    let mut catalog = None;
    let mut api_key = None;
    let mut base_url = None;
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
                "kind" => kind = as_string(value)?,
                "catalog" => catalog = as_string(value)?,
                "api-key" => api_key = as_string(value)?,
                "base-url" => base_url = as_string(value)?,
                _ => {}
            }
        }
    }
    let Some(kind) = kind else {
        return Err(node_error(
            input,
            node,
            "`provider` requires a `kind` child",
            Some(
                "expected: kind \"<transport>\" (openai, openai-compat, anthropic, google, \
                 ollama, ...) inside the provider block"
                    .into(),
            ),
        ));
    };
    let config = ProviderConfig {
        name,
        kind,
        catalog,
        api_key,
        base_url,
    };
    if providers.insert(id, config).is_some() {
        return Err(node_error(input, node, "duplicate provider id", None));
    }
    Ok(())
}

const FILE_HEADER: &str = concat!(
    "// ¡¡¡ THIS FILE CONTAINS YOUR API KEYS !!!\n",
    "// ¡¡¡ DO NOT SHARE IT IN PUBLIC !!!\n",
    "\n",
);

pub(crate) fn to_kdl(connections: &Connections) -> Result<String> {
    let mut doc = KdlDocument::new();
    if let Some(active) = &connections.active {
        let mut node = KdlNode::new("active");
        let mut body = KdlDocument::new();
        let mut child = KdlNode::new("provider");
        child.push(KdlEntry::new(active.provider.as_str()));
        body.nodes_mut().push(child);
        if let Some(model) = &active.model {
            let mut child = KdlNode::new("model");
            child.push(KdlEntry::new(model.as_str()));
            body.nodes_mut().push(child);
        }
        if let Some(variant) = &active.variant {
            let mut child = KdlNode::new("variant");
            child.push(KdlEntry::new(variant.as_str()));
            body.nodes_mut().push(child);
        }
        node.set_children(body);
        doc.nodes_mut().push(node);
    }
    let mut providers = KdlNode::new("providers");
    let mut body = KdlDocument::new();
    for (id, config) in &connections.providers {
        let mut node = KdlNode::new("provider");
        node.push(KdlEntry::new_prop("id", KdlValue::String(id.clone())));
        node.push(KdlEntry::new_prop(
            "name",
            KdlValue::String(config.name.clone()),
        ));
        let mut children = KdlDocument::new();
        let mut kind = KdlNode::new("kind");
        kind.push(KdlEntry::new(config.kind.as_str()));
        children.nodes_mut().push(kind);
        if let Some(catalog) = &config.catalog {
            let mut child = KdlNode::new("catalog");
            child.push(KdlEntry::new(catalog.as_str()));
            children.nodes_mut().push(child);
        }
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
        node.set_children(children);
        body.nodes_mut().push(node);
    }
    providers.set_children(body);
    doc.nodes_mut().push(providers);
    // Emitted even when empty, like `providers`, so the file's shape stays
    // stable and a hand-edit has a block to land in.
    let mut decision_providers = KdlNode::new("decision-providers");
    let mut body = KdlDocument::new();
    for (name, config) in &connections.decision_providers {
        let mut node = KdlNode::new("provider");
        node.push(KdlEntry::new(name.as_str()));
        let mut children = KdlDocument::new();
        let mut kind = KdlNode::new("type");
        kind.push(KdlEntry::new(config.kind.as_str()));
        children.nodes_mut().push(kind);
        if let Some(url) = &config.base_url {
            let mut child = KdlNode::new("base-url");
            child.push(KdlEntry::new(url.as_str()));
            children.nodes_mut().push(child);
        }
        if let Some(key) = &config.api_key {
            let mut child = KdlNode::new("api-key");
            child.push(KdlEntry::new(key.as_str()));
            children.nodes_mut().push(child);
        }
        node.set_children(children);
        body.nodes_mut().push(node);
    }
    decision_providers.set_children(body);
    doc.nodes_mut().push(decision_providers);
    if let Some(decision) = &connections.decision {
        let mut node = KdlNode::new("decision");
        let mut body = KdlDocument::new();
        let mut child = KdlNode::new("provider");
        child.push(KdlEntry::new(decision.provider.as_str()));
        body.nodes_mut().push(child);
        let mut child = KdlNode::new("model");
        child.push(KdlEntry::new(decision.model.as_str()));
        body.nodes_mut().push(child);
        node.set_children(body);
        doc.nodes_mut().push(node);
    }
    autoformat(&mut doc);
    Ok(format!("{FILE_HEADER}{doc}"))
}
