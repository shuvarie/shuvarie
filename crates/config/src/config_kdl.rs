use std::collections::BTreeMap;

use kdl::{KdlDocument, KdlEntry, KdlEntryFormat, KdlNode, KdlValue};

use super::kdl_util::{at, autoformat, child_nodes, node_error, parse_document};
use super::*;
use crate::{Result, theme_key_label};

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
            "scenes" => config.scenes = parse_scenes(node, contents)?,
            "themes" => config.themes = parse_themes(node, contents)?,
            "retry" => config.retry = parse_retry(node, contents)?,
            _ => {}
        }
    }
    Ok((config, sections))
}

/// Parses a standalone `scenes` document (a `scene.d` drop-in): every
/// top-level `scenes` node comes back as its own source, so the level merge
/// can see a name defined by several nodes of one file; other top-level
/// nodes are ignored.
pub(crate) fn scenes_from_document(contents: &str) -> Result<Vec<ScenesConfig>> {
    let doc = parse_document(contents)?;
    let mut sources = Vec::new();
    for node in doc.nodes() {
        if node.name().value() == "scenes" {
            sources.push(parse_scenes(node, contents)?);
        }
    }
    Ok(sources)
}

/// Parses a standalone `themes` document (a `themes.d` drop-in): every
/// top-level `themes` node comes back as its own source, so the level merge
/// can see a name defined by several nodes of one file; other top-level
/// nodes are ignored.
pub(crate) fn themes_from_document(contents: &str) -> Result<Vec<ThemesConfig>> {
    let doc = parse_document(contents)?;
    let mut sources = Vec::new();
    for node in doc.nodes() {
        if node.name().value() == "themes" {
            sources.push(parse_themes(node, contents)?);
        }
    }
    Ok(sources)
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

/// A bare switch node (`disabled`): no arguments, no children — its presence
/// alone sets the flag to `true`. Any argument (the old `disabled #true`
/// form) is an error.
fn switch_flag(input: &str, node: &KdlNode) -> Result<Option<bool>> {
    if !node.entries().is_empty() {
        return Err(node_error(
            input,
            node,
            format!(
                "`{}` takes no arguments (it is a switch)",
                node.name().value()
            ),
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
    Ok(Some(true))
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
    let mut theme = None;
    let mut title = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "frame-rate" => set_once(input, child, &mut frame_rate, scalar_u32(input, child))?,
            "sidebar" => set_once(input, child, &mut sidebar, parse_sidebar(input, child))?,
            "copy-on-select" => {
                set_once(input, child, &mut copy_on_select, scalar_bool(input, child))?;
            }
            "theme" => set_once(input, child, &mut theme, parse_theme_pref(input, child))?,
            "title" => set_once(input, child, &mut title, parse_title(child, input))?,
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
    prefs.theme = theme;
    prefs.title = title.unwrap_or_default();
    Ok(prefs)
}

/// `title { … }` — how a new session's title is drafted. The optional
/// `max-chars` property caps the provisional first-prompt title; the
/// `auto-gen` switch opts into LLM drafting; `llm { … }` carries the
/// settings for those calls. An absent (or empty) `title` keeps the default:
/// first-prompt titling with no LLM involved.
fn parse_title(node: &KdlNode, input: &str) -> Result<Option<TitleConfig>> {
    if node.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            node,
            "`title` takes no positional arguments",
            None,
        ));
    }
    for entry in node.entries() {
        let name = entry.name().map(|n| n.value()).unwrap_or_default();
        if name != "max-chars" {
            return Err(node_error(
                input,
                node,
                format!("unknown property `{name}` (expected `max-chars`)"),
                None,
            ));
        }
    }
    let max_chars = property_usize(input, node, "max-chars")?;
    if max_chars.is_some_and(|value| value == 0) {
        return Err(node_error(
            input,
            node,
            "`max-chars` must be greater than zero",
            None,
        ));
    }
    let mut auto_gen = None;
    let mut llm = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "auto-gen" => set_once(input, child, &mut auto_gen, switch_flag(input, child))?,
            "llm" => set_once(
                input,
                child,
                &mut llm,
                parse_title_llm(child, input).map(Some),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!("unknown node `{other}` in `title` (expected `auto-gen` or `llm`)"),
                    None,
                ));
            }
        }
    }
    if max_chars.is_none() && auto_gen.is_none() && llm.is_none() {
        return Ok(None);
    }
    Ok(Some(TitleConfig {
        max_chars: max_chars.unwrap_or(DEFAULT_TITLE_PROMPT_CHARS),
        auto_gen: auto_gen.unwrap_or(false),
        ..llm.unwrap_or_default()
    }))
}

fn parse_title_llm(node: &KdlNode, input: &str) -> Result<TitleConfig> {
    if node.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            node,
            "`llm` takes no positional arguments",
            None,
        ));
    }
    let mut provider = None;
    let mut model = None;
    let mut system_prompt = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "provider" => set_once(input, child, &mut provider, scalar_string(input, child))?,
            "model" => set_once(input, child, &mut model, scalar_string(input, child))?,
            "system-prompt" => set_once(
                input,
                child,
                &mut system_prompt,
                parse_prompt_text(input, child, "system-prompt"),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `llm` (expected `provider`, \
                         `model`, or `system-prompt`)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(TitleConfig {
        provider,
        model,
        system_prompt,
        ..TitleConfig::default()
    })
}

fn parse_theme_pref(input: &str, node: &KdlNode) -> Result<Option<String>> {
    match scalar_string(input, node)? {
        None => Ok(None),
        Some(name) if !name.trim().is_empty() => Ok(Some(name)),
        Some(_) => Err(node_error(input, node, "`theme` must name a theme", None)),
    }
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
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
    let mut tools = BTreeMap::new();
    let mut mcp = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "web-search" => set_once(
                input,
                child,
                &mut web_search,
                parse_web_search(child, input).map(Some),
            )?,
            "tool" => {
                let (name, config) = parse_tool(child, input)?;
                if tools.insert(name.clone(), config).is_some() {
                    return Err(duplicate(input, child, &name));
                }
            }
            "mcp" => set_once(input, child, &mut mcp, parse_mcp(child, input).map(Some))?,
            _ => {}
        }
    }
    Ok(ToolsConfig {
        web_search,
        tools,
        mcp: mcp.unwrap_or_default(),
    })
}

