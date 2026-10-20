//! Rendering the catalog into the tool's description.
//!
//! The description *is* the interface: the model reads a Rhai signature per
//! tool and calls it, so the signature has to be faithful to the schema and
//! short enough to pay for itself. Declarations are inlined up to a budget of
//! estimated tokens; whatever does not fit stays callable and reachable by name.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::doc;
use crate::spec::{Catalog, ToolSpec};

/// What a declaration says when the model cannot call the tool itself.
const SCRIPT_ONLY: &str = " (script only)";

/// How much of the catalog is inlined, and how much of a tool's own prose
/// travels with its signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderOptions {
    /// Estimated tokens of inlined declarations. The rest are listed by name
    /// only, so a big MCP roster costs a fraction of its signatures.
    pub inline_budget: usize,
    /// How much of a tool's description is shown beside its signature.
    pub brief_chars: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        // Pi's number, and Pi's heuristic for counting: a token is about four
        // characters. It is an estimate on purpose -- a real tokenizer would
        // couple this crate to one provider's encoding.
        Self {
            inline_budget: 3000,
            brief_chars: 120,
        }
    }
}

/// Estimated tokens of a piece of the description.
pub fn estimated_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

/// `tools::read_file(path: String, offset?: int, limit?: int) -> Dynamic`
///
/// The parameters are the tool's JSON Schema properties, in schema order, with
/// `?` on everything the schema does not require. The return is always
/// `Dynamic`: a tool's output is its text, or a map when it returned
/// structured content, and only the tool knows which.
pub fn signature(spec: &ToolSpec) -> String {
    let properties = spec.schema.get("properties").and_then(Value::as_object);
    let required: Vec<&str> = spec
        .schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut parameters = Vec::new();
    // Sorted, not in schema order: the parameters are a set, and a stable order
    // keeps the description (and its golden test) independent of how
    // `serde_json` was built for this particular target.
    let mut properties: Vec<(&String, &Value)> = properties.into_iter().flatten().collect();
    properties.sort_by_key(|(name, _)| name.as_str());
    for (name, schema) in properties {
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        parameters.push(format!(
            "{}{optional}: {}",
            argument_key(name),
            type_of(schema)
        ));
    }

    format!("{}({}) -> Dynamic", spec.qualified(), parameters.join(", "))
}

