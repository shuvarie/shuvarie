use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::kdl_util::{child_nodes, node_error, parse_document};
use super::{
    AgentConfig, Config, ConfigError, ContextConfig, EmbeddingConfig, LspConfigRepr,
    LspServerSpecRepr, PathRule, PermissionsConfig, RegistriesConfig, RegistryEntry, RetryConfig,
    RuleSet, ShellConfig, ShellPatternKind, ShellRule, SidebarPref, SkillsConfig, ToolsConfig,
    UiPrefs, Verb, WebSearchConfig, WebSearchKind, WebSearchParamKind, WebSearchParams,
};
use crate::Result;

pub(crate) fn from_kdl(contents: &str) -> Result<Config> {
    Ok(from_kdl_with_sections(contents)?.0)
}

/// Parses one config file, returning the [`Config`] plus the top-level node
/// names its file actually defines — absent sections keep their defaults, so
/// the node list is what tells the merge step apart "absent" from "defined".
pub(crate) fn from_kdl_with_sections(contents: &str) -> Result<(Config, Vec<String>)> {
    let doc = parse_document(contents)?;
    let mut config = Config::default();
    let mut sections = Vec::new();
    for node in doc.nodes() {
        sections.push(node.name().value().to_string());
        match node.name().value() {
            "ui" => config.ui = parse_ui(node, contents)?,
            "embedding" => config.embedding = parse_embedding(node, contents)?,
            "agent" => config.agent = parse_agent(node, contents)?,
            "lsp" => config.lsp = parse_lsp(node, contents)?,
            "skills" => config.skills = parse_skills(node, contents)?,
            "context" => config.context = parse_context(node, contents)?,
            "shell" => config.shell = parse_shell(node, contents)?,
            "registries" => config.registries = parse_registries(node, contents)?,
            "tools" => config.tools = parse_tools(node, contents)?,
            "permissions" => config.permissions = parse_permissions(node, contents)?,
            "retry" => config.retry = parse_retry(node, contents)?,
            _ => {}
        }
    }
    Ok((config, sections))
}

fn set_once<T>(
    input: &str,
    node: &KdlNode,
    slot: &mut Option<T>,
    value: Result<Option<T>>,
) -> Result<()> {
    if slot.is_some() {
        return Err(duplicate(input, node, node.name().value()));
    }
    *slot = value?;
    Ok(())
}

fn duplicate(input: &str, node: &KdlNode, field: &str) -> ConfigError {
    node_error(input, node, format!("duplicate `{field}`"), None)
}

fn scalar_value<'a>(input: &str, node: &'a KdlNode) -> Result<Option<&'a KdlValue>> {
    let mut positionals = node.entries().iter().filter(|entry| entry.name().is_none());
    let Some(first) = positionals.next() else {
        return Ok(None);
    };
    if positionals.next().is_some() {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes a single argument", node.name().value()),
            None,
        ));
    }
    Ok(Some(first.value()))
}

fn type_error(input: &str, node: &KdlNode, expected: &str) -> ConfigError {
    node_error(
        input,
        node,
        format!("`{}` must be {expected}", node.name().value()),
        None,
    )
}

fn range_error(input: &str, node: &KdlNode) -> ConfigError {
    node_error(
        input,
        node,
        format!("`{}` is out of range", node.name().value()),
        None,
    )
}

fn scalar_int<T: TryFrom<i128>>(input: &str, node: &KdlNode) -> Result<Option<T>> {
    match scalar_value(input, node)? {
        None => Ok(None),
        Some(KdlValue::Integer(value)) => T::try_from(*value)
            .map(Some)
            .map_err(|_| range_error(input, node)),
        Some(_) => Err(type_error(input, node, "an integer")),
    }
}

fn scalar_u32(input: &str, node: &KdlNode) -> Result<Option<u32>> {
    scalar_int(input, node)
}

fn scalar_u64(input: &str, node: &KdlNode) -> Result<Option<u64>> {
    scalar_int(input, node)
}

fn scalar_usize(input: &str, node: &KdlNode) -> Result<Option<usize>> {
    scalar_int(input, node)
}

