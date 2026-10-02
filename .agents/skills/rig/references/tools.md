# Tools

Tools let an agent do more than generate text: they expose your Rust functions to the model so it can fetch data, run computations, or reach external systems. When the model decides a tool is needed, Rig parses the call, runs your code, feeds the result back, and continues the loop.

Official docs: https://rig.rs/docs/concepts/tools · API: https://docs.rs/rig-core/latest/rig_core/tool/index.html (0.43: the contracts live in `rig-core-0.43.0/src/tool/`, the runtime registration in `rig-agent-0.43.0/src/tool/`)

## Complete example (0.43)

Two tool authoring surfaces:
- **`Tool`** (contextual) — `call(&self, context: &mut ToolContext, args)`; the classic contract, re-exported at `rig_agent::tool::Tool` (defined in `rig_core::tool`).
- **`PortableTool`** — context-free: `call(&self, args)`. Portable types get a blanket `Tool` impl, so a portable tool works everywhere the runtime needs one.

`#[rig_tool]` (the attribute macro; `tool_macro` was renamed) derives the `Tool` impl from a function:

```rust
use rig_agent::{AgentBuilder, tool::Tool};
use rig_core::providers::openai::{self, OpenAI};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct OperationArgs { x: i32, y: i32 }

// From a plain function via the macro (type name is PascalCase: subtract -> Subtract).
#[rig_agent::rig_tool(
    description = "Subtract y from x",
    params(x = "the first operand", y = "the second operand"),
    required(x, y)
)]
async fn subtract(x: i32, y: i32) -> Result<i32, rig_core::tool::ToolExecutionError> {
    Ok(x - y)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let calculator = AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a calculator. Use the provided tools.")
        .max_tokens(1024)
        .tool(subtract)
        .build();
    let answer = calculator.prompt("What is 5 - 2?").await?;
    println!("{}", answer.output);
    Ok(())
}
```

> A prompt that triggers **several** tool calls in sequence needs turn budget — add `.max_turns(n)` or the run fails with `PromptError::MaxTurnsError`.

## The `Tool` trait (contextual)

```rust
pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
    type Args: for<'de> Deserialize<'de> + Send + Sync;
    type Output: IntoToolOutput;           // any Serializable, or ToolOutput, or Vec<ToolResultContent>
    type Error: Error + Send + Sync;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;    // JSON Schema
    fn map_error(&self, error: Self::Error) -> ToolExecutionError; // default: from_error
    fn call(&self, context: &mut ToolContext, args: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}
```

```rust
// Hand-written contextual tool.
struct Adder;
impl Tool for Adder {
    const NAME: &'static str = "add";
    type Args = OperationArgs;
    type Output = i32;
    type Error = MathError;

    fn description(&self) -> String { "Add x and y together".into() }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number", "description": "First number to add" },
                "y": { "type": "number", "description": "Second number to add" }
            },
            "required": ["x", "y"]
        })
    }
    async fn call(&self, _ctx: &mut rig_core::tool::ToolContext, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(args.x + args.y)
    }
}
```

- `const NAME` — unique id the model uses to reference the tool.
- `Args` — `Deserialize` type the model's JSON arguments parse into (raw-JSON tools use `type Args = serde_json::Value`).
- `Output` — what your tool returns on success. `IntoToolOutput` is implemented for every owned serializable value (as text) and for `ToolResultContent`/`Vec<ToolResultContent>`/`ToolOutput` (preserving rich content).
- `Error` — your error type; Rig normalizes it into `ToolExecutionError` at the dispatch boundary via `map_error` (default `from_error`, which redacts into kind-level model feedback).
- `description()` + `parameters()` — the provider-facing tool definition.
- `call(&mut ToolContext, args)` — the execution logic. Read caller-provided runtime values via `context.require::<T>()` (see `ToolContext` below).

## `ToolContext` (0.43: typed, serde-backed values)

`ToolContext` is a mutable, run-scoped value store (inbound values + published result metadata + driver scopes) passed to every `Tool::call`. Values are **`ContextValue`s**: serde-backed types with a stable slot `KEY`.

