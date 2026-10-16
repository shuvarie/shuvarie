//! Finding a tool the description did not have room for.
//!
//! The catalog inlines declarations up to a budget; past it, a tool is still
//! callable and listed by name. These are the three globals that get the model
//! from a name it half-remembers -- or from a task in words -- to a call it can
//! write: `search_tools` ranks the roster against a query, `describe_tool`
//! spells one tool out including its parameter descriptions, and
//! `describe_namespace` browses a whole server's worth at once. `ALL_TOOLS` is
//! the flat list, for when the model would rather grep in the script than ask.
//!
//! Everything here is pure and takes the [`Catalog`], so the ranking and the
//! rendering are unit-testable without an engine.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::render::{brief, signature, type_of};
use crate::spec::{Catalog, ToolSpec};

/// How many hits `search_tools` returns before it stops.
///
/// A broad query on a big MCP roster can match everything, and a script that
/// prints the hits would spend its result budget on them. The cap is the
/// search's, not the model's, so the model is told what it did not see.
pub const DEFAULT_SEARCH_LIMIT: usize = 10;

/// The most a query can raise its own limit to. Past this the answer is
/// `describe_namespace`, or a narrower query.
pub const MAX_SEARCH_LIMIT: usize = 100;

/// How much of a tool's description `describe_tool` shows.
const FULL_DESCRIPTION_CHARS: usize = 2000;

/// How many declarations `describe_namespace` prints.
pub const DEFAULT_NAMESPACE_LIMIT: usize = 25;

/// One hit: enough to recognise a tool from, but not its signature -- that is
/// `describe_tool`'s job, and keeping the two apart keeps a wide search cheap.
#[derive(Debug, PartialEq, Eq)]
pub struct Hit {
    /// The name a script calls, `tools::read_file`.
    pub name: String,
    /// The tool's first line, trimmed to a readable length.
    pub description: String,
}

/// Rank the catalog against a query.
///
/// The order is what a reader means by "relevance": an exact name beats a
/// prefix, a prefix beats a mention in the description, and the name is
/// searched before the prose. Ties break by name so the answer is stable, and
/// the caller learns how many matches it did not get.
///
/// An empty query is not a mistake -- it lists the roster, which is how a
/// model with no idea what is available starts.
pub fn search(
    catalog: &Catalog,
    query: &str,
    namespace: Option<&str>,
    limit: usize,
) -> (Vec<Hit>, usize) {
    let query = query.trim().to_lowercase();
    let wanted = namespace.map(namespace_segments);
    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);

    let mut scored: Vec<(u8, &ToolSpec)> = catalog
        .tools()
        .iter()
        .filter(|spec| match &wanted {
            Some(wanted) => starts_with(&spec.namespace, wanted),
            None => true,
        })
        .filter_map(|spec| score(spec, &query).map(|rank| (rank, spec)))
        .collect();

    // Best rank first, then by callable name: `sort_by` is stable, so the
    // catalog's own name order survives inside a rank.
    scored.sort_by_key(|(rank, spec)| (*rank, spec.qualified()));

    let matched = scored.len();
    let hits = scored
        .into_iter()
        .take(limit)
        .map(|(_, spec)| Hit {
            name: spec.qualified(),
            description: brief(&spec.description, 200),
        })
        .collect();
    (hits, matched)
}

/// A tool's rank against a lowercased query, or `None` when it does not match
/// at all. Lower is better.
fn score(spec: &ToolSpec, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(5);
    }
    let qualified = spec.qualified().to_lowercase();
    let callable = spec.callable.to_lowercase();
    let name = spec.name.to_lowercase();
    let description = spec.description.to_lowercase();

    Some(match () {
        _ if qualified == *query || callable == *query || name == *query => 0,
        _ if callable.starts_with(query) => 1,
        _ if qualified.starts_with(query) => 2,
        _ if callable.contains(query) => 3,
        // The tool's own name, before mangling: a model that read the raw MCP
        // roster somewhere still finds it.
        _ if name.contains(query) => 4,
        // Several words: every one of them has to appear somewhere, so a
        // phrase like "search docs" does not match on "search" alone.
        _ if words_match(query, &qualified, &description) => 5,
        _ => return None,
    })
}