fn scalar_bool(input: &str, node: &KdlNode) -> Result<Option<bool>> {
    match scalar_value(input, node)? {
        None => Ok(None),
        Some(KdlValue::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(type_error(input, node, "a boolean")),
    }
}

fn scalar_string(input: &str, node: &KdlNode) -> Result<Option<String>> {
    match scalar_value(input, node)? {
        None => Ok(None),
        Some(KdlValue::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(type_error(input, node, "a string")),
    }
}

fn scalar_string_vec(input: &str, node: &KdlNode) -> Result<Option<Vec<String>>> {
    let mut values = Vec::new();
    for entry in node.entries().iter().filter(|entry| entry.name().is_none()) {
        match entry.value() {
            KdlValue::String(value) => values.push(value.clone()),
            _ => {
                return Err(type_error(input, node, "a list of string arguments"));
            }
        }
    }
    if values.is_empty() {
        Ok(None)
    } else {
        Ok(Some(values))
    }
}

fn parse_ui(node: &KdlNode, input: &str) -> Result<UiPrefs> {
    let mut frame_rate = None;
    let mut sidebar = None;
    let mut copy_on_select = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "frame-rate" => set_once(input, child, &mut frame_rate, scalar_u32(input, child))?,
            "sidebar" => set_once(input, child, &mut sidebar, parse_sidebar(input, child))?,
            "copy-on-select" => {
                set_once(input, child, &mut copy_on_select, scalar_bool(input, child))?;
            }
            _ => {}
        }
    }
    let mut prefs = UiPrefs::default();
    if let Some(value) = frame_rate {
        prefs.frame_rate = value;
    }
    if let Some(value) = sidebar {
        prefs.sidebar = value;
    }
    if let Some(value) = copy_on_select {
        prefs.copy_on_select = value;
    }
    Ok(prefs)
}

fn parse_sidebar(input: &str, node: &KdlNode) -> Result<Option<SidebarPref>> {
    match scalar_string(input, node)? {
        None => Ok(None),
        Some(value) => match value.as_str() {
            "auto" => Ok(Some(SidebarPref::Auto)),
            "expanded" => Ok(Some(SidebarPref::Expanded)),
            "collapsed" => Ok(Some(SidebarPref::Collapsed)),
            other => Err(node_error(
                input,
                node,
                format!("`sidebar` must be `auto`, `expanded`, or `collapsed`, found `{other}`"),
                None,
            )),
        },
    }
}

fn parse_embedding(node: &KdlNode, input: &str) -> Result<EmbeddingConfig> {
    let mut disabled = None;
    let mut provider = None;
    let mut model = None;
    let mut dimensions = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, scalar_bool(input, child))?,
            "provider" => set_once(input, child, &mut provider, scalar_string(input, child))?,
            "model" => set_once(input, child, &mut model, scalar_string(input, child))?,
            "dimensions" => set_once(input, child, &mut dimensions, scalar_u32(input, child))?,
            _ => {}
        }
    }
    let mut config = EmbeddingConfig::default();
    if let Some(value) = disabled {
        config.disabled = value;
    }
    if let Some(value) = provider {
        config.provider = Some(value);
    }
    if let Some(value) = model {
        config.model = Some(value);
    }
    if let Some(value) = dimensions {
        config.dimensions = Some(value);
    }
    Ok(config)
}

fn parse_agent(node: &KdlNode, input: &str) -> Result<AgentConfig> {
    let mut max_turns = None;
    let mut worker_max_turns = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "max-turns" => set_once(input, child, &mut max_turns, scalar_usize(input, child))?,
            "worker-max-turns" => set_once(
                input,
                child,
                &mut worker_max_turns,
                scalar_usize(input, child),
            )?,
            _ => {}
        }
    }
    let mut config = AgentConfig::default();
    if let Some(value) = max_turns {
        config.max_turns = value;
    }
    if let Some(value) = worker_max_turns {
        config.worker_max_turns = value;
    }
    Ok(config)
}

fn parse_lsp(node: &KdlNode, input: &str) -> Result<LspConfigRepr> {
    let mut disabled = None;
    let mut servers = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, scalar_bool(input, child))?,
            "servers" => {
                set_once(input, child, &mut servers, parse_servers(child, input))?;
            }
            _ => {}
        }
    }
    let mut config = LspConfigRepr::default();
    if let Some(value) = disabled {
        config.disabled = value;
    }
    if let Some(value) = servers {
        config.servers = value;
    }
    Ok(config)
}

fn parse_servers(
    node: &KdlNode,
    input: &str,
) -> Result<Option<BTreeMap<String, LspServerSpecRepr>>> {
    let mut servers = BTreeMap::new();
    for server_node in child_nodes(node) {
        let name = server_node.name().value().to_string();
        let spec = parse_server_spec(server_node, input)?;
        if servers.insert(name.clone(), spec).is_some() {
            return Err(duplicate(input, server_node, &name));
        }
    }
    if servers.is_empty() {
        Ok(None)
    } else {
        Ok(Some(servers))
    }
}

