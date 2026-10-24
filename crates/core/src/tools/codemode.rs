//! `codemode`: one tool that runs a Rhai script, and the script calls the
//! other tools.
//!
//! The reason to do this at all is context: a script's intermediate results
//! never enter the model's window, so several dependent calls cost one round
//! trip and one summary. See `docs/designs/codemode.md`.
//!
//! The interesting part of the wiring is that a nested call is not a
//! re-implementation of a tool call -- it is the *same* [`DynamicTool`] the
//! model would have called directly, with the same [`Access`] already inside
//! it. So permissions, scene asks, file locking, LSP notifications and
//! `read_file`'s per-turn cache behave identically whether the model calls a
//! tool itself or a script calls it.
//!
//! What a nested call does *not* do is appear: it produces no stream item, so a
//! script's calls never become transcript blocks, sidebar rows or session tool
//! records of their own. The turn's one `codemode` block reports the whole run
//! (`Tool calls (N)` with each call's name, arguments and outcome), which is
//! what the transcript, a replayed session and the model all read. One block
//! per script instead of one per call -- and the live turn and the reloaded one
//! agree, because neither has ever seen the nested calls.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::{Value, json};
use shuvarie_codemode::script::InvokeFuture;
use shuvarie_codemode::{
    Catalog, Limits, RenderOptions, ScriptRequest, Stop, Store, ToolCall, ToolEntry, ToolInvoker,
    block, describe, run,
};
use shuvarie_llm::{DynamicTool, Tool, ToolContext, ToolExecutionError, ToolOutput, into_dynamic};

use crate::permissions::Access;
use crate::scenes::ToolScene;

/// The tool's name in the roster.
pub const NAME: &str = "codemode";

/// How codemode presents the roster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Tools stay in the model's roster and are described as scriptable, so the
    /// model chooses between a script and a direct call.
    #[default]
    Declared,
    /// The built-in tools stay callable directly; stdio and MCP tools are
    /// reachable only through a script. The builtins are few and stable, while a
    /// connected MCP server (or a handful of configured subprocess tools) is
    /// where the roster bloats and where a typed API in a script reads better
    /// than a schema per tool.
    DeclareBuiltin,
    /// Tools are hidden from the model and reachable only through a script.
    Only,
}

/// Everything the roster needs to offer the tool.
pub struct Wiring {
    pub mode: Mode,
    pub limits: Limits,
    pub render: RenderOptions,
    /// The tools scripts may not orchestrate (`codemode { tools { .. } }`). The
    /// model may still call them directly.
    pub tool_scene: ToolScene,
    /// Where `store`/`load` keep state.
    pub store: Arc<Store>,
}

impl Wiring {
    /// The default wiring, for a store.
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            mode: Mode::default(),
            limits: Limits::default(),
            render: RenderOptions::default(),
            tool_scene: ToolScene::build(None),
            store,
        }
    }
}

/// Take the roster the turn built and return the roster the model sees, with
/// the tool added.
///
/// `mcp` is the per-server roster those tools were built from, used to give an
/// MCP tool its namespace (`mcp::dev_radius::search`) rather than the composite
/// name it dispatches by, and `stdio` is the configured `tool` entries — the two
/// sources [`Mode::DeclareBuiltin`] hides behind scripts.
pub fn install(
    tools: Vec<DynamicTool>,
    wiring: &Wiring,
    mcp: &BTreeMap<String, shuvarie_mcp::McpToolMap>,
    stdio: &BTreeMap<String, shuvarie_config::StdioToolConfig>,
    max_output_chars: usize,
    access: &Access,
) -> Vec<DynamicTool> {
    let scriptable: Vec<DynamicTool> = tools
        .iter()
        .filter(|tool| {
            let name = tool.definition().name.to_string();
            // A script never orchestrates codemode, so the tool is never part
            // of its own roster.
            name != NAME && wiring.tool_scene.allows(&name)
        })
        .cloned()
        .collect();

    let catalog = catalog_for(&scriptable, &mcp_servers(mcp));
    let roster: Arc<BTreeMap<String, DynamicTool>> = Arc::new(
        scriptable
            .iter()
            .map(|tool| (tool.definition().name.to_string(), tool.clone()))
            .collect(),
    );

    // Only tools a script can actually run may leave the model's roster. A tool
    // a script may not orchestrate has to stay visible, or it would be reachable
    // by neither route -- gone rather than hidden.
    let hide = |target: BTreeSet<String>| -> BTreeSet<String> {
        target
            .into_iter()
            .filter(|name| roster.contains_key(name))
            .collect()
    };

    // What the model's own tool list will not offer: nothing in `declared`, the
    // configured sources in `declare-builtin`, the whole scriptable roster in
    // `only`. The catalog marks these, so a script-only tool is stated rather
    // than left to be inferred from the model's function schemas.
    let hidden: BTreeSet<String> = match wiring.mode {
        Mode::Declared => BTreeSet::new(),
        Mode::DeclareBuiltin => hide(script_only(stdio, mcp)),
        Mode::Only => hide(roster.keys().cloned().collect()),
    };

    let codemode = into_dynamic(
        NAME,
        CodeMode {
            description: describe(&catalog, &wiring.render, &hidden),
            catalog: Arc::new(catalog),
            roster: Arc::clone(&roster),
            // The tool's own access, so its scene overrides work like any other
            // gated tool's.
            access: access.for_tool(NAME, &wiring.tool_scene),
            store: Arc::clone(&wiring.store),
            limits: wiring.limits.clone(),
            max_output_chars,
        },
    );

    let mut kept: Vec<DynamicTool> = match wiring.mode {
        // Nothing is hidden, so every tool is callable both ways.
        Mode::Declared => tools,
        Mode::DeclareBuiltin | Mode::Only => tools
            .into_iter()
            .filter(|tool| !hidden.contains(&tool.definition().name.to_string()))
            .collect(),
    };
    kept.push(codemode);
    kept
}

