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
//! tool itself or a script calls it. What the invoker adds is the two stream
//! items (`ToolStart`/`ToolResult`) a direct call would have produced, carrying
//! the nested call's [`FileChange`] or [`ShellStreams`], so the transcript, the
//! sidebar and the session's tool records see it too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};
use shuvarie_codemode::script::InvokeFuture;
use shuvarie_codemode::{
    Catalog, Limits, RenderOptions, ScriptRequest, Stop, Store, ToolCall, ToolEntry, ToolInvoker,
    block, describe, run,
};
use shuvarie_llm::{
    DynamicTool, FileChange, ShellStreams, StreamItem, Tool, ToolContext, ToolExecutionError,
    ToolOutput, into_dynamic,
};
use tokio::sync::mpsc::Sender;

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
    /// Where nested calls are announced, so the turn stream can show them.
    pub activity: Option<Sender<StreamItem>>,
}

impl Wiring {
    /// The default wiring, for a store and an activity channel.
    pub fn new(store: Arc<Store>, activity: Option<Sender<StreamItem>>) -> Self {
        Self {
            mode: Mode::default(),
            limits: Limits::default(),
            render: RenderOptions::default(),
            tool_scene: ToolScene::build(None),
            store,
            activity,
        }
    }
}

/// Take the roster the turn built and return the roster the model sees, with
/// the tool added.
///
/// `mcp` is the per-server roster those tools were built from, used only to
/// give an MCP tool its namespace (`mcp::dev_radius::search`) rather than the
/// composite name it dispatches by.
pub fn install(
    tools: Vec<DynamicTool>,
    wiring: &Wiring,
    mcp: &BTreeMap<String, shuvarie_mcp::McpToolMap>,
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

    let codemode = into_dynamic(
        NAME,
        CodeMode {
            description: describe(&catalog, &wiring.render),
            catalog: Arc::new(catalog),
            roster: Arc::clone(&roster),
            // The tool's own access, so its scene overrides work like any other
            // gated tool's.
            access: access.for_tool(NAME, &wiring.tool_scene),
            store: Arc::clone(&wiring.store),
            limits: wiring.limits.clone(),
            activity: wiring.activity.clone(),
            max_output_chars,
        },
    );

    match wiring.mode {
        Mode::Declared => {
            let mut tools = tools;
            tools.push(codemode);
            tools
        }
        Mode::Only => {
            // What a script cannot run has to stay in the model's roster:
            // dropping it would leave a tool the model may not call and no
            // script can either, which is gone rather than hidden.
            let scriptable: BTreeSet<String> = roster.keys().cloned().collect();
            let mut kept: Vec<DynamicTool> = tools
                .into_iter()
                .filter(|tool| !scriptable.contains(&tool.definition().name.to_string()))
                .collect();
            kept.push(codemode);
            kept
        }
    }
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
    activity: Option<Sender<StreamItem>>,
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
            activity: self.activity.clone(),
            run: uuid::Uuid::new_v4().to_string(),
            calls: AtomicUsize::new(0),
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
struct RosterInvoker {
    roster: Arc<BTreeMap<String, DynamicTool>>,
    activity: Option<Sender<StreamItem>>,
    /// This run's id. A tool cannot see its own call id -- rig hands ids to
    /// hooks, not to the tool -- so nested calls are announced with ids of
    /// their own under the run, which is what the transcript and the session's
    /// tool records key on.
    run: String,
    calls: AtomicUsize,
}

impl RosterInvoker {
    /// Announce a nested call, and never let the announcement hold up the
    /// script: a closed channel means the turn is gone, which is not the
    /// script's problem.
    async fn announce(&self, item: StreamItem) {
        if let Some(activity) = &self.activity {
            let _ = activity.send(item).await;
        }
    }
}

impl ToolInvoker for RosterInvoker {
    fn invoke(&self, call: ToolCall) -> InvokeFuture<'_> {
        Box::pin(async move {
            let Some(tool) = self.roster.get(&call.host_name) else {
                return Err(format!("no such tool: {}", call.host_name));
            };

            let sequence = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            let call_id = format!("{}:{sequence}", self.run);
            let name = call.host_name.clone();
            self.announce(StreamItem::ToolStart {
                name: name.clone(),
                args: call.args.clone(),
                worker: None,
                spawn: None,
                call_id: call_id.clone(),
            })
            .await;

            // A fresh context per call: the tool writes its host-only metadata
            // ([`FileChange`], [`ShellStreams`]) into it, and nothing in the
            // roster reads anything else out of a context.
            let mut context = ToolContext::new();
            let result = tool.execute_with(&mut context, call.args).await;
            let file_change = context.result::<FileChange>().ok().flatten();
            let streams = context.result::<ShellStreams>().ok().flatten();

            let (output, ok) = match &result {
                Ok(output) => (output.render(), true),
                Err(error) => (error.message().to_string(), false),
            };
            self.announce(StreamItem::ToolResult {
                name,
                output,
                ok,
                worker: None,
                spawn: None,
                file_change,
                streams,
                call_id,
            })
            .await;

            match result {
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
            ..Wiring::new(Arc::new(Store::new()), None)
        }
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
            &Wiring::new(Arc::new(Store::new()), None),
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
            100,
            &access(),
        );
        assert_eq!(names(&tools), ["run_shell", NAME]);
    }

    #[test]
    fn the_catalog_never_offers_codemode_itself() {
        let tools = install(
            vec![tool("read_file", "Read a file."), tool(NAME, "Recursive.")],
            &Wiring::new(Arc::new(Store::new()), None),
            &BTreeMap::new(),
            100,
            &access(),
        );
        let description = tools.last().unwrap().definition().description.clone();
        assert!(!description.contains("tools::codemode"), "{description}");
    }
}