fn parse_server_spec(node: &KdlNode, input: &str) -> Result<LspServerSpecRepr> {
    let mut command = None;
    let mut extensions = None;
    let mut no_auto_start = None;
    let mut root_markers = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "command" => set_once(input, child, &mut command, scalar_string_vec(input, child))?,
            "extensions" => set_once(
                input,
                child,
                &mut extensions,
                scalar_string_vec(input, child),
            )?,
            "no-auto-start" => {
                set_once(input, child, &mut no_auto_start, scalar_bool(input, child))?
            }
            "root-markers" => set_once(
                input,
                child,
                &mut root_markers,
                scalar_string_vec(input, child),
            )?,
            _ => {}
        }
    }
    Ok(LspServerSpecRepr {
        command: command.unwrap_or_default(),
        extensions: extensions.unwrap_or_default(),
        no_auto_start: no_auto_start.unwrap_or_default(),
        root_markers: root_markers.unwrap_or_default(),
    })
}

fn parse_skills(node: &KdlNode, input: &str) -> Result<SkillsConfig> {
    let mut disabled = None;
    let mut dirs = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, scalar_bool(input, child))?,
            "dirs" => set_once(input, child, &mut dirs, scalar_string_vec(input, child))?,
            _ => {}
        }
    }
    let mut config = SkillsConfig::default();
    if let Some(value) = disabled {
        config.disabled = value;
    }
    if let Some(value) = dirs {
        config.dirs = value;
    }
    Ok(config)
}

fn parse_context(node: &KdlNode, input: &str) -> Result<ContextConfig> {
    let mut disabled = None;
    let mut reserved = None;
    let mut keep_recent_tokens = None;
    let mut tool_output_max_chars = None;
    let mut tool_output_max_bytes = None;
    let mut fallback_context_length = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, scalar_bool(input, child))?,
            "reserved" => set_once(input, child, &mut reserved, scalar_u64(input, child))?,
            "keep-recent-tokens" => {
                set_once(
                    input,
                    child,
                    &mut keep_recent_tokens,
                    scalar_u64(input, child),
                )?;
            }
            "tool-output-max-chars" => set_once(
                input,
                child,
                &mut tool_output_max_chars,
                scalar_usize(input, child),
            )?,
            "tool-output-max-bytes" => set_once(
                input,
                child,
                &mut tool_output_max_bytes,
                scalar_usize(input, child),
            )?,
            "fallback-context-length" => set_once(
                input,
                child,
                &mut fallback_context_length,
                scalar_u64(input, child),
            )?,
            _ => {}
        }
    }
    let mut config = ContextConfig::default();
    if let Some(value) = disabled {
        config.disabled = value;
    }
    if let Some(value) = reserved {
        config.reserved = value;
    }
    if let Some(value) = keep_recent_tokens {
        config.keep_recent_tokens = value;
    }
    if let Some(value) = tool_output_max_chars {
        config.tool_output_max_chars = value;
    }
    if let Some(value) = tool_output_max_bytes {
        config.tool_output_max_bytes = value;
    }
    if let Some(value) = fallback_context_length {
        config.fallback_context_length = value;
    }
    Ok(config)
}

fn parse_shell(node: &KdlNode, input: &str) -> Result<ShellConfig> {
    let mut path = None;
    for child in child_nodes(node) {
        if child.name().value() == "path" {
            set_once(input, child, &mut path, scalar_string(input, child))?;
        }
    }
    let mut config = ShellConfig::default();
    if let Some(value) = path {
        config.path = Some(value);
    }
    Ok(config)
}

fn parse_retry(node: &KdlNode, input: &str) -> Result<RetryConfig> {
    let mut max_retries = None;
    for child in child_nodes(node) {
        if child.name().value() == "max-retries" {
            set_once(input, child, &mut max_retries, scalar_usize(input, child))?;
        }
    }
    let mut config = RetryConfig::default();
    if let Some(value) = max_retries {
        config.max_retries = value;
    }
    Ok(config)
}

fn parse_registries(node: &KdlNode, input: &str) -> Result<RegistriesConfig> {
    let mut entries = BTreeMap::new();
    for child in child_nodes(node) {
        let name = child.name().value().to_string();
        let entry = parse_registry_entry(child, input)?;
        if entries.insert(name.clone(), entry).is_some() {
            return Err(duplicate(input, child, &name));
        }
    }
    Ok(RegistriesConfig { entries })
}

fn parse_registry_entry(node: &KdlNode, input: &str) -> Result<RegistryEntry> {
    let mut disabled = None;
    let mut remote_first = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, scalar_bool(input, child))?,
            "remote-first" => {
                set_once(input, child, &mut remote_first, scalar_bool(input, child))?;
            }
            _ => {}
        }
    }
    Ok(RegistryEntry {
        disabled: disabled.unwrap_or_default(),
        remote_first: remote_first.unwrap_or_default(),
    })
}