/// `tool name="…" { description …; cmd …; input …; timeout …; params …;
/// envs … }` — a user-defined tool: every call spawns a fresh process running
/// `cmd` with the `{{param}}` placeholders replaced by the call's arguments.
fn parse_tool(node: &KdlNode, input: &str) -> Result<(String, StdioToolConfig)> {
    reject_positionals(input, node)?;
    reject_unknown_props(input, node, &["name"])?;
    let name = name_prop(input, node, "tool")?;
    if name.starts_with("mcp__") {
        return Err(node_error(
            input,
            node,
            format!("`tool` name `{name}` starts with `mcp__`, which is reserved for MCP tools"),
            None,
        ));
    }
    let mut description = None;
    let mut cmd = None;
    let mut input_kind = None;
    let mut timeout_secs = None;
    let mut params = None;
    let mut envs = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "description" => set_once(input, child, &mut description, scalar_string(input, child))?,
            "cmd" => set_once(input, child, &mut cmd, scalar_string_vec(input, child))?,
            "input" => set_once(
                input,
                child,
                &mut input_kind,
                parse_tool_input(input, child).map(Some),
            )?,
            "timeout" => set_once(input, child, &mut timeout_secs, scalar_u64(input, child))?,
            "params" => set_once(input, child, &mut params, parse_tool_params(child, input))?,
            "envs" => set_once(input, child, &mut envs, parse_envs(child, input))?,
            _ => {}
        }
    }
    let cmd = cmd
        .ok_or_else(|| node_error(input, node, format!("tool `{name}` requires a `cmd`"), None))?;
    if timeout_secs.is_some_and(|secs| secs == 0) {
        return Err(node_error(
            input,
            node,
            "`timeout` must be at least one second",
            None,
        ));
    }
    let params = params.unwrap_or_default();
    validate_cmd_placeholders(input, node, &cmd, &params)?;
    Ok((
        name,
        StdioToolConfig {
            description,
            cmd,
            input: input_kind,
            timeout_secs: timeout_secs.unwrap_or(TOOL_DEFAULT_TIMEOUT_SECS),
            params,
            envs: envs.unwrap_or_default(),
        },
    ))
}

/// `input "json"` — pipe the resolved args object to the child's stdin as
/// JSON on top of the templated argv.
fn parse_tool_input(input: &str, node: &KdlNode) -> Result<ToolInputKind> {
    let Some(value) = scalar_string(input, node)? else {
        return Err(node_error(
            input,
            node,
            "`input` takes a single string value",
            None,
        ));
    };
    match value.as_str() {
        "json" => Ok(ToolInputKind::Json),
        other => Err(node_error(
            input,
            node,
            format!("`input` must be `json`, found `{other}`"),
            None,
        )),
    }
}

/// `params { param "name" type="string" required=#true description="…" }` —
/// the tool's declared parameters, which form its JSON input schema and feed
/// the `{{param}}` templates in `cmd`.
fn parse_tool_params(node: &KdlNode, input: &str) -> Result<Option<BTreeMap<String, ToolParam>>> {
    reject_positionals(input, node)?;
    let mut params = BTreeMap::new();
    for child in child_nodes(node) {
        if child.name().value() != "param" {
            return Err(node_error(
                input,
                child,
                format!(
                    "unsupported `{}` (declare parameters with `param \"name\" …`)",
                    child.name().value()
                ),
                None,
            ));
        }
        let (name, param) = parse_tool_param(child, input)?;
        if params.insert(name.clone(), param).is_some() {
            return Err(duplicate(input, child, &name));
        }
    }
    Ok((!params.is_empty()).then_some(params))
}

/// One `param "name" type=…` entry; only `type` is required.
fn parse_tool_param(node: &KdlNode, input: &str) -> Result<(String, ToolParam)> {
    let name = single_positional_string(input, node, "param")?;
    if !valid_name(&name) {
        return Err(node_error(
            input,
            node,
            format!("param name `{name}` must contain only letters, digits, `_`, and `-`"),
            None,
        ));
    }
    let kind = match property_string(input, node, "type")?.as_deref() {
        Some("string") => ToolParamKind::String,
        Some("integer") => ToolParamKind::Integer,
        Some("number") => ToolParamKind::Number,
        Some("boolean") => ToolParamKind::Boolean,
        Some(other) => {
            return Err(node_error(
                input,
                node,
                format!(
                    "param `{name}` type must be `string`, `integer`, `number`, or `boolean`, found `{other}`"
                ),
                None,
            ));
        }
        None => {
            return Err(node_error(
                input,
                node,
                format!(
                    "param `{name}` requires a `type` property (\"string\", \"integer\", \"number\", or \"boolean\")"
                ),
                None,
            ));
        }
    };
    reject_unknown_props(input, node, &["type", "required", "description"])?;
    let required = property_bool(input, node, "required")?.unwrap_or(false);
    let description = property_string(input, node, "description")?;
    Ok((
        name,
        ToolParam {
            kind,
            required,
            description,
        },
    ))
}

/// `envs inherit=#true { env "NAME" "value" }` — the environment of a spawned
/// process. Values may carry `$VAR` / `${VAR}` placeholders.
fn parse_envs(node: &KdlNode, input: &str) -> Result<Option<EnvsConfig>> {
    reject_positionals(input, node)?;
    reject_unknown_props(input, node, &["inherit"])?;
    let inherit = property_bool(input, node, "inherit")?;
    let mut entries = BTreeMap::new();
    for child in child_nodes(node) {
        if child.name().value() != "env" {
            return Err(node_error(
                input,
                child,
                format!(
                    "unsupported `{}` (declare variables with `env \"NAME\" \"value\"`)",
                    child.name().value()
                ),
                None,
            ));
        }
        let (name, value) = env_entry_pair(input, child)?;
        if entries.insert(name.clone(), value).is_some() {
            return Err(duplicate(input, child, &name));
        }
    }
    if inherit.is_none() && entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(EnvsConfig {
        inherit: inherit.unwrap_or(true),
        entries,
    }))
}