/// A map key in the signature: bare when it is an identifier, quoted otherwise
/// (a script writes it either way, and MCP argument names are not ours).
fn argument_key(name: &str) -> String {
    let identifier = !name.is_empty()
        && !name.starts_with(|ch: char| ch.is_ascii_digit())
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if identifier {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

/// A JSON Schema type as the signature writes it.
///
/// Shared with `describe_tool`, so a parameter's fallback description is
/// written in the same notation as the signature it sits under.
pub fn type_of(schema: &Value) -> String {
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.is_empty()
    {
        let mut literals: Vec<String> = values.iter().map(literal).collect();
        literals.dedup();
        return literals.join(" | ");
    }

    for union in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(union).and_then(Value::as_array)
            && !variants.is_empty()
        {
            let mut parts: Vec<String> = variants.iter().map(type_of).collect();
            parts.dedup();
            return parts.join(" | ");
        }
    }

    let named = |name: &str| match name {
        "string" => Some("String".to_string()),
        "integer" => Some("int".to_string()),
        "number" => Some("float".to_string()),
        "boolean" => Some("bool".to_string()),
        // `null` has no Rhai value: it is `()`, which is also what a missing
        // map key reads as.
        "null" => Some("()".to_string()),
        "object" => Some("Map".to_string()),
        "array" => Some(match schema.get("items") {
            Some(items) => format!("Array<{}>", type_of(items)),
            None => "Array<Dynamic>".to_string(),
        }),
        _ => None,
    };

    match schema.get("type") {
        Some(Value::String(name)) => named(name).unwrap_or_else(|| "Dynamic".to_string()),
        // `"type": ["string", "null"]` is a nullable argument; the union says so.
        Some(Value::Array(names)) => {
            let mut parts: Vec<String> = names
                .iter()
                .filter_map(Value::as_str)
                .map(|name| named(name).unwrap_or_else(|| "Dynamic".to_string()))
                .collect();
            parts.dedup();
            match parts.len() {
                0 => "Dynamic".to_string(),
                _ => parts.join(" | "),
            }
        }
        // No type at all: the schema leaves it to the tool, and so do we.
        _ => "Dynamic".to_string(),
    }
}

/// An enum member as a signature literal.
fn literal(value: &Value) -> String {
    match value {
        Value::String(text) => format!("\"{text}\""),
        other => other.to_string(),
    }
}

/// The first line of a description, cut to fit beside a signature.
pub fn brief(description: &str, max_chars: usize) -> String {
    let first = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    crate::convert::truncate(first, max_chars)
}

/// The `codemode` tool's description for one turn's catalog: the dialect card,
/// then as much of the catalog as the budget pays for.
///
/// `script_only` holds the roster names the model's own tool list does not
/// offer, so a tool reachable only from a script says so. Without the mark the
/// model has to infer the split by diffing the catalog against its function
/// schemas — whose names differ (`mcp__dev__x` vs `mcp::dev::x`) — and in `only`
/// mode there is nothing to diff against.
pub fn describe(
    catalog: &Catalog,
    options: &RenderOptions,
    script_only: &BTreeSet<String>,
) -> String {
    let marked = |spec: &ToolSpec| script_only.contains(&spec.host_name);
    let mut inlined = Vec::new();
    let mut deferred = Vec::new();
    let mut spent = 0;
    let mut any_marked = false;

    for spec in catalog.tools() {
        let mark = marked(spec);
        any_marked |= mark;
        let line = declaration(spec, options, mark);
        let cost = estimated_tokens(&line) + 1;
        if spent + cost <= options.inline_budget {
            spent += cost;
            inlined.push(line);
        } else {
            deferred.push(spec);
        }
    }

    let mut out = String::new();
    out.push_str(doc::INTRO);
    out.push_str("\n\n");
    out.push_str(doc::CARD);
    out.push('\n');
    out.push_str(&format!("\n# Tools ({})\n\n", catalog.len()));

    if inlined.is_empty() {
        out.push_str(
            "Every tool is listed by name below; call `describe_tool(name)` \
                      for a signature.\n",
        );
    } else {
        out.push_str(&inlined.join("\n"));
        out.push('\n');
    }

    if any_marked {
        out.push_str(
            "\nTools marked `script only` are not on your own roster: call them from a script. \
             Every other tool can also be called directly.\n",
        );
    }

    if !deferred.is_empty() {
        out.push_str(&format!("\n# Also callable ({})\n\n", deferred.len()));
        let (plain, script): (Vec<&ToolSpec>, Vec<&ToolSpec>) =
            deferred.iter().partition(|spec| !marked(spec));
        if !plain.is_empty() {
            out.push_str(&name_listing(&plain, options.inline_budget));
        }
        if !script.is_empty() {
            // A name list carries no room for a per-name mark, so the script-only
            // ones get a listing of their own.
            let lead = if plain.is_empty() { "" } else { "\n" };
            out.push_str(&format!("{lead}Script only ({}): ", script.len()));
            out.push_str(&name_listing(&script, options.inline_budget));
        }
        out.push_str(
            "\nCall `describe_tool(name)` for a signature, or \
             `search_tools(query)` to find one.\n",
        );
    }

    out
}

/// One inlined declaration: the signature, its brief, and the roster name when
/// the callable name had to change.
fn declaration(spec: &ToolSpec, options: &RenderOptions, script_only: bool) -> String {
    let mut line = format!(
        "{}\n    {}",
        signature(spec),
        brief(&spec.description, options.brief_chars)
    );
    if spec.renamed() {
        line.push_str(&format!(" (tool name \"{}\")", spec.name));
    }
    if script_only {
        line.push_str(SCRIPT_ONLY);
    }
    line
}

/// The deferred tools by name — or, when even the names would not fit the
/// budget, a count per namespace, which is enough to aim a `search_tools` at one
/// of them, rather than spending the whole budget on names.
fn name_listing(deferred: &[&ToolSpec], budget: usize) -> String {
    let names: Vec<String> = deferred.iter().map(|spec| spec.qualified()).collect();
    let listing = names.join(", ");
    if estimated_tokens(&listing) <= budget {
        format!("{listing}\n")
    } else {
        namespace_summary(deferred)
    }
}

/// What is behind a roster too large to list: a count per namespace, which is
/// enough to aim a `search_tools` at one of them.
fn namespace_summary(deferred: &[&ToolSpec]) -> String {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for spec in deferred {
        let namespace = spec.namespace.join("::");
        match counts.last_mut() {
            Some((name, count)) if *name == namespace => *count += 1,
            _ => counts.push((namespace, 1)),
        }
    }
    let summary = counts
        .iter()
        .map(|(namespace, count)| format!("{namespace} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{summary}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ToolEntry;
    use serde_json::json;

    fn spec(name: &str, schema: Value) -> ToolSpec {
        let mut tools = Catalog::new([ToolEntry::roster(name, "Does a thing.", schema)])
            .tools()
            .to_vec();
        tools.remove(0)
    }

    #[test]
    fn signatures_follow_the_schema() {
        assert_eq!(
            signature(&spec(
                "read_file",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "offset": { "type": "integer" },
                        "limit": { "type": "integer" },
                    },
                    "required": ["path"],
                }),
            )),
            "tools::read_file(limit?: int, offset?: int, path: String) -> Dynamic"
        );

        // No arguments at all, and a schema that describes none.
        assert_eq!(
            signature(&spec("todos", json!({}))),
            "tools::todos() -> Dynamic"
        );
        assert_eq!(
            signature(&spec("todos", json!({ "type": "object" }))),
            "tools::todos() -> Dynamic"
        );
    }

    #[test]
    fn signature_types_cover_the_schema_shapes_mcp_servers_use() {
        let schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "mode": { "enum": ["fast", "thorough"] },
                "level": { "enum": [1, 2, 3] },
                "paths": { "type": "array", "items": { "type": "string" } },
                "bare": { "type": "array" },
                "options": { "type": "object" },
                "ratio": { "type": "number" },
                "force": { "type": "boolean" },
                "either": { "anyOf": [{ "type": "string" }, { "type": "integer" }] },
                "nullable": { "type": ["string", "null"] },
                "anything": {},
                "nothing": { "type": "null" },
            },
        });
        assert_eq!(
            signature(&spec("search", schema)),
            "tools::search(anything?: Dynamic, bare?: Array<Dynamic>, either?: String | int, \
             force?: bool, level?: 1 | 2 | 3, mode?: \"fast\" | \"thorough\", nothing?: (), \
             nullable?: String | (), options?: Map, paths?: Array<String>, query?: String, \
             ratio?: float) -> Dynamic"
        );
    }

    #[test]
    fn argument_keys_that_are_not_identifiers_are_quoted() {
        let schema = json!({
            "type": "object",
            "properties": { "repo-name": { "type": "string" }, "2fa": { "type": "boolean" } },
        });
        assert_eq!(
            signature(&spec("search", schema)),
            "tools::search(\"2fa\"?: bool, \"repo-name\"?: String) -> Dynamic"
        );
    }

    #[test]
    fn a_renamed_tool_says_what_the_roster_calls_it() {
        let catalog = Catalog::new([ToolEntry::mcp(
            "dev-radius",
            "dev_radius__resolve-library-id",
            "resolve-library-id",
            "Resolve a package to an id.",
            json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
        )]);
        let rendered = describe(&catalog, &RenderOptions::default(), &BTreeSet::new());
        assert!(
            rendered.contains(
                "mcp::dev_radius::resolve_library_id(name?: String) -> Dynamic\n    \
                 Resolve a package to an id. (tool name \"resolve-library-id\")"
            ),
            "{rendered}"
        );
    }

    /// A tool the model's own roster does not offer says so in the catalog, so
    /// the split is stated rather than inferred from function schemas.
    #[test]
    fn script_only_tools_say_so() {
        let catalog = Catalog::new([
            ToolEntry::roster("read_file", "Read a file.", json!({})),
            ToolEntry::mcp(
                "dev-radius",
                "dev_radius__search",
                "search",
                "An MCP search.",
                json!({}),
            ),
        ]);
        let script_only = BTreeSet::from(["dev_radius__search".to_string()]);
        let rendered = describe(&catalog, &RenderOptions::default(), &script_only);

        assert!(
            rendered
                .contains("mcp::dev_radius::search() -> Dynamic\n    An MCP search. (script only)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("tools::read_file() -> Dynamic\n    Read a file.\n"),
            "a directly callable tool is not marked: {rendered}"
        );
        assert!(rendered.contains("not on your own roster"), "{rendered}");

        // Nothing hidden, nothing marked.
        let plain = describe(&catalog, &RenderOptions::default(), &BTreeSet::new());
        assert!(!plain.contains("script only"), "{plain}");
    }

    /// Over budget, the script-only names are still said to be script-only —
    /// a name list has no room for a per-name mark, so they get their own.
    #[test]
    fn deferred_script_only_tools_are_listed_apart() {
        let catalog = Catalog::new([
            ToolEntry::roster(
                "read_file",
                "Read a file. A description long enough to blow a tiny budget.",
                json!({}),
            ),
            ToolEntry::mcp(
                "dev-radius",
                "dev_radius__search",
                "search",
                "An MCP search, also past the budget.",
                json!({}),
            ),
        ]);
        let script_only = BTreeSet::from(["dev_radius__search".to_string()]);
        let rendered = describe(
            &catalog,
            &RenderOptions {
                inline_budget: 10,
                brief_chars: 120,
            },
            &script_only,
        );

        assert!(
            rendered.contains("Script only (1): mcp::dev_radius::search"),
            "{rendered}"
        );
        assert!(rendered.contains("tools::read_file\n"), "{rendered}");
    }

    /// The description is the tool's contract with the model, so its exact
    /// shape is pinned: a change to it should be a decision, not an accident.
    #[test]
    fn the_description_is_pinned() {
        let catalog = Catalog::new([
            ToolEntry::roster(
                "read_file",
                "Read a file from disk. Supports an offset and a limit.",
                json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" }, "offset": { "type": "integer" } },
                    "required": ["path"],
                }),
            ),
            ToolEntry::roster("grep", "Search file contents.", json!({})),
        ]);

        let rendered = describe(&catalog, &RenderOptions::default(), &BTreeSet::new());
        let expected = format!(
            "{}\n\n{}\n\n# Tools (2)\n\n\
             tools::grep() -> Dynamic\n    Search file contents.\n\
             tools::read_file(offset?: int, path: String) -> Dynamic\n    \
             Read a file from disk. Supports an offset and a limit.\n",
            doc::INTRO,
            doc::CARD
        );
        assert_eq!(rendered, expected);
    }

    #[test]
    fn over_budget_tools_stay_reachable_by_name() {
        let mut entries: Vec<ToolEntry> = Vec::new();
        for index in 0..200 {
            entries.push(ToolEntry::roster(
                format!("tool_{index}"),
                "A description long enough to be worth a budget.",
                json!({ "type": "object", "properties": { "value": { "type": "string" } } }),
            ));
        }
        let catalog = Catalog::new(entries);
        let rendered = describe(&catalog, &RenderOptions::default(), &BTreeSet::new());

        // The names of everything deferred are still there, and the total stays
        // near the budget rather than carrying 200 signatures.
        assert!(rendered.contains("# Also callable"));
        assert!(rendered.contains("tools::tool_199"));
        // The budget is for the *declarations*; the prose and the listing of
        // names are the fixed costs on top, so they are counted rather than
        // allowed for. A regression that inlined every signature could not hide
        // inside them.
        let prose = estimated_tokens(doc::INTRO) + estimated_tokens(doc::CARD);
        assert!(
            estimated_tokens(&rendered) < 3000 + prose + 300,
            "the description grew past its budget: {} tokens",
            estimated_tokens(&rendered)
        );
    }

    /// The prose is what every turn pays for whatever the roster is, so its cost
    /// is a decision rather than a discovery: growing the card has to raise a
    /// number here on purpose.
    #[test]
    fn the_always_on_prose_has_a_budget_of_its_own() {
        let tokens = estimated_tokens(doc::INTRO) + estimated_tokens(doc::CARD);
        assert!(
            tokens < 1100,
            "the always-on prose is {tokens} tokens; trim the card or raise this deliberately"
        );
    }

    #[test]
    fn a_roster_too_big_to_even_name_is_summarized_by_namespace() {
        let mut entries: Vec<ToolEntry> = Vec::new();
        for server in 0..40 {
            for index in 0..40 {
                entries.push(ToolEntry::mcp(
                    format!("server_{server}"),
                    format!("server_{server}__tool_{index}"),
                    format!("tool_{index}"),
                    "An MCP tool.",
                    json!({}),
                ));
            }
        }
        let catalog = Catalog::new(entries);
        let rendered = describe(
            &catalog,
            &RenderOptions {
                inline_budget: 100,
                brief_chars: 120,
            },
            &BTreeSet::new(),
        );
        // The last server has nothing inlined, so all of its tools are behind
        // the count.
        assert!(rendered.contains("mcp::server_39 (40)"), "{rendered}");
        assert!(!rendered.contains("mcp::server_39::tool_0,"));
        assert!(rendered.contains("search_tools(query)"));
    }

    #[test]
    fn an_empty_catalog_still_teaches_the_language() {
        let rendered = describe(
            &Catalog::default(),
            &RenderOptions::default(),
            &BTreeSet::new(),
        );
        assert!(rendered.contains("# Tools (0)"));
        assert!(rendered.contains("describe_tool"));
        assert!(rendered.contains("Rhai"));
    }

    #[test]
    fn briefs_are_the_first_line_only() {
        assert_eq!(brief("First line.\nSecond line.", 80), "First line.");
        assert_eq!(brief("\n\n  Padded.  ", 80), "Padded.");
        assert_eq!(brief("", 80), "");
        assert_eq!(brief("A long first line indeed", 10), "A long fi…");
    }
}