fn parse_tools(node: &KdlNode, input: &str) -> Result<ToolsConfig> {
    let mut web_search = None;
    for child in child_nodes(node) {
        if child.name().value() == "web-search" {
            set_once(
                input,
                child,
                &mut web_search,
                parse_web_search(child, input),
            )?;
        }
    }
    Ok(ToolsConfig { web_search })
}

fn parse_web_search(node: &KdlNode, input: &str) -> Result<Option<WebSearchConfig>> {
    let mut enabled = None;
    let mut url = None;
    let mut kind = None;
    let mut headers = None;
    let mut params = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "enabled" => set_once(input, child, &mut enabled, scalar_bool(input, child))?,
            "url" => set_once(input, child, &mut url, scalar_string(input, child))?,
            "type" => set_once(input, child, &mut kind, parse_web_search_kind(input, child))?,
            "headers" => {
                set_once(
                    input,
                    child,
                    &mut headers,
                    parse_web_search_headers(child, input),
                )?;
            }
            "params" => set_once(
                input,
                child,
                &mut params,
                parse_web_search_params(child, input),
            )?,
            _ => {}
        }
    }
    let Some(url) = url else {
        return Err(node_error(
            input,
            node,
            "`web-search` requires a `url`",
            None,
        ));
    };
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(node_error(
            input,
            node,
            "`web-search` url must start with http:// or https://",
            None,
        ));
    }
    let kind = kind.ok_or_else(|| {
        node_error(
            input,
            node,
            "`web-search` requires a `type` (\"ollama\" or \"to_markdown\")",
            None,
        )
    })?;
    let params = match params {
        Some((param_kind, map)) => WebSearchParams {
            kind: param_kind.unwrap_or_else(|| WebSearchParamKind::default_for(kind)),
            map,
        },
        None => WebSearchParams::default_for(kind),
    };
    Ok(Some(WebSearchConfig {
        enabled: enabled.unwrap_or(false),
        url,
        kind,
        headers: headers.unwrap_or_default(),
        params,
    }))
}

fn parse_web_search_kind(input: &str, node: &KdlNode) -> Result<Option<WebSearchKind>> {
    match scalar_string(input, node)? {
        None => Ok(None),
        Some(value) => match value.as_str() {
            "ollama" => Ok(Some(WebSearchKind::Ollama)),
            "to_markdown" => Ok(Some(WebSearchKind::ToMarkdown)),
            other => Err(node_error(
                input,
                node,
                format!("`type` must be `ollama` or `to_markdown`, found `{other}`"),
                None,
            )),
        },
    }
}

fn parse_web_search_headers(
    node: &KdlNode,
    input: &str,
) -> Result<Option<BTreeMap<String, String>>> {
    let mut headers = BTreeMap::new();
    for child in child_nodes(node) {
        let name = child.name().value().to_string();
        if !is_header_token(&name) {
            return Err(node_error(
                input,
                child,
                format!("`{name}` is not a valid header name"),
                None,
            ));
        }
        let Some(value) = scalar_string(input, child)? else {
            return Err(node_error(
                input,
                child,
                format!("header `{name}` takes a single string value"),
                None,
            ));
        };
        if headers.insert(name.clone(), value).is_some() {
            return Err(duplicate(input, child, &name));
        }
    }
    Ok((!headers.is_empty()).then_some(headers))
}

fn is_header_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// The parsed `params` block: optional transport override plus the
/// tool-argument → remote-parameter mapping.
type WebSearchParamMapping = (Option<WebSearchParamKind>, BTreeMap<String, String>);

fn parse_web_search_params(node: &KdlNode, input: &str) -> Result<Option<WebSearchParamMapping>> {
    let mut kind = None;
    let type_value = property_string(input, node, "type")?;
    if let Some(value) = type_value {
        kind = Some(match value.as_str() {
            "body-json" => WebSearchParamKind::BodyJson,
            "query" => WebSearchParamKind::Query,
            other => {
                return Err(node_error(
                    input,
                    node,
                    format!("`params` type must be `body-json` or `query`, found `{other}`"),
                    None,
                ));
            }
        });
    }
    let mut map = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "query" => {
                let remote = parse_param_remote(input, child)?;
                if map.insert("query".to_string(), remote).is_some() {
                    return Err(duplicate(input, child, "query"));
                }
            }
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!("unsupported parameter `{other}` (only `query` is supported)"),
                    None,
                ));
            }
        }
    }
    if map.is_empty() {
        return Err(node_error(
            input,
            node,
            "`params` requires a `query` mapping",
            None,
        ));
    }
    Ok(Some((kind, map)))
}