/// Whether every word of the query appears in the name or the description.
fn words_match(query: &str, qualified: &str, description: &str) -> bool {
    query
        .split_whitespace()
        .all(|word| qualified.contains(word) || description.contains(word))
}

/// One tool, spelled out: its signature, what it does, and what each parameter
/// means -- the last of which is the part a signature cannot carry and the part
/// that decides whether the call is written correctly.
///
/// The name may be qualified (`tools::read_file`) or bare (`read_file`), because
/// a model that has just read a description has the bare name and should not
/// have to guess the namespace. An unknown name is an error naming the
/// near-misses: a script that stops here can be fixed in one step, whereas a
/// silent `()` costs the model a whole round trip to discover.
pub fn describe_tool(catalog: &Catalog, name: &str) -> Result<String, String> {
    let spec = resolve(catalog, name)?;
    let mut out = signature(spec);
    if spec.renamed() {
        out.push_str(&format!(" (tool name \"{}\")", spec.name));
    }
    out.push('\n');
    out.push_str(&crate::convert::truncate(
        spec.description.trim(),
        FULL_DESCRIPTION_CHARS,
    ));

    let parameters = parameters(spec);
    for (name, required, description) in parameters {
        let marker = if required { "" } else { "?" };
        out.push_str(&format!("\n\n{name}{marker}: {description}"));
    }
    Ok(out)
}

/// A tool's parameters as `(name, required, description)`.
fn parameters(spec: &ToolSpec) -> Vec<(String, bool, String)> {
    let required: BTreeSet<&str> = spec
        .schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut parameters: Vec<(String, bool, String)> = spec
        .schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| {
            properties
                .iter()
                .map(|(name, schema)| {
                    let description = schema
                        .get("description")
                        .and_then(Value::as_str)
                        .map(|text| brief(text, 300))
                        // No prose, so the type is the next best thing: enough
                        // to know what to put in the field, and written the way
                        // the signature writes it.
                        .unwrap_or_else(|| type_of(schema));
                    (name.clone(), required.contains(name.as_str()), description)
                })
                .collect()
        })
        .unwrap_or_default();
    // Same order as the signature prints, so the two read as one document.
    parameters.sort_by(|a, b| a.0.cmp(&b.0));
    parameters
}

/// Every declaration in one namespace, so a server's roster can be browsed
/// without searching for each tool.
pub fn describe_namespace(
    catalog: &Catalog,
    namespace: &str,
    limit: usize,
) -> Result<String, String> {
    let wanted = namespace_segments(namespace);
    let specs: Vec<&ToolSpec> = catalog
        .tools()
        .iter()
        .filter(|spec| starts_with(&spec.namespace, &wanted))
        .collect();

    if specs.is_empty() {
        return Err(format!(
            "no namespace `{}`; the namespaces are {}",
            namespace.trim(),
            namespaces(catalog).join(", ")
        ));
    }

    let limit = limit.clamp(1, MAX_SEARCH_LIMIT);
    let mut out = format!(
        "{} ({} {})",
        namespace.trim(),
        specs.len(),
        if specs.len() == 1 { "tool" } else { "tools" }
    );
    for spec in specs.iter().take(limit) {
        out.push_str(&format!(
            "\n\n{}\n    {}",
            signature(spec),
            brief(&spec.description, 200)
        ));
    }
    if specs.len() > limit {
        out.push_str(&format!(
            "\n\n… and {} more; `search_tools(query, #{{ namespace: \"{}\" }})` narrows it.",
            specs.len() - limit,
            namespace.trim()
        ));
    }
    Ok(out)
}

