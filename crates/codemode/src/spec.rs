//! The catalog: which tools a script may call, and under which name.
//!
//! The host knows a tool by its roster name (for an MCP tool, the composite
//! `server__tool` name it dispatches on). A script calls it by a Rhai
//! identifier inside a namespace, so the catalog is where the two meet: it
//! mangles names into callability, resolves collisions deterministically, and
//! prints the callable name so the model never has to derive it.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

/// The namespace the roster's own tools live in: `tools::read_file(...)`.
pub const TOOLS_NS: &str = "tools";
/// The namespace MCP servers nest under, one segment per server:
/// `mcp::dev_radius::search(...)`.
pub const MCP_NS: &str = "mcp";

/// Words Rhai's parser will not accept as a call, even namespace-qualified --
/// verified against the parser by
/// `catalog::tests::reserved_names_really_are_uncallable`, which also keeps the
/// list free of names that merely look like keywords (`type` and `each` are
/// callable, so mangling them would only cost readability).
const RESERVED: &[&str] = &[
    "as",
    "async",
    "await",
    "break",
    "call",
    "case",
    "catch",
    "const",
    "continue",
    "curry",
    "default",
    "do",
    "else",
    "eval",
    "export",
    "false",
    "fn",
    "for",
    "go",
    "if",
    "import",
    "in",
    "is",
    "let",
    "loop",
    "match",
    "module",
    "new",
    "nil",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "shared",
    "spawn",
    "static",
    "super",
    "switch",
    "sync",
    "this",
    "thread",
    "throw",
    "true",
    "try",
    "until",
    "use",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// The name a script calls a tool (or a namespace) by.
///
/// The rules, in order: every character outside Rhai's identifier set becomes
/// `_`; a name that begins with a digit, is a [`RESERVED`] word, or has no
/// letters at all gets a `_` prefix or a `tool` fallback. Every rule exists
/// because the alternative is a call that silently does something else -- a
/// `-` in a name parses as subtraction, and a keyword fails to parse at all.
pub fn callable_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();

    // A bare `_` (or `__`) is not an identifier, so a name with no letters or
    // digits needs a real one.
    if !out.chars().any(|ch| ch.is_ascii_alphanumeric()) {
        out.push_str("tool");
    }
    if out.starts_with(|ch: char| ch.is_ascii_digit()) {
        out.insert(0, '_');
    }
    if RESERVED.contains(&out.as_str()) {
        out.insert(0, '_');
    }
    out
}

/// A tool as the host knows it, before the catalog gives it a callable name.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolEntry {
    /// The roster name the host dispatches on.
    pub host_name: String,
    /// The tool's own name, as its namespace sees it.
    pub name: String,
    /// Namespace segments, outermost first.
    pub namespace: Vec<String>,
    pub description: String,
    /// The tool's JSON Schema arguments object.
    pub schema: Value,
}

impl ToolEntry {
    /// A tool of the roster: `tools::<name>`.
    pub fn roster(name: impl Into<String>, description: impl Into<String>, schema: Value) -> Self {
        let name = name.into();
        Self {
            host_name: name.clone(),
            name,
            namespace: vec![TOOLS_NS.to_string()],
            description: description.into(),
            schema,
        }
    }

    /// An MCP server's tool: `mcp::<server>::<name>`, dispatched by the roster's
    /// composite name.
    pub fn mcp(
        server: impl Into<String>,
        host_name: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        schema: Value,
    ) -> Self {
        Self {
            host_name: host_name.into(),
            name: name.into(),
            namespace: vec![MCP_NS.to_string(), server.into()],
            description: description.into(),
            schema,
        }
    }
}

/// A tool as a script sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolSpec {
    /// The roster name the host dispatches on.
    pub host_name: String,
    /// The callable namespace path, outermost first.
    pub namespace: Vec<String>,
    /// The callable name inside the namespace.
    pub callable: String,
    /// The tool's own name, kept to explain a name that had to change.
    pub name: String,
    pub description: String,
    pub schema: Value,
}

impl ToolSpec {
    /// The name a script writes, and the name the catalog prints.
    pub fn qualified(&self) -> String {
        format!("{}::{}", self.namespace.join("::"), self.callable)
    }

    /// Whether the callable name differs from the tool's own name.
    pub fn renamed(&self) -> bool {
        self.callable != self.name
    }
}

/// The tools one script may call.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    tools: Vec<ToolSpec>,
}

impl Catalog {
    /// Mangle every name into callability, resolving collisions inside a
    /// namespace with a numeric suffix so that a collision costs a tool its
    /// pretty name rather than its reachability. Sorted by callable name, so
    /// the rendered description is stable across turns.
    pub fn new(entries: impl IntoIterator<Item = ToolEntry>) -> Self {
        let mut tools = Vec::new();
        let mut taken: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();

        for entry in entries {
            let namespace: Vec<String> = entry
                .namespace
                .iter()
                .map(|segment| callable_name(segment))
                .collect();
            let base = callable_name(&entry.name);
            let used = taken.entry(namespace.clone()).or_default();
            let mut callable = base.clone();
            let mut suffix = 2;
            while !used.insert(callable.clone()) {
                callable = format!("{base}_{suffix}");
                suffix += 1;
            }
            tools.push(ToolSpec {
                host_name: entry.host_name,
                namespace,
                callable,
                name: entry.name,
                description: entry.description,
                schema: entry.schema,
            });
        }

        tools.sort_by_key(ToolSpec::qualified);
        Self { tools }
    }