/// One `env "NAME" "value"` entry: exactly two string arguments.
fn env_entry_pair(input: &str, node: &KdlNode) -> Result<(String, String)> {
    let mut positionals = node.entries().iter().filter(|entry| entry.name().is_none());
    let first = positionals.next();
    let second = positionals.next();
    if second.is_none() || positionals.next().is_some() {
        return Err(node_error(
            input,
            node,
            "`env` takes two string arguments (name and value)",
            None,
        ));
    }
    let (KdlValue::String(name), KdlValue::String(value)) =
        (first.unwrap().value(), second.unwrap().value())
    else {
        return Err(node_error(
            input,
            node,
            "`env` takes two string arguments (name and value)",
            None,
        ));
    };
    if !valid_env_name(name) {
        return Err(node_error(
            input,
            node,
            format!("`{name}` is not a valid environment variable name"),
            None,
        ));
    }
    Ok((name.clone(), value.clone()))
}

/// `mcp { stdio name="…" { … }; http name="…" { … } }` — MCP servers. Names
/// are unique across both transports because generated tool names are
/// `mcp__<server>__<tool>`.
fn parse_mcp(node: &KdlNode, input: &str) -> Result<McpConfig> {
    reject_positionals(input, node)?;
    let mut stdio = BTreeMap::new();
    let mut http = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "stdio" => {
                let (name, config) = parse_mcp_stdio(child, input)?;
                if stdio.contains_key(&name) || http.contains_key(&name) {
                    return Err(duplicate(input, child, &name));
                }
                stdio.insert(name, config);
            }
            "http" => {
                let (name, config) = parse_mcp_http(child, input)?;
                if stdio.contains_key(&name) || http.contains_key(&name) {
                    return Err(duplicate(input, child, &name));
                }
                http.insert(name, config);
            }
            _ => {}
        }
    }
    Ok(McpConfig { stdio, http })
}

/// `stdio name="…" { command "…"; args …; envs … }` — an MCP server spawned
/// as a child process.
fn parse_mcp_stdio(node: &KdlNode, input: &str) -> Result<(String, McpStdioConfig)> {
    reject_positionals(input, node)?;
    reject_unknown_props(input, node, &["name"])?;
    let name = name_prop(input, node, "mcp stdio server")?;
    let mut command = None;
    let mut args = None;
    let mut envs = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "command" => set_once(input, child, &mut command, scalar_string(input, child))?,
            "args" => set_once(input, child, &mut args, scalar_string_vec(input, child))?,
            "envs" => set_once(input, child, &mut envs, parse_envs(child, input))?,
            _ => {}
        }
    }
    let command = command.ok_or_else(|| {
        node_error(
            input,
            node,
            format!("mcp server `{name}` requires a `command`"),
            None,
        )
    })?;
    Ok((
        name,
        McpStdioConfig {
            command,
            args: args.unwrap_or_default(),
            envs: envs.unwrap_or_default(),
        },
    ))
}

/// `http name="…" { url "…"; headers { … } }` — an MCP server reached over
/// streamable HTTP.
fn parse_mcp_http(node: &KdlNode, input: &str) -> Result<(String, McpHttpConfig)> {
    reject_positionals(input, node)?;
    reject_unknown_props(input, node, &["name"])?;
    let name = name_prop(input, node, "mcp http server")?;
    let mut url = None;
    let mut headers = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "url" => set_once(input, child, &mut url, scalar_string(input, child))?,
            "headers" => set_once(
                input,
                child,
                &mut headers,
                parse_web_search_headers(child, input),
            )?,
            _ => {}
        }
    }
    let url = url.ok_or_else(|| {
        node_error(
            input,
            node,
            format!("mcp server `{name}` requires a `url`"),
            None,
        )
    })?;
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(node_error(
            input,
            node,
            format!("mcp server `{name}` url must start with http:// or https://"),
            None,
        ));
    }
    Ok((
        name,
        McpHttpConfig {
            url,
            headers: headers.unwrap_or_default(),
        },
    ))
}

/// The single `name` property of a named entry node (`tool name="…"`):
/// required, a string, and a valid identifier.
fn name_prop(input: &str, node: &KdlNode, what: &str) -> Result<String> {
    let Some(name) = property_string(input, node, "name")? else {
        return Err(node_error(
            input,
            node,
            format!("`{what}` requires a `name` property"),
            None,
        ));
    };
    if !valid_name(&name) {
        return Err(node_error(
            input,
            node,
            format!("`name` must contain only letters, digits, `_`, and `-`, found `{name}`"),
            None,
        ));
    }
    Ok(name)
}

/// The identifier charset shared by tool names, MCP server names, and param
/// names: the composite `mcp__<server>__<tool>` form must stay provider-safe.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A `NAME` usable as an environment variable: `[A-Za-z_][A-Za-z0-9_]*`.
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every `{{name}}` template reference inside one `cmd` argument. Text that
/// does not form a valid `{{name}}` is left alone (it stays literal).
fn cmd_placeholders(arg: &str) -> Vec<String> {
    let mut found = Vec::new();
    let bytes = arg.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{'
            && bytes[i + 1] == b'{'
            && let Some(rel) = arg[i + 2..].find("}}")
        {
            let inner = arg[i + 2..i + 2 + rel].trim();
            if valid_name(inner) {
                found.push(inner.to_string());
                i += 2 + rel + 2;
                continue;
            }
        }
        i += 1;
    }
    found
}