fn parse_param_remote(input: &str, child: &KdlNode) -> Result<String> {
    if child.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            child,
            format!("`{}` takes no positional arguments", child.name().value()),
            None,
        ));
    }
    for entry in child.entries() {
        let name = entry.name().map(|n| n.value()).unwrap_or_default();
        if name != "as" {
            return Err(node_error(
                input,
                child,
                format!("unknown property `{name}`"),
                None,
            ));
        }
    }
    let Some(remote) = property_string(input, child, "as")? else {
        return Err(node_error(
            input,
            child,
            format!(
                "`{}` requires `as=\"…\"` (the remote parameter name)",
                child.name().value()
            ),
            None,
        ));
    };
    Ok(remote)
}

/// The value of a single named property entry, when present.
fn property_string(input: &str, node: &KdlNode, name: &str) -> Result<Option<String>> {
    let mut found = None;
    for entry in node.entries() {
        if entry.name().is_some_and(|n| n.value() == name) {
            if found.is_some() {
                return Err(duplicate(input, node, name));
            }
            match entry.value() {
                KdlValue::String(value) => found = Some(value.clone()),
                _ => return Err(type_error(input, node, "a string")),
            }
        }
    }
    Ok(found)
}

fn parse_permissions(node: &KdlNode, input: &str) -> Result<PermissionsConfig> {
    let mut default = None;
    let mut paths = None;
    let mut shell = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "allow-all" | "deny-all" | "ask-all" => {
                let verb = parse_all_verb(input, child)?;
                set_once(input, child, &mut default, Ok(Some(verb)))?;
            }
            "paths" => set_once(
                input,
                child,
                &mut paths,
                parse_scope(input, child, "paths", parse_path_rules).map(Some),
            )?,
            "shell-patterns" => set_once(
                input,
                child,
                &mut shell,
                parse_scope(input, child, "shell-patterns", parse_shell_rules).map(Some),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `permissions` (expected `allow-all`, \
                         `deny-all`, `ask-all`, `paths`, or `shell-patterns`)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(PermissionsConfig {
        default,
        paths: paths.unwrap_or_default(),
        shell: shell.unwrap_or_default(),
    })
}

fn parse_scope<T>(
    input: &str,
    node: &KdlNode,
    section: &str,
    mut parse_rules: impl FnMut(&KdlNode, &str) -> Result<Vec<T>>,
) -> Result<RuleSet<T>> {
    let mut default = None;
    let mut rules = Vec::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "allow-all" | "deny-all" | "ask-all" => {
                let verb = parse_all_verb(input, child)?;
                set_once(input, child, &mut default, Ok(Some(verb)))?;
            }
            "allow" | "deny" | "ask" => rules.extend(parse_rules(child, input)?),
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `{section}` (expected a permission verb or rule)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(RuleSet { default, rules })
}

fn parse_all_verb(input: &str, node: &KdlNode) -> Result<Verb> {
    if !node.entries().is_empty() {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no arguments", node.name().value()),
            None,
        ));
    }
    if node.children().is_some() {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no children", node.name().value()),
            None,
        ));
    }
    match node.name().value() {
        "allow-all" => Ok(Verb::Allow),
        "deny-all" => Ok(Verb::Deny),
        "ask-all" => Ok(Verb::Ask),
        other => Err(node_error(
            input,
            node,
            format!("`{other}` is not a permission verb"),
            None,
        )),
    }
}

fn rule_verb(input: &str, node: &KdlNode) -> Result<Verb> {
    match node.name().value() {
        "allow" => Ok(Verb::Allow),
        "deny" => Ok(Verb::Deny),
        "ask" => Ok(Verb::Ask),
        other => Err(node_error(
            input,
            node,
            format!("`{other}` is not a permission verb"),
            None,
        )),
    }
}