/// The roster names that come from config rather than code: the configured
/// `tool` subprocess entries plus every connected MCP server's tools (keyed in
/// the roster by their composite name).
fn script_only(
    stdio: &BTreeMap<String, shuvarie_config::StdioToolConfig>,
    mcp: &BTreeMap<String, shuvarie_mcp::McpToolMap>,
) -> BTreeSet<String> {
    stdio
        .keys()
        .cloned()
        .chain(mcp.values().flat_map(|roster| roster.keys().cloned()))
        .collect()
}

/// The tool's callable roster as a catalog, with MCP tools in their server's
/// namespace.
fn catalog_for(tools: &[DynamicTool], servers: &BTreeMap<String, (String, String)>) -> Catalog {
    Catalog::new(tools.iter().map(|tool| {
        let definition = tool.definition();
        let name = definition.name.to_string();
        match servers.get(&name) {
            Some((server, tool_name)) => ToolEntry::mcp(
                server.clone(),
                name,
                tool_name.clone(),
                definition.description.clone(),
                definition.parameters.clone(),
            ),
            None => ToolEntry::roster(
                name,
                definition.description.clone(),
                definition.parameters.clone(),
            ),
        }
    }))
}

/// Composite roster name → (server, the server's own name for the tool).
fn mcp_servers(
    mcp: &BTreeMap<String, shuvarie_mcp::McpToolMap>,
) -> BTreeMap<String, (String, String)> {
    mcp.iter()
        .flat_map(|(server, roster)| {
            roster.iter().map(move |(composite, info)| {
                (composite.clone(), (server.clone(), info.name.clone()))
            })
        })
        .collect()
}

/// The tool.
struct CodeMode {
    description: String,
    catalog: Arc<Catalog>,
    roster: Arc<BTreeMap<String, DynamicTool>>,
    access: Access,
    store: Arc<Store>,
    limits: Limits,
    max_output_chars: usize,
}

impl Tool for CodeMode {
    const NAME: &'static str = "codemode";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "The Rhai script to run. Its last expression is the result; \
                                    use `text(v)` to report more than one value.",
                },
            },
            "required": ["script"],
        })
    }

    async fn call(
        &self,
        _context: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let script = args
            .get("script")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolExecutionError::invalid_args("missing string argument 'script'"))?;

        // The call authorizes like any gated tool, and each nested call
        // authorizes inside its own tool, so a script cannot reach anything the
        // model could not have.
        let detail = shuvarie_codemode::convert::truncate(
            script
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default(),
            120,
        );
        self.access
            .authorize_tool(NAME, &detail)
            .await
            .map_err(ToolExecutionError::permission_denied)?;

        let invoker = Arc::new(RosterInvoker {
            roster: Arc::clone(&self.roster),
        });

        let outcome = run(
            ScriptRequest {
                script: script.to_string(),
                catalog: Arc::clone(&self.catalog),
                store: Some(Arc::clone(&self.store)),
                limits: self.limits.clone(),
            },
            invoker,
            Stop::new(),
        )
        .await;

        Ok(ToolOutput::text(block(&outcome, self.max_output_chars)))
    }
}