/// Every `{{param}}` reference in `cmd` must name a declared param.
fn validate_cmd_placeholders(
    input: &str,
    node: &KdlNode,
    cmd: &[String],
    params: &BTreeMap<String, ToolParam>,
) -> Result<()> {
    for arg in cmd {
        for placeholder in cmd_placeholders(arg) {
            if !params.contains_key(&placeholder) {
                return Err(node_error(
                    input,
                    node,
                    format!(
                        "`cmd` references `{{{{{placeholder}}}}}` but no `param` named `{placeholder}` is declared"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(())
}

fn reject_positionals(input: &str, node: &KdlNode) -> Result<()> {
    if node.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            node,
            format!("`{}` takes no positional arguments", node.name().value()),
            None,
        ));
    }
    Ok(())
}

fn reject_unknown_props(input: &str, node: &KdlNode, allowed: &[&str]) -> Result<()> {
    for entry in node.entries() {
        let Some(name) = entry.name() else {
            continue;
        };
        if !allowed.contains(&name.value()) {
            return Err(entry_error(
                input,
                entry,
                format!("unknown property `{}`", name.value()),
            ));
        }
    }
    Ok(())
}

fn entry_error(input: &str, entry: &KdlEntry, message: impl Into<String>) -> ConfigError {
    at(
        input,
        entry.span().offset(),
        entry.span().len(),
        message,
        None,
    )
}

/// The single required positional string argument of a node (`param "url"`).
fn single_positional_string(input: &str, node: &KdlNode, what: &str) -> Result<String> {
    let mut positionals = node.entries().iter().filter(|entry| entry.name().is_none());
    let Some(first) = positionals.next() else {
        return Err(node_error(
            input,
            node,
            format!("`{what}` takes a single string argument"),
            None,
        ));
    };
    if positionals.next().is_some() {
        return Err(node_error(
            input,
            node,
            format!("`{what}` takes a single argument"),
            None,
        ));
    }
    match first.value() {
        KdlValue::String(value) => Ok(value.clone()),
        _ => Err(node_error(
            input,
            node,
            format!("`{what}` must be a string"),
            None,
        )),
    }
}

fn parse_web_search(node: &KdlNode, input: &str) -> Result<WebSearchConfig> {
    let mut disabled = None;
    let mut url = None;
    let mut kind = None;
    let mut headers = None;
    let mut params = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
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
    // `url` and `type` come as a pair; omitted together they fall back to the
    // built-in DuckDuckGo Lite backend.
    let (url, kind) = match (url, kind) {
        (Some(url), Some(kind)) => {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(node_error(
                    input,
                    node,
                    "`web-search` url must start with http:// or https://",
                    None,
                ));
            }
            (url, kind)
        }
        (None, None) => (DUCKDUCKGO_LITE_URL.to_string(), WebSearchKind::ToMarkdown),
        (Some(_), None) => {
            return Err(node_error(
                input,
                node,
                "`web-search` requires a `type` (\"ollama\" or \"to_markdown\")",
                None,
            ));
        }
        (None, Some(_)) => {
            return Err(node_error(
                input,
                node,
                "`web-search` requires a `url`",
                None,
            ));
        }
    };
    let params = match params {
        Some((param_kind, map)) => WebSearchParams {
            kind: param_kind.unwrap_or_else(|| WebSearchParamKind::default_for(kind)),
            map,
        },
        None => WebSearchParams::default_for(kind),
    };
    Ok(WebSearchConfig {
        disabled: disabled.unwrap_or_default(),
        url,
        kind,
        headers: headers.unwrap_or_default(),
        params,
    })
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

/// The value of a single named integer property entry, when present.
fn property_usize(input: &str, node: &KdlNode, name: &str) -> Result<Option<usize>> {
    let mut found = None;
    for entry in node.entries() {
        if entry.name().is_some_and(|n| n.value() == name) {
            if found.is_some() {
                return Err(duplicate(input, node, name));
            }
            match entry.value() {
                KdlValue::Integer(value) => {
                    found = Some(usize::try_from(*value).map_err(|_| range_error(input, node))?);
                }
                _ => return Err(type_error(input, node, "an integer")),
            }
        }
    }
    Ok(found)
}

/// The value of a single named boolean property entry, when present.
fn property_bool(input: &str, node: &KdlNode, name: &str) -> Result<Option<bool>> {
    let mut found = None;
    for entry in node.entries() {
        if entry.name().is_some_and(|n| n.value() == name) {
            if found.is_some() {
                return Err(duplicate(input, node, name));
            }
            match entry.value() {
                KdlValue::Bool(value) => found = Some(*value),
                _ => return Err(type_error(input, node, "a boolean")),
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
    check_props(input, node, &["except-hidden", "exact", "mode"])?;
    let verb = rule_verb(input, node)?;
    let except_hidden = prop_bool(input, node, "except-hidden")?;
    let exact = prop_bool(input, node, "exact")?;
    let mode_raw = property_string(input, node, "mode")?;
    if mode_raw.is_some() && verb != Verb::Allow {
        return Err(node_error(
            input,
            node,
            "`mode` is only valid on `allow` rules",
            None,
        ));
    }
    let mode = match mode_raw.as_deref() {
        None => Mode::Rw,
        Some("rw") => Mode::Rw,
        Some("ro") => Mode::Ro,
        Some(other) => {
            return Err(node_error(
                input,
                node,
                format!("`mode` must be `ro` or `rw`, found `{other}`"),
                None,
            ));
        }
    };
    rule_patterns(input, node, "path")?
        .into_iter()
        .map(|path| {
            Ok(PathRule {
                verb,
                path,
                except_hidden: except_hidden.unwrap_or_default(),
                exact: exact.unwrap_or_default(),
                mode,
            })
        })
        .collect()
}

fn parse_shell_rules(node: &KdlNode, input: &str) -> Result<Vec<ShellRule>> {
    check_props(input, node, &["pattern"])?;
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
    rule_patterns(input, node, "pattern")?
        .into_iter()
        .map(|pattern| {
            Ok(ShellRule {
                verb,
                pattern,
                kind,
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

fn parse_scenes(node: &KdlNode, input: &str) -> Result<ScenesConfig> {
    let mut default = None;
    let mut scenes = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "default" => set_once(
                input,
                child,
                &mut default,
                parse_scene_name(input, child, "default"),
            )?,
            "scene" => {
                let (name, scene) = parse_scene(child, input)?;
                if scenes.insert(name.clone(), scene).is_some() {
                    return Err(duplicate(input, child, &format!("scene `{name}`")));
                }
            }
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!("unknown node `{other}` in `scenes` (expected `default` or `scene`)"),
                    None,
                ));
            }
        }
    }
    Ok(ScenesConfig { default, scenes })
}

fn parse_scene_name(input: &str, node: &KdlNode, what: &str) -> Result<Option<String>> {
    match scalar_string(input, node)? {
        None => Ok(None),
        Some(name) if !name.trim().is_empty() => Ok(Some(name)),
        Some(_) => Err(node_error(
            input,
            node,
            format!("`{what}` must name a scene"),
            None,
        )),
    }
}

fn parse_scene(node: &KdlNode, input: &str) -> Result<(String, SceneConfig)> {
    check_props(input, node, &["name"])?;
    if node.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            node,
            "`scene` takes no positional arguments (name the scene with a `name` property)",
            None,
        ));
    }
    let Some(name) = property_string(input, node, "name")? else {
        return Err(node_error(
            input,
            node,
            "`scene` requires a `name` property",
            None,
        ));
    };
    if name.trim().is_empty() {
        return Err(node_error(input, node, "`scene` must name a scene", None));
    }

    let mut description = None;
    let mut subagents = None;
    let mut system_prompts = None;
    let mut thinking = None;
    let mut tools = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "description" => set_once(input, child, &mut description, scalar_string(input, child))?,
            "subagents" => set_once(
                input,
                child,
                &mut subagents,
                parse_subagents(child, input).map(Some),
            )?,
            "system-prompts" => set_once(
                input,
                child,
                &mut system_prompts,
                parse_system_prompts(child, input).map(Some),
            )?,
            "thinking" => set_once(input, child, &mut thinking, scalar_bool(input, child))?,
            "tools" => set_once(
                input,
                child,
                &mut tools,
                parse_scene_tools(child, input).map(Some),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `scene` (expected `description`, \
                         `subagents`, `system-prompts`, `thinking`, or `tools`)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok((
        name,
        SceneConfig {
            description,
            subagents: subagents.unwrap_or_default(),
            system_prompts: system_prompts.unwrap_or_default(),
            thinking,
            tools: tools.unwrap_or_default(),
        },
    ))
}

fn parse_themes(node: &KdlNode, input: &str) -> Result<ThemesConfig> {
    let mut themes = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "theme" => {
                let (name, variant, theme) = parse_theme(child, input)?;
                let key = (name, variant);
                if themes.insert(key.clone(), theme).is_some() {
                    return Err(duplicate(
                        input,
                        child,
                        &format!("theme `{}`", theme_key_label(&key)),
                    ));
                }
            }
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!("unknown node `{other}` in `themes` (expected `theme`)"),
                    None,
                ));
            }
        }
    }
    Ok(ThemesConfig { themes })
}