/// Rejects property entries outside `known` so typos fail loudly instead of
/// being silently ignored.
fn check_props(input: &str, node: &KdlNode, known: &[&str]) -> Result<()> {
    for entry in node.entries() {
        let Some(name) = entry.name() else {
            continue;
        };
        if !known.contains(&name.value()) {
            return Err(node_error(
                input,
                node,
                format!(
                    "unknown property `{}` (expected one of {})",
                    name.value(),
                    known
                        .iter()
                        .map(|p| format!("`{p}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
            ));
        }
    }
    Ok(())
}

fn parse_path_rules(node: &KdlNode, input: &str) -> Result<Vec<PathRule>> {
    check_props(
        input,
        node,
        &["except-hidden", "exact", "exclude-shell-pattern"],
    )?;
    let verb = rule_verb(input, node)?;
    let except_hidden = prop_bool(input, node, "except-hidden")?;
    let exact = prop_bool(input, node, "exact")?;
    let exclude_shell_pattern = prop_bool(input, node, "exclude-shell-pattern")?;
    rule_patterns(input, node, "path")?
        .into_iter()
        .map(|path| {
            Ok(PathRule {
                verb,
                path,
                except_hidden: except_hidden.unwrap_or_default(),
                exact: exact.unwrap_or_default(),
                exclude_shell_pattern: exclude_shell_pattern.unwrap_or_default(),
            })
        })
        .collect()
}

fn parse_shell_rules(node: &KdlNode, input: &str) -> Result<Vec<ShellRule>> {
    check_props(input, node, &["pattern", "interrupt"])?;
    let verb = rule_verb(input, node)?;
    let kind = match property_string(input, node, "pattern")?.as_deref() {
        None => ShellPatternKind::Raw,
        Some("raw") => ShellPatternKind::Raw,
        Some("regex") => ShellPatternKind::Regex,
        Some(other) => {
            return Err(node_error(
                input,
                node,
                format!("`pattern` must be `raw` or `regex`, found `{other}`"),
                None,
            ));
        }
    };
    let interrupt = prop_bool(input, node, "interrupt")?.unwrap_or_default();
    rule_patterns(input, node, "pattern")?
        .into_iter()
        .map(|pattern| {
            Ok(ShellRule {
                verb,
                pattern,
                kind,
                interrupt,
            })
        })
        .collect()
}

/// The rule node's positional arguments: one rule per argument, all sharing
/// the verb and properties (`allow "a" "b"` ≡ two `allow` rules).
fn rule_patterns(input: &str, node: &KdlNode, what: &str) -> Result<Vec<String>> {
    if node.children().is_some() {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no children", node.name().value()),
            None,
        ));
    }
    scalar_string_vec(input, node)?.ok_or_else(|| {
        node_error(
            input,
            node,
            format!(
                "`{}` requires at least one {} argument",
                node.name().value(),
                what
            ),
            None,
        )
    })
}

/// The value of a single named boolean property entry, when present.
fn prop_bool(input: &str, node: &KdlNode, name: &str) -> Result<Option<bool>> {
    let mut found = None;
    for entry in node.entries() {
        if entry.name().is_some_and(|n| n.value() == name) {
            if found.is_some() {
                return Err(duplicate(input, node, name));
            }
            match entry.value() {
                KdlValue::Bool(value) => found = Some(*value),
                _ => {
                    return Err(node_error(
                        input,
                        node,
                        format!("`{name}` must be a boolean"),
                        None,
                    ));
                }
            }
        }
    }
    Ok(found)
}

pub(crate) fn to_kdl(config: &Config) -> Result<String> {
    let mut doc = KdlDocument::new();
    let sections = [
        ui_node(&config.ui),
        embedding_node(&config.embedding),
        agent_node(&config.agent),
        lsp_node(&config.lsp),
        skills_node(&config.skills),
        context_node(&config.context),
        shell_node(&config.shell),
        permissions_node(&config.permissions),
        tools_node(&config.tools),
        registries_node(&config.registries),
        retry_node(&config.retry),
    ];
    for node in sections.into_iter().flatten() {
        doc.nodes_mut().push(node);
    }
    doc.autoformat();
    Ok(doc.to_string())
}

fn section_node(name: &str, children: Vec<KdlNode>) -> Option<KdlNode> {
    if children.is_empty() {
        return None;
    }
    let mut body = KdlDocument::new();
    body.nodes_mut().extend(children);
    let mut node = KdlNode::new(name);
    node.set_children(body);
    Some(node)
}

fn value_node(name: &str, value: impl Into<KdlValue>) -> KdlNode {
    let mut node = KdlNode::new(name);
    node.push(KdlEntry::new(value));
    node
}

fn int_node(name: &str, value: impl Into<i128>) -> KdlNode {
    value_node(name, value.into())
}

fn string_vec_node(name: &str, values: &[String]) -> Option<KdlNode> {
    if values.is_empty() {
        return None;
    }
    let mut node = KdlNode::new(name);
    for value in values {
        node.push(KdlEntry::new(value.as_str()));
    }
    Some(node)
}

fn sidebar_pref_text(pref: SidebarPref) -> &'static str {
    match pref {
        SidebarPref::Auto => "auto",
        SidebarPref::Expanded => "expanded",
        SidebarPref::Collapsed => "collapsed",
    }
}