/// The host side of a nested call.
///
/// It runs the call and nothing else: a nested call is deliberately silent, so
/// a script's calls never become transcript blocks of their own. What the
/// script did is reported once, in the script's own result (`Tool calls (N)`),
/// which is where the model and the transcript both read it — so the turn has
/// one block per script instead of one per call, and a replayed session shows
/// the same thing as the live one.
struct RosterInvoker {
    roster: Arc<BTreeMap<String, DynamicTool>>,
}

impl ToolInvoker for RosterInvoker {
    fn invoke(&self, call: ToolCall) -> InvokeFuture<'_> {
        Box::pin(async move {
            let Some(tool) = self.roster.get(&call.host_name) else {
                return Err(format!("no such tool: {}", call.host_name));
            };

            // A fresh context per call: a tool writes its host-only metadata
            // ([`FileChange`], [`ShellStreams`]) into it. A script's calls do
            // not forward that metadata -- they have no block to draw it in --
            // so the context exists only to satisfy the tool contract.
            let mut context = ToolContext::new();
            match tool.run_with(&mut context, call.args).await {
                Ok(output) => Ok(output_as_json(&output)),
                Err(error) => Err(error.message().to_string()),
            }
        })
    }
}

/// A tool's output as a script sees it: its text, or its structure when it
/// returned structure.
fn output_as_json(output: &ToolOutput) -> Value {
    if let Some(json) = output.as_json() {
        return json.clone();
    }
    // Several text blocks become one string, the way the stream's tool result
    // joins them.
    let text = output
        .as_content()
        .iter()
        .filter_map(|content| content.as_text())
        .collect::<Vec<_>>()
        .join("\n");
    Value::String(if text.is_empty() {
        output.render()
    } else {
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, description: &str) -> DynamicTool {
        let name = shuvarie_llm::tool_name(name);
        DynamicTool::new_with_context(
            name.clone(),
            description,
            json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            move |_ctx, _args| {
                let name = name.to_string();
                Box::pin(async move { Ok(ToolOutput::text(format!("{name} ran"))) })
            },
        )
    }

    fn access() -> Access {
        crate::test_util::access()
    }

    fn names(tools: &[DynamicTool]) -> Vec<String> {
        tools
            .iter()
            .map(|tool| tool.definition().name.to_string())
            .collect()
    }

    fn only(disabled: &str) -> Wiring {
        let mut config = shuvarie_config::SceneToolsConfig::default();
        config.tools.insert(
            disabled.to_string(),
            shuvarie_config::ToolOverride {
                disabled: Some(true),
                ask: None,
            },
        );
        Wiring {
            mode: Mode::Only,
            tool_scene: ToolScene::build(Some(&config)),
            ..Wiring::new(Arc::new(Store::new()))
        }
    }

    /// A wiring in `mode`, with no per-tool script gating.
    fn mode_wiring(mode: Mode) -> Wiring {
        Wiring {
            mode,
            ..Wiring::new(Arc::new(Store::new()))
        }
    }

    /// Permissive permissions, for exercising the tool end to end: the builtin
    /// default asks, and a test's gate denies rather than blocking.
    fn allow_all() -> Access {
        crate::test_util::access_for_config(&shuvarie_config::PermissionsConfig {
            default: Some(shuvarie_config::Verb::Allow),
            ..Default::default()
        })
    }

    /// A nested call is silent, so the script's own result is the only place its
    /// calls are reported. This pins that the trace survives the trip through
    /// the invoker: the model and the transcript both read the calls here or
    /// nowhere.
    #[tokio::test]
    async fn a_scripts_nested_calls_are_reported_in_its_own_result() {
        let tools = install(
            vec![tool("read_file", "Read a file."), tool("grep", "Search.")],
            &mode_wiring(Mode::DeclareBuiltin),
            &BTreeMap::new(),
            &BTreeMap::new(),
            100_000,
            &allow_all(),
        );
        let codemode = tools.last().expect("the tool was added");
        let script = r#"
            let a = tools::read_file(#{ path: "a.rs" });
            let b = tools::grep(#{ path: "b.rs" });
            text(a + "|" + b)
        "#;

        let mut context = ToolContext::new();
        let output = codemode
            .run_with(&mut context, json!({ "script": script }))
            .await
            .expect("the script runs");
        let text = output.render();

        // The value the script returned comes first, then the run's report.
        assert!(text.contains("read_file ran|grep ran"), "{text}");
        assert!(text.contains("Tool calls (2)"), "{text}");
        assert!(text.contains("tools::read_file"), "{text}");
        assert!(text.contains("tools::grep"), "{text}");
    }

    /// A one-server MCP roster carrying one tool, keyed by the composite name
    /// the roster registers it under.
    fn mcp_roster(server: &str, tool_name: &str) -> BTreeMap<String, shuvarie_mcp::McpToolMap> {
        let composite = shuvarie_mcp::composite_tool_name(server, tool_name);
        let info = shuvarie_mcp::McpToolInfo {
            name: tool_name.to_string(),
            title: None,
            description: Some("An MCP tool.".to_string()),
            input_schema: None,
        };
        BTreeMap::from([(server.to_string(), BTreeMap::from([(composite, info)]))])
    }

    /// The configured `tool` entries a turn would pass, by name.
    fn stdio(names: &[&str]) -> BTreeMap<String, shuvarie_config::StdioToolConfig> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    shuvarie_config::StdioToolConfig::default(),
                )
            })
            .collect()
    }

    #[test]
    fn the_catalog_carries_the_roster() {
        let tools = vec![tool("read_file", "Read a file."), tool("grep", "Search.")];
        let catalog = catalog_for(&tools, &BTreeMap::new());
        assert_eq!(
            catalog
                .tools()
                .iter()
                .map(|spec| spec.qualified())
                .collect::<Vec<_>>(),
            ["tools::grep", "tools::read_file"]
        );
    }

    #[test]
    fn mcp_tools_land_in_their_servers_namespace() {
        let tools = vec![tool("dev_radius__resolve-library-id", "Resolve.")];
        let servers = BTreeMap::from([(
            "dev_radius__resolve-library-id".to_string(),
            ("dev-radius".to_string(), "resolve-library-id".to_string()),
        )]);
        let catalog = catalog_for(&tools, &servers);
        assert_eq!(
            catalog.tools()[0].qualified(),
            "mcp::dev_radius::resolve_library_id"
        );
        // ... while dispatch stays on the roster's composite name.
        assert_eq!(
            catalog.tools()[0].host_name,
            "dev_radius__resolve-library-id"
        );
    }

    #[test]
    fn output_becomes_text_or_structure_for_the_script() {
        assert_eq!(
            output_as_json(&ToolOutput::text("hello")),
            Value::String("hello".to_string())
        );
        assert_eq!(
            output_as_json(&ToolOutput::json(json!({ "id": "/dev/x" }))),
            json!({ "id": "/dev/x" })
        );
    }

    #[test]
    fn declared_mode_keeps_every_tool_and_adds_the_one() {
        let tools = install(
            vec![tool("read_file", "Read a file."), tool("grep", "Search.")],
            &mode_wiring(Mode::Declared),
            &BTreeMap::new(),
            &BTreeMap::new(),
            100,
            &access(),
        );
        assert_eq!(names(&tools), ["read_file", "grep", NAME]);
        assert!(
            tools
                .last()
                .unwrap()
                .definition()
                .description
                .contains("tools::read_file"),
            "the description carries the catalog"
        );
        assert!(
            !tools
                .last()
                .unwrap()
                .definition()
                .description
                .contains("(script only)"),
            "declared mode hides nothing, so nothing is marked"
        );
    }

    /// In `only` mode the scriptable tools leave the model's roster, but a tool
    /// a script may not run has to stay: the model cannot call it and no script
    /// can either, so dropping it would delete it.
    #[test]
    fn only_mode_hides_the_scriptable_tools_but_keeps_the_rest() {
        let tools = install(
            vec![
                tool("read_file", "Read a file."),
                tool("run_shell", "Run a command."),
            ],
            &only("run_shell"),
            &BTreeMap::new(),
            &BTreeMap::new(),
            100,
            &access(),
        );
        assert_eq!(names(&tools), ["run_shell", NAME]);
        // `read_file` is hidden, so the catalog has to say a script is the way
        // in; `run_shell` is not (no script may run it), so it stays unmarked.
        let description = tools.last().unwrap().definition().description.clone();
        assert!(
            description.contains(
                "tools::read_file(path?: String) -> Dynamic\n    Read a file. (script only)"
            ),
            "{description}"
        );
        assert!(
            !description.contains("Run a command. (script only)"),
            "{description}"
        );
    }

    /// `declare-builtin` is the split that pays for itself: the roster's own
    /// tools stay directly callable, while the two configured sources (a `tool`
    /// subprocess entry and an MCP server's tools) are reachable only from a
    /// script.
    #[test]
    fn declare_builtin_hides_stdio_and_mcp_but_keeps_the_builtins() {
        let mcp = mcp_roster("dev-radius", "resolve-library-id");
        let composite = shuvarie_mcp::composite_tool_name("dev-radius", "resolve-library-id");
        let tools = install(
            vec![
                tool("read_file", "Read a file."),
                tool("grep", "Search."),
                tool("fetch-json", "A configured tool."),
                tool(&composite, "An MCP tool."),
            ],
            &mode_wiring(Mode::DeclareBuiltin),
            &mcp,
            &stdio(&["fetch-json"]),
            100,
            &access(),
        );

        assert_eq!(names(&tools), ["read_file", "grep", NAME]);
        // ... and the hidden ones are still callable from a script, under the
        // names the catalog prints, marked as script-only so the model does not
        // have to infer which tools it cannot call itself.
        let description = tools.last().unwrap().definition().description.clone();
        assert!(description.contains("tools::read_file"), "{description}");
        assert!(
            description.contains(
                "tools::fetch_json(path?: String) -> Dynamic\n    \
                 A configured tool. (tool name \"fetch-json\") (script only)"
            ),
            "{description}"
        );
        assert!(
            description.contains(
                "mcp::dev_radius::resolve_library_id(path?: String) -> Dynamic\n    \
                 An MCP tool. (tool name \"resolve-library-id\") (script only)"
            ),
            "{description}"
        );
        assert!(
            !description.contains("Read a file. (script only)"),
            "a builtin the model keeps is not script-only: {description}"
        );
        assert!(
            description.contains("not on your own roster"),
            "{description}"
        );
    }

    /// The same rule `only` mode follows: a configured tool a script may not run
    /// has to stay directly callable, or it would be reachable by neither route.
    #[test]
    fn declare_builtin_keeps_a_configured_tool_a_script_may_not_run() {
        let mcp = mcp_roster("dev-radius", "resolve-library-id");
        let composite = shuvarie_mcp::composite_tool_name("dev-radius", "resolve-library-id");
        let tools = install(
            vec![
                tool("read_file", "Read a file."),
                tool("fetch-json", "A configured tool."),
                tool(&composite, "An MCP tool."),
            ],
            // The script may not orchestrate the MCP tool, so it must stay
            // visible to the model even though its source is script-only.
            &Wiring {
                mode: Mode::DeclareBuiltin,
                tool_scene: ToolScene::build(Some(&shuvarie_config::SceneToolsConfig {
                    tools: BTreeMap::from([(
                        composite.clone(),
                        shuvarie_config::ToolOverride {
                            disabled: Some(true),
                            ask: None,
                        },
                    )]),
                    ..Default::default()
                })),
                ..Wiring::new(Arc::new(Store::new()))
            },
            &mcp,
            &stdio(&["fetch-json"]),
            100,
            &access(),
        );

        assert_eq!(names(&tools), ["read_file", composite.as_str(), NAME]);
    }

    /// A configured tool the scene disables never reaches the roster at all, so
    /// `declare-builtin` has nothing to hide: the configured name is in `stdio`
    /// but not in `tools`.
    #[test]
    fn declare_builtin_ignores_a_tool_the_scene_already_removed() {
        let tools = install(
            // `fetch-json` is configured but the scene dropped it, so the roster
            // the turn built never carried it.
            vec![tool("read_file", "Read a file.")],
            &Wiring {
                mode: Mode::DeclareBuiltin,
                ..Wiring::new(Arc::new(Store::new()))
            },
            &BTreeMap::new(),
            &stdio(&["fetch-json"]),
            100,
            &access(),
        );
        assert_eq!(names(&tools), ["read_file", NAME]);
    }

    #[test]
    fn the_catalog_never_offers_codemode_itself() {
        let tools = install(
            vec![tool("read_file", "Read a file."), tool(NAME, "Recursive.")],
            &mode_wiring(Mode::Declared),
            &BTreeMap::new(),
            &BTreeMap::new(),
            100,
            &access(),
        );
        let description = tools.last().unwrap().definition().description.clone();
        assert!(!description.contains("tools::codemode"), "{description}");
    }
}