fn parse_theme(node: &KdlNode, input: &str) -> Result<(String, Option<String>, ThemeDef)> {
    check_props(input, node, &["name", "variant", "mode"])?;
    if node.entries().iter().any(|entry| entry.name().is_none()) {
        return Err(node_error(
            input,
            node,
            "`theme` takes no positional arguments (name the theme with a `name` property)",
            None,
        ));
    }
    let Some(name) = property_string(input, node, "name")? else {
        return Err(node_error(
            input,
            node,
            "`theme` requires a `name` property",
            None,
        ));
    };
    if name.trim().is_empty() {
        return Err(node_error(input, node, "`theme` must name a theme", None));
    }
    let variant = match property_string(input, node, "variant")? {
        Some(variant) if variant.trim().is_empty() => {
            return Err(node_error(
                input,
                node,
                "`variant` must name a variant",
                None,
            ));
        }
        variant => variant,
    };
    let mode = match property_string(input, node, "mode")? {
        Some(text) => ThemeVariant::parse(&text)
            .ok_or_else(|| node_error(input, node, "`mode` must be `dark` or `light`", None))?,
        None => ThemeVariant::Dark,
    };

    let mut colors = BTreeMap::new();
    for child in child_nodes(node) {
        let role = child.name().value();
        if !THEME_ROLES.contains(&role) {
            return Err(node_error(
                input,
                child,
                format!(
                    "unknown color `{role}` in `theme` (expected one of {})",
                    THEME_ROLES
                        .iter()
                        .map(|r| format!("`{r}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
            ));
        }
        let value = match scalar_string(input, child)? {
            Some(text) => parse_hex_color(input, child, &text)?,
            None => {
                return Err(node_error(
                    input,
                    child,
                    format!("`{role}` requires a color value"),
                    None,
                ));
            }
        };
        if !child_nodes(child).is_empty() {
            return Err(node_error(
                input,
                child,
                format!("`{role}` takes no children"),
                None,
            ));
        }
        if colors.insert(role.to_string(), value).is_some() {
            return Err(duplicate(input, child, &format!("color `{role}`")));
        }
    }
    let def = ThemeDef {
        colors,
        variant: variant.clone(),
        mode,
    };
    Ok((name, variant, def))
}

/// Parses `#rgb` or `#rrggbb` (the leading `#` optional) into an RGB triple.
fn parse_hex_color(input: &str, node: &KdlNode, text: &str) -> Result<Rgb> {
    let text = text.strip_prefix('#').unwrap_or(text);
    let value = match text.len() {
        3 => {
            let channel = |c: char| u8::from_str_radix(&c.to_string(), 16).ok();
            let mut chars = text.chars();
            match (
                chars.next().and_then(channel),
                chars.next().and_then(channel),
                chars.next().and_then(channel),
            ) {
                (Some(r), Some(g), Some(b)) if chars.next().is_none() => {
                    Some((r * 17, g * 17, b * 17))
                }
                _ => None,
            }
        }
        6 if text.is_ascii() => {
            let pair = |i: usize| u8::from_str_radix(&text[i..i + 2], 16).ok();
            match (pair(0), pair(2), pair(4)) {
                (Some(r), Some(g), Some(b)) => Some((r, g, b)),
                _ => None,
            }
        }
        _ => None,
    };
    value.ok_or_else(|| {
        node_error(
            input,
            node,
            format!(
                "`{}` must be a hex color like `#1a2b3c` (found `{text}`)",
                node.name().value()
            ),
            None,
        )
    })
}

fn parse_subagents(node: &KdlNode, input: &str) -> Result<SubagentsConfig> {
    let mut disabled = None;
    let mut workers = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
            name => {
                let worker = parse_subagent(child, input)?;
                if workers.insert(name.to_string(), worker).is_some() {
                    return Err(duplicate(input, child, name));
                }
            }
        }
    }
    Ok(SubagentsConfig {
        disabled: disabled.unwrap_or_default(),
        workers,
    })
}

fn parse_subagent(node: &KdlNode, input: &str) -> Result<SubagentConfig> {
    if !node.entries().is_empty() {
        return Err(node_error(
            input,
            node,
            format!(
                "`{}` takes no arguments (name the worker with the node itself)",
                node.name().value()
            ),
            None,
        ));
    }
    let mut disabled = None;
    let mut thinking = None;
    let mut system_prompts = None;
    let mut tools = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
            "thinking" => set_once(input, child, &mut thinking, scalar_bool(input, child))?,
            "system-prompts" => set_once(
                input,
                child,
                &mut system_prompts,
                parse_system_prompts(child, input).map(Some),
            )?,
            "tools" => set_once(
                input,
                child,
                &mut tools,
                parse_scene_tools(child, input).map(Some),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `{}` (expected `disabled`, `thinking`, \
                         `system-prompts`, or `tools`)",
                        node.name().value()
                    ),
                    None,
                ));
            }
        }
    }
    Ok(SubagentConfig {
        disabled: disabled.unwrap_or_default(),
        thinking,
        system_prompts: system_prompts.unwrap_or_default(),
        tools: tools.unwrap_or_default(),
    })
}