```rust
use rig_core::tool::{ToolContext, ContextValue};

// A typed context value (KEY defaults to the type name; a bare String/int cannot be stored):
#[derive(serde::Serialize, serde::Deserialize, rig_core::ContextValue)]
#[context(key = "api.token")]   // optional override
struct ApiToken(String);

// Author attaches it to the run:
let ctx = {
    let mut c = ToolContext::new();
    let _ = c.insert(ApiToken("t".into()));
    c
};
let response = agent
    .prompt("...")
    .tool_context(ctx)
    .run().await?;

// Tool reads it:
async fn call(&self, ctx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, Self::Error> {
    let token: ApiToken = ctx.require()?;      // Result<_, ToolContextError> on absence/shape mismatch
    // ...
    let _ = ctx.insert_result(SomethingPublished { .. }); // host-only result metadata (Result-returning)
}
```

Two namespaces: `insert/get/require/remove` for **inbound** values; `insert_result/result/require_result` for **published result metadata** (read after the call by hooks via `OutcomeEvent.tool_context()`, e.g. shuvarie's `FileChange`/`ShellStreams`/`SpawnTag`). `insert_result` values survive as `ToolResultContext` snapshots; they are never model-visible.

> OpenAI Responses API requires every input parameter under `required`. Include a `"required"` array, or use `schemars::JsonSchema` (non-`Option` fields are required), or the macro's `required(...)`.

## Deriving the schema with `schemars`

**v1.0 required.** Derive `schemars::JsonSchema` and describe each field with a `///` doc comment:

```rust
#[derive(Deserialize, Serialize, schemars::JsonSchema)]
struct OperationArgs {
    /// The first number to add.
    x: i32,
    /// The second number to add.
    y: i32,
}

fn parameters(&self) -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(OperationArgs)).unwrap()
}
```

Migration from v0.8: descriptions move from `#[schemars(description = "...")]` to `///` doc comments; `schema_for!` becomes `schemars::schema_for!` (or `T::json_schema()`); pin `schemars = "1"`.

## The `rig_tool` macro

For simple tools, `#[rig_tool(...)]` turns a plain function into a tool type (named in PascalCase):

```rust
#[rig_agent::rig_tool(
    description = "Basic arithmetic",
    params(x = "the first operand", y = "the second operand", operation = "add|subtract|divide"),
    required(x, y, operation)
)]
async fn calculator(x: i32, y: i32, operation: String) -> Result<i32, rig_core::tool::ToolExecutionError> {
    match operation.as_str() {
        "add" => Ok(x + y),
        "subtract" => Ok(x - y),
        "divide" if y == 0 => Err(rig_core::tool::ToolExecutionError::invalid_args("Division by zero")),
        "divide" => Ok(x / y),
        _ => Err(rig_core::tool::ToolExecutionError::other(format!("Unknown op: {operation}"))),
    }
}
```

(`#[rig::tool_macro(...)]` — 0.42's alias — is gone; the macro is `rig_tool` on both `rig_core` and `rig_agent`, `derive` feature.) The macro derives the argument struct (schema from `params(...)` descriptions), the JSON schema, and the `Tool` impl; the generated type is passed to `.tool(...)` like any other.

> The macro's error type is `rig_core::tool::ToolExecutionError`. Use the typed constructors — `.other(msg)`, `invalid_args`, `timeout`, `cancelled`, `not_found`, `permission_denied`, `rate_limited`, `network`, `provider` — or `ToolExecutionError::new(kind, msg)` with a `ToolErrorKind`.

## When a tool call fails

`call` returning `Err` does **not** abort the prompt. Rig converts the error to its model-facing output (via `map_error`, default `from_error` — which redacts to kind-level feedback and keeps the operator message on the error) and sends it back to the model as the tool result; the loop continues. Tool errors get a `ToolErrorKind` (retryability defaults per kind, e.g. `Timeout`/`Network`/`RateLimited` are retryable).

- **Make error messages instructive** — the model is the audience. `"Division by zero"` lets it recover; `"error 500"` doesn't. For sensitive diagnostics that must not reach the model, construct with `ToolExecutionError::other(kind_text)` and attach your real detail with `.with_source(..)` / read it back with `downcast_ref::<E>()`.
- **Budget turns for recovery** — a retry costs a turn; use `.max_turns(...)`.

A model calling a tool that doesn't exist fails the prompt immediately by default; a hook can opt into retry/repair/skip recovery (see `hooks.md`). Hook-steered results: tools can also be denied/patched/skipped at the `on_dispatch` boundary, and their model-visible output rewritten at `on_outcome` (`OutcomeAction::rewrite_tool_result`).

## Designing good tools

- **Name tools descriptively** in `snake_case`, no abbreviations: `search_orders` beats `so`.
- **Write descriptions for the model**, not docs. Say what the tool does, when to use it, when not to.
- **Describe every parameter** and keep parameters few and primitive. Three well-described string/number fields beat one nested object.
- **Offer few tools per request.** Selection quality degrades past ~10–20. Split across specialized agents or use dynamic tool retrieval.
- **Return compact results.** Tool output is prompt input next turn — filter/summarize in the tool, not in the prompt. For large/sensitive data, return an ID your code resolves later.

## Attaching tools to an agent

**Static tools** — always available:

```rust
let agent = rig_agent::AgentBuilder::new(model)
    .preamble("You are a calculator.")
    .tool(Adder)
    .tool(Subtract)
    .build();
```

The builder is typestate: `NoToolConfig` → `WithBuilderTools` once you add one tool; keep chaining `tool`/`dynamic_tool(s)`/`retrieved_tools`. Build with zero tools via plain `.build()`; build over a shared registry via `.tool_server_handle(handle).build()`.

**Runtime-defined tools** — see "Dynamic tools" below. Attach with `.dynamic_tool(t)` / `.dynamic_tools(vec)`.

## Tool-RAG with `ToolEmbedding`

Implement `ToolEmbedding` (in addition to `Tool`) to make a tool retrievable. It provides the text to embed (`embedding_docs`) plus the state/context to reconstruct the tool:

```rust
use rig_core::tool::ToolEmbedding;

impl ToolEmbedding for Adder {
    type InitError = InitError;
    type Context = ();
    type State = ();
    fn init(_state: Self::State, _context: Self::Context) -> Result<Self, Self::InitError> {
        Ok(Adder)
    }
    fn embedding_docs(&self) -> Vec<String> {
        vec!["Add x and y together".into()]
    }
    fn context(&self) -> Self::Context {}
}
```

Embed tools into a `VectorStoreIndex`, attach with `.retrieved_tools(sample, index, toolset)`:

```rust
use rig_agent::tool::{ToolSet, ToolEmbedding};
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

let mut toolset = ToolSet::default();
let _ = toolset.add_retrieved_tool(Adder);        // registers as an embedding-capable tool
let schemas = toolset.schemas()?;
let embeddings = EmbeddingsBuilder::new(embed_model.clone())
    .documents(schemas)?
    .build()
    .await?;

let vector_store =
    InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
let index = vector_store.index(embed_model);

let agent = AgentBuilder::new(model)
    .preamble("You are a calculator. Use the tools.")
    .retrieved_tools(2, index, toolset)
    .build();
```

At each turn the agent fetches the `sample` most relevant tools and offers only those; called tools are executed from the toolset. (`ToolSchema` — the embeddable form of a tool definition — comes from `rig_core::embeddings::ToolSchema` via `ToolSet::schemas()`.)

## Dynamic tools

For a tool whose name/description/callback is only known at runtime, build a `DynamicTool` directly. 0.43 has two constructors: `new` (context-free, args only) and `new_with_context` (carries the `&mut ToolContext`), plus `.with_liveness(..)` for registry retirement:

```rust
use rig_agent::tool::DynamicTool;

// Context-free callback:
let tool = DynamicTool::new(
    "echo",
    "Echo the text back",
    json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"]
    }),
    |args| Box::pin(async move {
        Ok(rig_core::tool::ToolOutput::text(args["text"].as_str().unwrap_or_default()))
    }),
);

// Context-carrying callback (e.g. wrapping a typed Tool that needs `call(ctx, args)`):
let tool = DynamicTool::new_with_context(
    name, tool.description(), tool.parameters(),
    move |ctx, args| {
        let tool = Arc::clone(&tool);
        Box::pin(async move { tool.call(ctx, args).await })
    },
);
```

Attach with `.dynamic_tool(t)` / `.dynamic_tools(vec)`. (0.42's `PortableDynamicTool`/`DynamicTool::from_portable` are gone — context-free wrapping is `new`, contextual wrapping is `new_with_context`.)

## Tool servers

Sharing a mutable tool set across async tasks usually means `Arc<Mutex<T>>` (lock contention/deadlocks). Rig's tool server runs tools in a registry behind a handle and communicates over message passing. As long as one handle copy lives, the server keeps serving:

```rust
use rig_agent::tool::server::{ToolServer, ToolServerHandle};

let tool_server: ToolServerHandle = ToolServer::new()
    .tool(Adder)
    .run();          // no executor spawned: the bus drives registrations
```

Tool servers accept static, dynamic, and (via the `rig-rmcp` companion crate) MCP tools. Attach the handle with `AgentBuilder::tool_server_handle(..)`; handing several agents clones of one handle lets them share one tool set — and external sources (an MCP client handler, for example) can register/refresh tools mid-run through `agent.tool_server_handle()`, which the agent sees on its next turn.

## Tool output & errors

- **`ToolOutput`** (`rig_core::tool`) — the canonical model-visible output: one or more typed `ToolResultContent` blocks. `ToolOutput::text(...)`, `ToolOutput::json(...)`, `ToolOutput::content(vec)?` / `.one(..)`, `.as_text()`, `.as_json()`, `.as_content()`, `.render()` (model text). Return it directly as `type Output = ToolOutput`, or let any owned serializable value flow through `IntoToolOutput`.
- **`ToolResult`** (`rig_core::tool`) — the runtime execution record (`ToolResult::success(output)` / `failed(error)` / `skipped(reason)` / `.with_output(..)` / `.is_error()`/`is_skipped()`/`is_refused()` / `.output()`/`.error()`). This is what the bus dispatches and what hooks observe; the *message-layer* `rig_core::message::ToolResult` (the model-visible reply) still exists separately (see `completions.md`).
- **`ToolExecutionError`** (`rig_core::tool`) — the normalized runtime error. Construct with `ToolExecutionError::new(kind, message)` where `kind: ToolErrorKind` (`InvalidArgs`/`Timeout`/`Cancelled`/`NotFound`/`PermissionDenied`/`RateLimited`/`Provider`/`Network`/`Other`), the per-kind constructors (`other(msg)`, `invalid_args(msg)`, …), `from_error(e)`, or `refused(msg)` (intentional refusal — a distinct disposition hooks/telemetry can observe). Enrich with `.with_model_feedback(..)` / `.with_model_output(..)` / `.with_retryable(..)` / `.with_code(..)` / `.with_http_status(..)` / `.with_source(..)`; inspect with `.message()`, `.model_feedback()`, `.code()`, `downcast_ref::<E>()`.

## MCP tools

Tools don't have to live in your crate. The [Model Context Protocol](https://modelcontextprotocol.io) lets an agent consume tools served by external processes (filesystem, browsers, databases, SaaS). For 0.43 the MCP integration ships as the native-only **`rig-rmcp`** companion crate (an MCP client handler registering into an agent's `tool_server_handle()`); wire its tools through the tool-server surface. See https://rig.rs/docs/integrations/model_context_protocol.

## Tool organization

Tools are collected in a `ToolSet` (`rig_agent::tool::ToolSet`), which registers tools (`add_tool`/`add_dynamic_tool`/`add_retrieved_tool`/`add_registered_tool`, `from_tools`/`from_dynamic_tools`), looks them up by name, routes calls, and (for `ToolEmbedding` tools) produces embeddings (`schemas()`) for dynamic retrieval. On the runtime side the `ToolCatalog`/`ToolServer` snapshot the tools a turn advertises and execute them per dispatch. When you attach tools to an agent, Rig converts each `ToolDefinition` into the provider's format, parses model output into tool calls, executes them, and returns results to the model.