    pub fn tools(&self) -> &[ToolSpec] {
        &self.tools
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// The tool a script's `namespace::callable` refers to.
    pub fn find(&self, namespace: &[&str], callable: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|spec| {
            spec.callable == callable
                && spec.namespace.len() == namespace.len()
                && spec
                    .namespace
                    .iter()
                    .zip(namespace)
                    .all(|(segment, wanted)| segment == wanted)
        })
    }

    /// The tools of one namespace, in call order.
    pub fn in_namespace<'a>(
        &'a self,
        namespace: &'a [String],
    ) -> impl Iterator<Item = &'a ToolSpec> {
        self.tools
            .iter()
            .filter(move |spec| spec.namespace == namespace)
    }

    /// The distinct namespace paths, outermost first then alphabetically.
    pub fn namespaces(&self) -> Vec<Vec<String>> {
        let mut namespaces: Vec<Vec<String>> = self
            .tools
            .iter()
            .map(|spec| spec.namespace.clone())
            .collect();
        namespaces.dedup();
        namespaces
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_outside_rhai_identifiers_are_rewritten() {
        // MCP tools are the realistic source of these.
        assert_eq!(callable_name("resolve-library-id"), "resolve_library_id");
        assert_eq!(callable_name("search.docs"), "search_docs");
        assert_eq!(callable_name("read file"), "read_file");
        assert_eq!(callable_name("dev-radius").as_str(), "dev_radius");
        // Left alone when already callable.
        for name in ["read_file", "grep", "web_search", "list_dir", "_private"] {
            assert_eq!(callable_name(name), name);
        }
    }

    #[test]
    fn reserved_words_and_odd_names_get_a_usable_prefix() {
        assert_eq!(callable_name("call"), "_call");
        assert_eq!(callable_name("switch"), "_switch");
        assert_eq!(callable_name("in"), "_in");
        // A leading digit is not an identifier.
        assert_eq!(callable_name("2fa"), "_2fa");
        // A name with no letters or digits cannot be an identifier at all.
        assert_eq!(callable_name("_"), "_tool");
        assert_eq!(callable_name(""), "tool");
        assert_eq!(callable_name("---"), "___tool");
        // A word that merely looks like a keyword is callable, so it is left
        // alone: mangling it would only cost readability.
        assert_eq!(callable_name("type"), "type");
        assert_eq!(callable_name("each"), "each");
    }

    #[test]
    fn a_collision_costs_a_name_not_a_tool() {
        let catalog = Catalog::new([
            ToolEntry::mcp(
                "dev-radius",
                "dev_radius__a",
                "read-file",
                "first",
                json!({}),
            ),
            // A server name that mangles into the first one's, and a tool name
            // that mangles into the other's: both collisions are inside one
            // namespace, and neither may cost a tool its reachability.
            ToolEntry::mcp(
                "dev_radius",
                "dev_radius__b",
                "read_file",
                "second",
                json!({}),
            ),
        ]);

        let callables: Vec<String> = catalog.tools().iter().map(ToolSpec::qualified).collect();
        assert_eq!(
            callables,
            ["mcp::dev_radius::read_file", "mcp::dev_radius::read_file_2",],
            "a colliding name gets a suffix rather than overwriting a tool"
        );

        // Each is still reachable by the name the catalog printed, and the
        // host name behind it is the right one.
        assert_eq!(
            catalog
                .find(&["mcp", "dev_radius"], "read_file")
                .unwrap()
                .host_name,
            "dev_radius__a"
        );
        assert_eq!(
            catalog
                .find(&["mcp", "dev_radius"], "read_file_2")
                .unwrap()
                .host_name,
            "dev_radius__b"
        );
        // The second server mangled into the first one's namespace rather than
        // into one no script can name.
        assert_eq!(catalog.namespaces().len(), 1);
    }

    #[test]
    fn roster_tools_live_in_the_tools_namespace() {
        let catalog = Catalog::new([
            ToolEntry::roster("read_file", "Read a file.", json!({})),
            ToolEntry::mcp(
                "dev_radius",
                "dev_radius__search",
                "search",
                "Search.",
                json!({}),
            ),
        ]);

        assert_eq!(catalog.len(), 2);
        assert_eq!(catalog.tools()[1].qualified(), "tools::read_file");
        assert!(!catalog.tools()[1].renamed());
        assert_eq!(catalog.tools()[1].host_name, "read_file");
        assert_eq!(catalog.tools()[0].qualified(), "mcp::dev_radius::search");
        assert_eq!(catalog.namespaces().len(), 2);
    }

    /// The only names the mangling may rewrite are the ones the parser actually
    /// rejects, in both directions: every [`RESERVED`] word must be uncallable,
    /// and a word that is callable must not be listed.
    #[test]
    fn reserved_names_really_are_uncallable() {
        use rhai::{EvalAltResult, Module};

        for name in RESERVED {
            let mut module = Module::new();
            module.set_native_fn(*name, || Ok::<_, Box<EvalAltResult>>(()));
            let mut engine = rhai::Engine::new();
            engine.register_static_module("t", module.into());

            assert!(
                engine.compile(format!("t::{name}()")).is_err(),
                "'{name}' is listed as reserved but is callable -- remove it, so the \
                 rule stays the exact set rather than a guess"
            );
        }

        // The mangled form of every reserved word must itself be callable.
        for name in RESERVED {
            let mangled = callable_name(name);
            let mut module = Module::new();
            module.set_native_fn(mangled.clone(), || Ok::<_, Box<EvalAltResult>>(()));
            let mut engine = rhai::Engine::new();
            engine.register_static_module("t", module.into());
            assert!(
                engine.compile(format!("t::{mangled}()")).is_ok(),
                "'{name}' mangled into '{mangled}', which still does not parse"
            );
        }
    }
}