fn parse_system_prompts(node: &KdlNode, input: &str) -> Result<SystemPromptsConfig> {
    let mut prelude = None;
    let mut interlude = None;
    let mut before_each = None;
    let mut after_each = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "prelude" => set_once(
                input,
                child,
                &mut prelude,
                parse_prompt_text(input, child, "prelude"),
            )?,
            "interlude" => set_once(
                input,
                child,
                &mut interlude,
                parse_prompt_text(input, child, "interlude"),
            )?,
            "before-each" => set_once(
                input,
                child,
                &mut before_each,
                parse_hooks(child, input).map(Some),
            )?,
            "after-each" => set_once(
                input,
                child,
                &mut after_each,
                parse_hooks(child, input).map(Some),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `system-prompts` (expected `prelude`, \
                         `interlude`, `before-each`, or `after-each`)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(SystemPromptsConfig {
        prelude,
        interlude,
        before_each,
        after_each,
    })
}

fn parse_prompt_text(input: &str, node: &KdlNode, what: &str) -> Result<Option<String>> {
    match scalar_string(input, node)? {
        None => Ok(None),
        Some(text) if !text.trim().is_empty() => Ok(Some(text)),
        Some(_) => Err(node_error(
            input,
            node,
            format!("`{what}` must not be empty"),
            None,
        )),
    }
}

fn parse_hooks(node: &KdlNode, input: &str) -> Result<Hooks> {
    let mut user_prompt = None;
    let mut assistant_prompt = None;
    let mut turn = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "user-prompt" => set_once(
                input,
                child,
                &mut user_prompt,
                parse_prompt_text(input, child, "user-prompt"),
            )?,
            "assistant-prompt" => set_once(
                input,
                child,
                &mut assistant_prompt,
                parse_prompt_text(input, child, "assistant-prompt"),
            )?,
            "turn" => set_once(
                input,
                child,
                &mut turn,
                parse_prompt_text(input, child, "turn"),
            )?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `{}` (expected `user-prompt`, \
                         `assistant-prompt`, or `turn`)",
                        node.name().value()
                    ),
                    None,
                ));
            }
        }
    }
    Ok(Hooks {
        user_prompt,
        assistant_prompt,
        turn,
    })
}

fn parse_scene_tools(node: &KdlNode, input: &str) -> Result<SceneToolsConfig> {
    let mut verb = None;
    let mut tools = BTreeMap::new();
    for child in child_nodes(node) {
        match child.name().value() {
            "enable-all" | "ask-all" | "disable-all" => {
                let parsed = parse_scene_verb(input, child)?;
                set_once(input, child, &mut verb, Ok(Some(parsed)))?;
            }
            "tool" => {
                for (name, tool) in parse_tool_overrides(child, input)? {
                    if tools.insert(name.clone(), tool).is_some() {
                        return Err(duplicate(input, child, &format!("tool `{name}`")));
                    }
                }
            }
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!(
                        "unknown node `{other}` in `tools` (expected `enable-all`, \
                         `ask-all`, `disable-all`, or `tool`)"
                    ),
                    None,
                ));
            }
        }
    }
    Ok(SceneToolsConfig { verb, tools })
}

fn parse_scene_verb(input: &str, node: &KdlNode) -> Result<SceneToolVerb> {
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
        "enable-all" => Ok(SceneToolVerb::EnableAll),
        "ask-all" => Ok(SceneToolVerb::AskAll),
        "disable-all" => Ok(SceneToolVerb::DisableAll),
        other => Err(node_error(
            input,
            node,
            format!("`{other}` is not a scene tool verb"),
            None,
        )),
    }
}

/// One `tool` node: several tool name arguments sharing one override
/// (`tool "a" "b" { disabled }` ≡ two entries).
fn parse_tool_overrides(node: &KdlNode, input: &str) -> Result<Vec<(String, ToolOverride)>> {
    check_props(input, node, &[])?;
    let mut names = Vec::new();
    for entry in node.entries().iter().filter(|entry| entry.name().is_none()) {
        match entry.value() {
            KdlValue::String(name) if !name.trim().is_empty() => names.push(name.clone()),
            _ => {
                return Err(node_error(
                    input,
                    node,
                    "`tool` requires non-empty tool name arguments",
                    None,
                ));
            }
        }
    }
    if names.is_empty() {
        return Err(node_error(
            input,
            node,
            "`tool` requires at least one tool name argument",
            None,
        ));
    }
    let mut disabled = None;
    let mut ask = None;
    for child in child_nodes(node) {
        match child.name().value() {
            "disabled" => set_once(input, child, &mut disabled, switch_flag(input, child))?,
            "ask" => set_once(input, child, &mut ask, scalar_bool(input, child))?,
            other => {
                return Err(node_error(
                    input,
                    child,
                    format!("unknown node `{other}` in `tool` (expected `disabled` or `ask`)"),
                    None,
                ));
            }
        }
    }
    if disabled.is_none() && ask.is_none() {
        return Err(node_error(
            input,
            node,
            "`tool` override requires `disabled` or `ask`",
            None,
        ));
    }
    Ok(names
        .into_iter()
        .map(|name| (name, ToolOverride { disabled, ask }))
        .collect())
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
        scenes_node(&config.scenes),
        themes_node(&config.themes),
        retry_node(&config.retry),
    ];
    for node in sections.into_iter().flatten() {
        doc.nodes_mut().push(node);
    }
    autoformat(&mut doc);
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
    if let Some(theme) = &cfg.theme {
        children.push(value_node("theme", theme.as_str()));
    }
    if cfg.title != defaults.title {
        children.push(title_node(&cfg.title));
    }
    section_node("ui", children)
}