fn ui_node(cfg: &UiPrefs) -> Option<KdlNode> {
    let defaults = UiPrefs::default();
    let mut children = Vec::new();
    if cfg.frame_rate != defaults.frame_rate {
        children.push(int_node("frame-rate", cfg.frame_rate));
    }
    if cfg.sidebar != defaults.sidebar {
        children.push(value_node("sidebar", sidebar_pref_text(cfg.sidebar)));
    }
    if cfg.copy_on_select {
        children.push(value_node("copy-on-select", true));
    }
    section_node("ui", children)
}

fn embedding_node(cfg: &EmbeddingConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(value_node("disabled", true));
    }
    if let Some(provider) = &cfg.provider {
        children.push(value_node("provider", provider.as_str()));
    }
    if let Some(model) = &cfg.model {
        children.push(value_node("model", model.as_str()));
    }
    if let Some(dimensions) = cfg.dimensions {
        children.push(int_node("dimensions", dimensions));
    }
    section_node("embedding", children)
}

fn agent_node(cfg: &AgentConfig) -> Option<KdlNode> {
    let defaults = AgentConfig::default();
    let mut children = Vec::new();
    if cfg.max_turns != defaults.max_turns {
        children.push(int_node("max-turns", cfg.max_turns as i128));
    }
    if cfg.worker_max_turns != defaults.worker_max_turns {
        children.push(int_node("worker-max-turns", cfg.worker_max_turns as i128));
    }
    section_node("agent", children)
}

fn lsp_node(cfg: &LspConfigRepr) -> Option<KdlNode> {
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(value_node("disabled", true));
    }
    if !cfg.servers.is_empty() {
        let mut servers = KdlNode::new("servers");
        let mut body = KdlDocument::new();
        for (name, spec) in &cfg.servers {
            body.nodes_mut().push(server_spec_node(name, spec));
        }
        servers.set_children(body);
        children.push(servers);
    }
    section_node("lsp", children)
}

fn server_spec_node(name: &str, spec: &LspServerSpecRepr) -> KdlNode {
    let mut children = Vec::new();
    children.extend(string_vec_node("command", &spec.command));
    children.extend(string_vec_node("extensions", &spec.extensions));
    if spec.no_auto_start {
        children.push(value_node("no-auto-start", true));
    }
    children.extend(string_vec_node("root-markers", &spec.root_markers));
    let mut node = KdlNode::new(name);
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}

fn skills_node(cfg: &SkillsConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(value_node("disabled", true));
    }
    children.extend(string_vec_node("dirs", &cfg.dirs));
    section_node("skills", children)
}

fn context_node(cfg: &ContextConfig) -> Option<KdlNode> {
    let defaults = ContextConfig::default();
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(value_node("disabled", true));
    }
    if cfg.reserved != defaults.reserved {
        children.push(int_node("reserved", cfg.reserved));
    }
    if cfg.keep_recent_tokens != defaults.keep_recent_tokens {
        children.push(int_node("keep-recent-tokens", cfg.keep_recent_tokens));
    }
    if cfg.tool_output_max_chars != defaults.tool_output_max_chars {
        children.push(int_node(
            "tool-output-max-chars",
            cfg.tool_output_max_chars as i128,
        ));
    }
    if cfg.tool_output_max_bytes != defaults.tool_output_max_bytes {
        children.push(int_node(
            "tool-output-max-bytes",
            cfg.tool_output_max_bytes as i128,
        ));
    }
    if cfg.fallback_context_length != defaults.fallback_context_length {
        children.push(int_node(
            "fallback-context-length",
            cfg.fallback_context_length,
        ));
    }
    section_node("context", children)
}

fn shell_node(cfg: &ShellConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if let Some(path) = &cfg.path {
        children.push(value_node("path", path.as_str()));
    }
    section_node("shell", children)
}

fn retry_node(cfg: &RetryConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if cfg.max_retries != RetryConfig::default().max_retries {
        children.push(int_node("max-retries", cfg.max_retries as i128));
    }
    section_node("retry", children)
}

fn permissions_node(cfg: &PermissionsConfig) -> Option<KdlNode> {
    if *cfg == PermissionsConfig::default() {
        return None;
    }
    let mut children = Vec::new();
    if let Some(verb) = cfg.default {
        children.push(verb_all_node(verb));
    }
    if cfg.paths.default.is_some() || !cfg.paths.rules.is_empty() {
        children.push(scope_node(
            "paths",
            &cfg.paths,
            |rule| {
                (
                    rule.verb,
                    rule.except_hidden,
                    rule.exact,
                    rule.exclude_shell_pattern,
                )
            },
            path_rule_group,
        ));
    }
    if cfg.shell.default.is_some() || !cfg.shell.rules.is_empty() {
        children.push(scope_node(
            "shell-patterns",
            &cfg.shell,
            |rule| (rule.verb, rule.kind, rule.interrupt),
            shell_rule_group,
        ));
    }
    section_node("permissions", children)
}

