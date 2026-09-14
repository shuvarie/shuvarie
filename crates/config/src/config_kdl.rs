use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};

use super::kdl_util::{child_nodes, node_error, parse_document};
use super::{
    AgentConfig, Config, ConfigError, ContextConfig, EmbeddingConfig, LspConfigRepr,
    LspServerSpecRepr, RegistriesConfig, RegistryEntry, RetryConfig, ShellConfig, SidebarPref,
    SkillsConfig, UiPrefs,
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