/// The name a script should use, resolving a bare name when it is unambiguous.
fn resolve<'a>(catalog: &'a Catalog, name: &str) -> Result<&'a ToolSpec, String> {
    let name = name.trim();
    if let Some(spec) = catalog.tools().iter().find(|spec| spec.qualified() == name) {
        return Ok(spec);
    }

    let mut bare: Vec<&ToolSpec> = catalog
        .tools()
        .iter()
        .filter(|spec| spec.callable == name || spec.name == name)
        .collect();
    match bare.len() {
        1 => return Ok(bare.remove(0)),
        0 => {}
        _ => {
            return Err(format!(
                "`{name}` is ambiguous; it could be {}",
                bare.iter()
                    .map(|spec| format!("`{}`", spec.qualified()))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ));
        }
    }

    // Nothing matched: the near-misses are the fix, so they go in the message.
    let near = similar(catalog, &name.to_lowercase());
    Err(if near.is_empty() {
        format!("no tool called `{name}`; `ALL_TOOLS` lists the roster")
    } else {
        format!("no tool called `{name}`; did you mean {}?", near.join(", "))
    })
}

/// Up to three callable names close to what was asked for.
fn similar(catalog: &Catalog, query: &str) -> Vec<String> {
    let tokens: Vec<&str> = query.split('_').filter(|part| !part.is_empty()).collect();
    let mut ranked: Vec<(usize, &ToolSpec)> = catalog
        .tools()
        .iter()
        // A substring either way covers both "read" for `read_file` and
        // `read_file` for "read".
        .filter(|spec| {
            let qualified = spec.qualified().to_lowercase();
            let callable = spec.callable.to_lowercase();
            tokens
                .iter()
                .any(|token| qualified.contains(token) || token.contains(callable.as_str()))
        })
        .map(|spec| {
            let callable = spec.callable.to_lowercase();
            let distance = tokens
                .iter()
                .map(|token| edit_distance(token, &callable))
                .min()
                .unwrap_or(usize::MAX);
            (distance, spec)
        })
        .collect();
    ranked.sort_by_key(|(distance, spec)| (*distance, spec.qualified()));
    ranked
        .into_iter()
        .take(3)
        .map(|(_, spec)| format!("`{}`", spec.qualified()))
        .collect()
}

/// Levenshtein distance, capped by the strings' lengths. Small inputs only --
/// this runs over a typo against a tool name.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// The namespace paths in the catalog, as callable strings.
pub fn namespaces(catalog: &Catalog) -> Vec<String> {
    catalog
        .namespaces()
        .into_iter()
        .map(|namespace| namespace.join("::"))
        .collect()
}

/// Every callable name, for `ALL_TOOLS`.
pub fn all_tools(catalog: &Catalog) -> Vec<String> {
    catalog.tools().iter().map(ToolSpec::qualified).collect()
}

/// A namespace string as path segments.
fn namespace_segments(namespace: &str) -> Vec<String> {
    namespace
        .trim()
        .split("::")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect()
}