/// Builds `title { … }` — the session title settings. Only called with a
/// non-default config, so the node always gets content.
fn title_node(cfg: &TitleConfig) -> KdlNode {
    let defaults = TitleConfig::default();
    let mut title = KdlNode::new("title");
    if cfg.max_chars != defaults.max_chars {
        title.push(KdlEntry::new_prop("max-chars", cfg.max_chars as i128));
    }
    let mut children = Vec::new();
    if cfg.auto_gen {
        children.push(KdlNode::new("auto-gen"));
    }
    let mut fields = Vec::new();
    if let Some(provider) = &cfg.provider {
        fields.push(value_node("provider", provider.as_str()));
    }
    if let Some(model) = &cfg.model {
        fields.push(value_node("model", model.as_str()));
    }
    if let Some(system_prompt) = &cfg.system_prompt {
        fields.push(prompt_node("system-prompt", system_prompt));
    }
    if !fields.is_empty() {
        let mut llm = KdlNode::new("llm");
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(fields);
        llm.set_children(body);
        children.push(llm);
    }
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        title.set_children(body);
    }
    title
}

fn embedding_node(cfg: &EmbeddingConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(KdlNode::new("disabled"));
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
        children.push(KdlNode::new("disabled"));
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
        children.push(KdlNode::new("disabled"));
    }
    children.extend(string_vec_node("dirs", &cfg.dirs));
    section_node("skills", children)
}

