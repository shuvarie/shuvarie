use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::kdl_util::{autoformat, node_error, parse_document};
use super::{Active, Connections, ProviderConfig};
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
    autoformat(&mut doc);
    Ok(format!("{FILE_HEADER}{doc}"))
}