/// Whether `namespace` is `wanted` or a descendant of it, so filtering by
/// `mcp` catches `mcp::dev_radius` too.
fn starts_with(namespace: &[String], wanted: &[String]) -> bool {
    namespace.len() >= wanted.len() && namespace.iter().zip(wanted).all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::ToolEntry;
    use serde_json::json;

    fn catalog() -> Catalog {
        Catalog::new([
            ToolEntry::roster(
                "read_file",
                "Read a file.\n\nIt is long.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "The file to read." },
                        "offset": { "type": "integer" }
                    },
                    "required": ["path"]
                }),
            ),
            ToolEntry::roster("grep", "Search file contents.", json!({})),
            ToolEntry::roster("glob", "List paths by pattern.", json!({})),
            ToolEntry::mcp(
                "dev_radius",
                "dev_radius__search",
                "search",
                "Search the docs.",
                json!({}),
            ),
        ])
    }

    #[test]
    fn an_exact_name_outranks_a_mention_in_prose() {
        let (hits, matched) = search(&catalog(), "dev_radius", None, 10);
        assert_eq!(matched, 1);
        assert_eq!(hits[0].name, "mcp::dev_radius::search");
    }

    #[test]
    fn a_bare_name_finds_the_namespaced_tool() {
        let (hits, _) = search(&catalog(), "read_file", None, 10);
        assert_eq!(hits[0].name, "tools::read_file");
        assert_eq!(hits[0].description, "Read a file.");
    }

    #[test]
    fn search_reports_what_it_did_not_return() {
        // Every tool matches an empty query, and the limit is honoured.
        let (hits, matched) = search(&catalog(), "", None, 2);
        assert_eq!(hits.len(), 2);
        assert_eq!(matched, 4);
    }

    #[test]
    fn a_namespace_filter_narrows_and_its_children_still_match() {
        let (hits, _) = search(&catalog(), "", Some("mcp"), 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "mcp::dev_radius::search");

        let (hits, _) = search(&catalog(), "", Some("tools"), 10);
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn a_multi_word_query_has_to_match_every_word() {
        let (hits, _) = search(&catalog(), "search contents", None, 10);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].name, "tools::grep");

        // "search" alone matches two, but no tool has both words.
        let (hits, _) = search(&catalog(), "search docs", None, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "mcp::dev_radius::search");
    }

    #[test]
    fn the_limit_is_clamped_so_a_script_cannot_ask_for_everything() {
        let (hits, _) = search(&catalog(), "", None, 0);
        assert_eq!(hits.len(), 1, "a zero limit still returns one");

        let (hits, _) = search(&catalog(), "", None, MAX_SEARCH_LIMIT + 1_000);
        assert_eq!(hits.len(), 4);
    }

    #[test]
    fn describe_tool_spells_out_the_parameters() {
        let text = describe_tool(&catalog(), "read_file").unwrap();
        assert_eq!(
            text,
            "tools::read_file(offset?: int, path: String) -> Dynamic\n\
             Read a file.\n\n\
             It is long.\n\n\
             offset?: int\n\n\
             path: The file to read."
        );
        // While the search result keeps only the first line, so a wide search
        // stays cheap.
        let (hits, _) = search(&catalog(), "read_file", None, 1);
        assert_eq!(hits[0].description, "Read a file.");
    }

    #[test]
    fn describe_tool_takes_a_qualified_name_too() {
        assert_eq!(
            describe_tool(&catalog(), "tools::grep").unwrap(),
            "tools::grep() -> Dynamic\nSearch file contents."
        );
    }

    #[test]
    fn describe_tool_explains_a_name_it_does_not_know() {
        let error = describe_tool(&catalog(), "read_fil").unwrap_err();
        assert!(error.contains("did you mean"), "{error}");
        assert!(error.contains("tools::read_file"), "{error}");

        let error = describe_tool(&catalog(), "nonexistent").unwrap_err();
        assert!(error.contains("ALL_TOOLS"), "{error}");
    }

    #[test]
    fn describe_tool_names_the_candidates_when_a_name_is_ambiguous() {
        // Two namespaces, one callable name: the bare name cannot resolve.
        let catalog = Catalog::new([
            ToolEntry::roster("search", "Roster search.", json!({})),
            ToolEntry::mcp("docs", "docs__search", "search", "Docs search.", json!({})),
        ]);
        let error = describe_tool(&catalog, "search").unwrap_err();
        assert!(error.contains("ambiguous"), "{error}");
        assert!(error.contains("tools::search"), "{error}");
        assert!(error.contains("mcp::docs::search"), "{error}");
    }

    #[test]
    fn describe_namespace_lists_its_tools_and_counts_the_rest() {
        let text = describe_namespace(&catalog(), "tools", 2).unwrap();
        assert!(text.starts_with("tools (3 tools)"), "{text}");
        assert!(text.contains("tools::grep()"), "{text}");
        assert!(text.contains("… and 1 more"), "{text}");
        assert!(text.contains("search_tools"), "{text}");

        // One tool is not "1 tools".
        let text = describe_namespace(&catalog(), "mcp", 10).unwrap();
        assert!(text.starts_with("mcp (1 tool)"), "{text}");
    }

    #[test]
    fn describe_namespace_explains_an_unknown_namespace() {
        let error = describe_namespace(&catalog(), "nope", 25).unwrap_err();
        assert!(error.contains("mcp"), "{error}");
        assert!(error.contains("tools"), "{error}");
    }

    #[test]
    fn all_tools_is_every_callable_name_in_order() {
        assert_eq!(
            all_tools(&catalog()),
            [
                "mcp::dev_radius::search",
                "tools::glob",
                "tools::grep",
                "tools::read_file"
            ]
        );
    }

    #[test]
    fn edit_distance_is_the_textbook_metric() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("same", "same"), 0);
    }
}