fn context_node(cfg: &ContextConfig) -> Option<KdlNode> {
    let defaults = ContextConfig::default();
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(KdlNode::new("disabled"));
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
            |rule| (rule.verb, rule.except_hidden, rule.exact, rule.mode),
            path_rule_group,
        ));
    }
    if cfg.shell.default.is_some() || !cfg.shell.rules.is_empty() {
        children.push(scope_node(
            "shell-patterns",
            &cfg.shell,
            |rule| (rule.verb, rule.kind),
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
    if first.mode == Mode::Ro {
        node.push(KdlEntry::new_prop("mode", "ro"));
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
    for rule in rules {
        node.push(KdlEntry::new(rule.pattern.as_str()));
    }
    node
}

fn tools_node(cfg: &ToolsConfig) -> Option<KdlNode> {
    let mut children = Vec::new();
    if let Some(web) = &cfg.web_search {
        children.push(web_search_node(web));
    }
    for (name, tool) in &cfg.tools {
        children.push(tool_node(name, tool));
    }
    if let Some(mcp) = mcp_node(&cfg.mcp) {
        children.push(mcp);
    }
    section_node("tools", children)
}

fn tool_node(name: &str, cfg: &StdioToolConfig) -> KdlNode {
    let mut children = Vec::new();
    if let Some(description) = &cfg.description {
        children.push(prompt_node("description", description.as_str()));
    }
    children.push(string_vec_node("cmd", &cfg.cmd).expect("parse guarantees a non-empty `cmd`"));
    if cfg.input == Some(ToolInputKind::Json) {
        children.push(value_node("input", "json"));
    }
    if cfg.timeout_secs != TOOL_DEFAULT_TIMEOUT_SECS {
        children.push(int_node("timeout", cfg.timeout_secs));
    }
    if !cfg.params.is_empty() {
        let mut params = KdlNode::new("params");
        let mut body = KdlDocument::new();
        for (param_name, param) in &cfg.params {
            body.nodes_mut().push(tool_param_node(param_name, param));
        }
        params.set_children(body);
        children.push(params);
    }
    if let Some(envs) = envs_node(&cfg.envs) {
        children.push(envs);
    }
    node_with_prop("tool", "name", name, children)
}

fn tool_param_node(name: &str, param: &ToolParam) -> KdlNode {
    let mut node = KdlNode::new("param");
    node.push(string_entry(name));
    node.push(KdlEntry::new_prop("type", param.kind.as_str()));
    if param.required {
        node.push(KdlEntry::new_prop("required", true));
    }
    if let Some(description) = &param.description {
        node.push(KdlEntry::new_prop("description", description.as_str()));
    }
    node
}

/// Builds `envs { … }`; `None` when the config holds defaults (inherit on,
/// no variables).
fn envs_node(cfg: &EnvsConfig) -> Option<KdlNode> {
    if cfg.is_default() {
        return None;
    }
    let mut node = KdlNode::new("envs");
    if !cfg.inherit {
        node.push(KdlEntry::new_prop("inherit", false));
    }
    let mut body = KdlDocument::new();
    for (name, value) in &cfg.entries {
        let mut env = KdlNode::new("env");
        env.push(string_entry(name));
        env.push(string_entry(value));
        body.nodes_mut().push(env);
    }
    node.set_children(body);
    Some(node)
}

fn mcp_node(cfg: &McpConfig) -> Option<KdlNode> {
    if cfg.is_empty() {
        return None;
    }
    let mut children = Vec::new();
    for (name, server) in &cfg.stdio {
        children.push(mcp_stdio_node(name, server));
    }
    for (name, server) in &cfg.http {
        children.push(mcp_http_node(name, server));
    }
    section_node("mcp", children)
}

fn mcp_stdio_node(name: &str, cfg: &McpStdioConfig) -> KdlNode {
    let mut children = vec![value_node("command", cfg.command.as_str())];
    if let Some(args) = string_vec_node("args", &cfg.args) {
        children.push(args);
    }
    if let Some(envs) = envs_node(&cfg.envs) {
        children.push(envs);
    }
    node_with_prop("stdio", "name", name, children)
}

fn mcp_http_node(name: &str, cfg: &McpHttpConfig) -> KdlNode {
    let mut children = vec![value_node("url", cfg.url.as_str())];
    if !cfg.headers.is_empty() {
        let mut headers = KdlNode::new("headers");
        let mut body = KdlDocument::new();
        for (header, value) in &cfg.headers {
            body.nodes_mut().push(value_node(header, value.as_str()));
        }
        headers.set_children(body);
        children.push(headers);
    }
    node_with_prop("http", "name", name, children)
}

fn web_search_node(cfg: &WebSearchConfig) -> KdlNode {
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(KdlNode::new("disabled"));
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

fn scenes_node(cfg: &ScenesConfig) -> Option<KdlNode> {
    if cfg.default.is_none() && cfg.scenes.is_empty() {
        return None;
    }
    let mut children = Vec::new();
    if let Some(default) = &cfg.default {
        children.push(value_node("default", default.as_str()));
    }
    for (name, scene) in &cfg.scenes {
        children.push(scene_node(name, scene));
    }
    section_node("scenes", children)
}

fn themes_node(cfg: &ThemesConfig) -> Option<KdlNode> {
    if cfg.themes.is_empty() {
        return None;
    }
    let mut children = Vec::new();
    for (key, theme) in &cfg.themes {
        children.push(theme_node(&key.0, theme));
    }
    section_node("themes", children)
}

fn theme_node(name: &str, theme: &ThemeDef) -> KdlNode {
    let children: Vec<KdlNode> = theme
        .colors
        .iter()
        .map(|(role, (r, g, b))| value_node(role, format!("#{r:02x}{g:02x}{b:02x}")))
        .collect();
    let mut node = KdlNode::new("theme");
    node.push(KdlEntry::new_prop("name", name));
    if let Some(variant) = &theme.variant {
        node.push(KdlEntry::new_prop("variant", variant.as_str()));
    }
    if theme.mode != ThemeVariant::Dark {
        node.push(KdlEntry::new_prop("mode", theme.mode.as_str()));
    }
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}

fn scene_node(name: &str, scene: &SceneConfig) -> KdlNode {
    let mut children = Vec::new();
    if let Some(description) = &scene.description {
        children.push(prompt_node("description", description));
    }
    children.extend(subagents_node(&scene.subagents));
    children.extend(system_prompts_node(&scene.system_prompts));
    if let Some(thinking) = scene.thinking {
        children.push(value_node("thinking", thinking));
    }
    children.extend(scene_tools_node(&scene.tools));
    node_with_prop("scene", "name", name, children)
}

fn subagents_node(cfg: &SubagentsConfig) -> Option<KdlNode> {
    if cfg.is_default() {
        return None;
    }
    let mut children = Vec::new();
    if cfg.disabled {
        children.push(KdlNode::new("disabled"));
    }
    for (name, worker) in &cfg.workers {
        children.push(subagent_node(name, worker));
    }
    section_node("subagents", children)
}

fn subagent_node(name: &str, worker: &SubagentConfig) -> KdlNode {
    let mut children = Vec::new();
    if worker.disabled {
        children.push(KdlNode::new("disabled"));
    }
    children.extend(system_prompts_node(&worker.system_prompts));
    if let Some(thinking) = worker.thinking {
        children.push(value_node("thinking", thinking));
    }
    children.extend(scene_tools_node(&worker.tools));
    named_node(name, children)
}

fn system_prompts_node(cfg: &SystemPromptsConfig) -> Option<KdlNode> {
    if cfg.is_default() {
        return None;
    }
    let mut children = Vec::new();
    if let Some(text) = &cfg.prelude {
        children.push(prompt_node("prelude", text));
    }
    if let Some(text) = &cfg.interlude {
        children.push(prompt_node("interlude", text));
    }
    children.extend(hooks_node("before-each", &cfg.before_each));
    children.extend(hooks_node("after-each", &cfg.after_each));
    section_node("system-prompts", children)
}

fn hooks_node(name: &str, hooks: &Option<Hooks>) -> Option<KdlNode> {
    let hooks = hooks.as_ref()?;
    if hooks.is_default() {
        return None;
    }
    let mut children = Vec::new();
    if let Some(text) = &hooks.user_prompt {
        children.push(prompt_node("user-prompt", text));
    }
    if let Some(text) = &hooks.assistant_prompt {
        children.push(prompt_node("assistant-prompt", text));
    }
    if let Some(text) = &hooks.turn {
        children.push(prompt_node("turn", text));
    }
    section_node(name, children)
}

fn scene_tools_node(cfg: &SceneToolsConfig) -> Option<KdlNode> {
    if cfg.is_default() {
        return None;
    }
    let mut children = Vec::new();
    if let Some(verb) = cfg.verb {
        children.push(KdlNode::new(verb.as_str()));
    }
    for (name, tool) in &cfg.tools {
        children.push(tool_override_node(name, tool));
    }
    section_node("tools", children)
}

fn tool_override_node(name: &str, tool: &ToolOverride) -> KdlNode {
    let mut node = KdlNode::new("tool");
    node.push(KdlEntry::new(name.to_string()));
    let mut children = Vec::new();
    if tool.disabled == Some(true) {
        children.push(KdlNode::new("disabled"));
    }
    if let Some(ask) = tool.ask {
        children.push(value_node("ask", ask));
    }
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}

fn named_node(name: &str, children: Vec<KdlNode>) -> KdlNode {
    let mut node = KdlNode::new(name);
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}

fn node_with_prop(name: &str, prop: &str, value: &str, children: Vec<KdlNode>) -> KdlNode {
    let mut node = KdlNode::new(name);
    node.push(KdlEntry::new_prop(prop, value));
    if !children.is_empty() {
        let mut body = KdlDocument::new();
        body.nodes_mut().extend(children);
        node.set_children(body);
    }
    node
}

fn prompt_node(name: &str, text: &str) -> KdlNode {
    let mut node = KdlNode::new(name);
    node.push(string_entry(text));
    node
}

/// A string entry; text with newlines renders as a KDL multi-line string
/// (`"""`) so prompts stay readable in the saved file.
fn string_entry(text: &str) -> KdlEntry {
    let mut entry = KdlEntry::new(text);
    if let Some(repr) = multiline_string_repr(text) {
        let mut format = KdlEntryFormat {
            value_repr: repr,
            leading: " ".into(),
            ..KdlEntryFormat::default()
        };
        format.autoformat_keep = true;
        entry.set_format(format);
    }
    entry
}

/// The `"""`-wrapped literal for a multi-line string, or `None` when the text
/// has no newlines or cannot be represented raw (a `"""` sequence, a `\r`, or
/// another control character) — those fall back to an escaped single line.
fn multiline_string_repr(text: &str) -> Option<String> {
    let problematic = text.contains("\"\"\"")
        || text.contains('\r')
        || text.chars().any(|c| c != '\n' && c.is_control());
    if !text.contains('\n') || problematic {
        return None;
    }
    Some(format!("\"\"\"\n{}\n\"\"\"", text.replace('\\', "\\\\")))
}

fn registry_entry_node(name: &str, entry: RegistryEntry) -> KdlNode {
    let mut children = Vec::new();
    if entry.disabled {
        children.push(KdlNode::new("disabled"));
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