fn scope_node<T, K: PartialEq>(
    name: &str,
    scope: &RuleSet<T>,
    key: impl Fn(&T) -> K,
    group_node: impl Fn(&[&T]) -> KdlNode,
) -> KdlNode {
    let mut children = Vec::new();
    if let Some(verb) = scope.default {
        children.push(verb_all_node(verb));
    }
    let mut run: Vec<&T> = Vec::new();
    for rule in &scope.rules {
        if run.first().is_some_and(|first| key(first) != key(rule)) {
            children.push(group_node(&run));
            run.clear();
        }
        run.push(rule);
    }
    if !run.is_empty() {
        children.push(group_node(&run));
    }
    let mut node = KdlNode::new(name);
    let mut body = KdlDocument::new();
    body.nodes_mut().extend(children);
    node.set_children(body);
    node
}

fn verb_all_node(verb: Verb) -> KdlNode {
    KdlNode::new(format!("{}-all", verb.as_str()))
}

fn path_rule_group(rules: &[&PathRule]) -> KdlNode {
    let first = rules[0];
    let mut node = KdlNode::new(first.verb.as_str());
    if first.except_hidden {
        node.push(KdlEntry::new_prop("except-hidden", true));
    }
    if first.exact {
        node.push(KdlEntry::new_prop("exact", true));
    }
    if first.exclude_shell_pattern {
        node.push(KdlEntry::new_prop("exclude-shell-pattern", true));
    }
    for rule in rules {
        node.push(KdlEntry::new(rule.path.as_str()));
    }
    node
}

fn shell_rule_group(rules: &[&ShellRule]) -> KdlNode {
    let first = rules[0];
    let mut node = KdlNode::new(first.verb.as_str());
    if first.kind == ShellPatternKind::Regex {
        node.push(KdlEntry::new_prop("pattern", "regex"));
    }
    if first.interrupt {
        node.push(KdlEntry::new_prop("interrupt", true));
    }
    for rule in rules {
        node.push(KdlEntry::new(rule.pattern.as_str()));
    }
    node
}

fn tools_node(cfg: &ToolsConfig) -> Option<KdlNode> {
    let web = cfg.web_search.as_ref()?;
    section_node("tools", vec![web_search_node(web)])
}

fn web_search_node(cfg: &WebSearchConfig) -> KdlNode {
    let mut children = Vec::new();
    if cfg.enabled {
        children.push(value_node("enabled", true));
    }
    children.push(value_node("url", cfg.url.as_str()));
    children.push(value_node("type", cfg.kind.as_str()));
    if !cfg.headers.is_empty() {
        let mut headers = KdlNode::new("headers");
        let mut body = KdlDocument::new();
        for (name, value) in &cfg.headers {
            body.nodes_mut().push(value_node(name, value.as_str()));
        }
        headers.set_children(body);
        children.push(headers);
    }
    let defaults = WebSearchParams::default_for(cfg.kind);
    if cfg.params != defaults {
        let mut params = KdlNode::new("params");
        if cfg.params.kind != defaults.kind {
            params.push(KdlEntry::new_prop("type", cfg.params.kind.as_str()));
        }
        let mut body = KdlDocument::new();
        for (arg, remote) in &cfg.params.map {
            let mut child = KdlNode::new(arg.as_str());
            child.push(KdlEntry::new_prop("as", remote.as_str()));
            body.nodes_mut().push(child);
        }
        params.set_children(body);
        children.push(params);
    }
    let mut node = KdlNode::new("web-search");
    let mut body = KdlDocument::new();
    body.nodes_mut().extend(children);
    node.set_children(body);
    node
}

fn registries_node(cfg: &RegistriesConfig) -> Option<KdlNode> {
    let children = cfg
        .entries
        .iter()
        .map(|(name, entry)| registry_entry_node(name, *entry))
        .collect();
    section_node("registries", children)
}

fn registry_entry_node(name: &str, entry: RegistryEntry) -> KdlNode {
    let mut children = Vec::new();
    if entry.disabled {
        children.push(value_node("disabled", true));
    }
    if entry.remote_first {
        children.push(value_node("remote-first", true));
    }
    let mut node = KdlNode::new(name);
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}
