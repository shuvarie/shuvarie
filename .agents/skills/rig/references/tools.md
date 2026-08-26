# Tools

Tools let an agent do more than generate text: they expose your Rust functions to the model so it can fetch data, run computations, or reach external systems. When the model decides a tool is needed, Rig parses the call, runs your code, feeds the result back, and continues the loop.

Official docs: https://rig.rs/docs/concepts/tools · API: https://docs.rs/rig/latest/rig/tool/index.html

## Complete example

In 0.42 there are two tool authoring surfaces:
- **`PortableTool`** (`rig::tool`) — context-free: `call(&self, args)`, no runtime context. The canonical runtime-independent contract.
- **`Tool`** (classic, `rig::tool`) — contextual: `call(&self, context: &mut ToolContext, args)`. `PortableTool` types get a blanket `Tool` impl, so a portable tool works everywhere the classic runtime needs one.

The `tool_macro` derives the `Tool` impl (both `rig::rig_tool` and `rig::tool_macro` are exported from `rig_derive`):

```rust
use rig::{
    client::{CompletionClient, ProviderClient},
    completion::Prompt,
    providers::openai,
    tool::Tool,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
struct OperationArgs { x: i32, y: i32 }

#[derive(Debug)]
struct MathError;
impl std::fmt::Display for MathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "math error") }
}
impl std::error::Error for MathError {}

// From a plain function via the macro (type name is PascalCase: subtract -> Subtract).
#[rig::tool_macro(description = "Subtract y from x", required(x, y))]
async fn subtract(x: i32, y: i32) -> Result<i32, rig::tool::ToolExecutionError> {
    Ok(x - y)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = openai::Client::from_env()?;
    let calculator = openai
        .agent("gpt-5.5")
        .preamble("You are a calculator. Use the provided tools.")
        .max_tokens(1024)
        .tool(subtract)
        .build();
    let answer = calculator.prompt("What is 5 - 2?").await?;
    println!("{answer}");
    Ok(())
}
```

> A prompt that triggers **several** tool calls in sequence needs turn budget — add `.max_turns(n)` or the run fails with `MaxTurnsError`.

## The `Tool` trait (classic, contextual)

```rust
pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
    type Args: for<'de> Deserialize<'de> + Send + Sync;
    type Output: IntoToolOutput;           // any Serializable, or ToolOutput, or Vec<ToolResultContent>
    type Error: Error + Send + Sync;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;    // JSON Schema
    fn call(&self, context: &mut ToolContext, args: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}
```

In 0.42 the trait split `definition` into `description()` + `parameters()`, and `call` now takes a mutable `&mut ToolContext`:

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
    async fn call(&self, _ctx: &mut rig::tool::ToolContext, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(args.x + args.y)
    }
}
```

- `const NAME` — unique id the model uses to reference the tool.
- `Args` — `Deserialize` type the model's JSON arguments parse into.
- `Output` — what your tool returns on success. `IntoToolOutput` is implemented for every owned serializable value (as text) and for `ToolResultContent`/`Vec<ToolResultContent>`/`ToolOutput` (preserving rich content).
- `Error` — your error type; Rig normalizes it into `ToolExecutionError` at the dispatch boundary via `map_error` (default: `ToolErrorKind::Other`).
- `description()` + `parameters()` — the provider-facing tool definition.
- `call(&mut ToolContext, args)` — the execution logic. Read caller-provided runtime values via `context.require::<T>()` (see `ToolContext` below).

## `ToolContext`

`ToolContext` is a mutable type map passed to every `Tool::call`. It's how you inject runtime-only values (auth tokens, tenant IDs, session state) the model should never see — the 0.42 replacement for the old `tool_extensions`/`call_with_extensions`:

```rust
use rig::tool::ToolContext;

// Author attaches it to the run/request:
let ctx = {
    let mut c = ToolContext::new();
    c.insert("api-token".to_string());
    c
};
let response = agent
    .runner("...")
    .tool_context(ctx)
    .run().await?;

// Tool reads it:
async fn call(&self, ctx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, Self::Error> {
    let api_token: &String = ctx.require()?;   // MissingToolContext on absence
    // ...
}
```

> OpenAI Responses API requires every input parameter under `required`.** Include a `"required"` array, or use `schemars::JsonSchema` (non-`Option` fields are required), or the macro's `required(...)`.

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

## The `tool_macro` / `rig_tool`

For simple tools, `#[rig::tool_macro(...)]` (alias `#[rig::rig_tool]`) turns a plain function into a tool type (named in PascalCase):

```rust
#[rig::tool_macro(description = "Basic arithmetic", required(x, y, operation))]
async fn calculator(x: i32, y: i32, operation: String) -> Result<i32, rig::tool::ToolExecutionError> {
    match operation.as_str() {
        "add" => Ok(x + y),
        "subtract" => Ok(x - y),
        "divide" if y == 0 => Err(rig::tool::ToolExecutionError::invalid_args("Division by zero")),
        "divide" => Ok(x / y),
        _ => Err(rig::tool::ToolExecutionError::other(format!("Unknown op: {operation}"))),
    }
}
```

The macro derives the argument struct, the JSON schema, and the `Tool` impl. The generated type is passed to `.tool(...)` like any other. Requires the `derive` feature on `rig`.

> The macro's error type is `rig::tool::ToolExecutionError` (there is no separate `ToolError` in 0.42). Use the typed constructors — `ToolExecutionError::other(msg)`, `invalid_args`, `timeout`, `cancelled`, `not_found`, `permission_denied`, `rate_limited`, `network`, `provider` — or `ToolExecutionError::new(kind, msg)` with a `ToolErrorKind`.

## When a tool call fails

`call` returning `Err` does **not** abort the prompt. Rig converts the error to its model-facing output (via `map_error`, default `ToolErrorKind::Other`) and sends it back to the model as the tool result; the loop continues.

- **Make error messages instructive** — the model is the audience. `"Division by zero"` lets it recover; `"error 500"` doesn't. For sensitive diagnostics, override `map_error` and call `redact_model_feedback()` so the model sees stable kind-specific text while the operator diagnostic stays on the error.
- **Budget turns for recovery** — a retry costs a turn; use `.max_turns(...)`.

A model calling a tool that doesn't exist fails the prompt immediately by default; a hook can opt into retry/repair/skip recovery (see `hooks.md`).

## Designing good tools

- **Name tools descriptively** in `snake_case`, no abbreviations: `search_orders` beats `so`.
- **Write descriptions for the model**, not docs. Say what the tool does, when to use it, when not to.
- **Describe every parameter** and keep parameters few and primitive. Three well-described string/number fields beat one nested object.
- **Offer few tools per request.** Selection quality degrades past ~10–20. Split across specialized agents or use dynamic tool retrieval.
- **Return compact results.** Tool output is prompt input next turn — filter/summarize in the tool, not in the prompt. For large/sensitive data, return an ID your code resolves later.

## Attaching tools to an agent

**Static tools** — always available:

```rust
let agent = client
    .agent("gpt-5.5")
    .preamble("You are a calculator.")
    .tool(Adder)
    .tool(Subtract)
    .build();
```

**Dynamic tools** — retrieved from a vector store at prompt time by semantic similarity (see next section + `rag-and-vector-stores.md`).

## Tool-RAG with `ToolEmbedding`

Implement `ToolEmbedding` (in addition to `Tool`) to make a tool retrievable. It provides the text to embed (`embedding_docs`) plus the state/context to reconstruct the tool:

```rust
use rig::tool::ToolEmbedding;

#[derive(Debug)]
struct InitError;
impl std::fmt::Display for InitError { /* ... */ }
impl std::error::Error for InitError {}

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

Embed tools into a `VectorStoreIndex`, attach with `.retrieved_tools(sample, index, toolset)` (the 0.42 replacement for the old `dynamic_tools(n, index, toolset)`):

```rust
use rig::tool::{ToolSet, ToolEmbedding};

let mut toolset = ToolSet::default();
toolset.add_retrieved_tool(Adder);      // registers as an embedding-capable tool
let embeddings = EmbeddingsBuilder::new(embed_model.clone())
    .documents(toolset.schemas()?)?
    .build()
    .await?;

let vector_store =
    InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
let index = vector_store.index(embed_model);

let agent = client
    .agent("gpt-5.5")
    .preamble("You are a calculator. Use the tools.")
    .retrieved_tools(2, index, toolset)
    .build();
```

At each turn the agent fetches the `n` most relevant tools and offers only those; called tools are executed from the toolset. (Note: `dynamic_tool`/`dynamic_tools` in 0.42 now attach `DynamicTool` values directly — they no longer take an index/toolset. Use `ToolSet::default()` + `add_retrieved_tool(...)` for retrievable tools, or `add_tool`/`add_dynamic_tool` for plain ones.)

## Dynamic tools

For a tool whose name/description/callback is only known at runtime (not a `Tool` type), build a `DynamicTool` directly (or a `PortableDynamicTool` for a context-free version, wrapped via `DynamicTool::from_portable`):

```rust
use rig::tool::DynamicTool;

let tool = DynamicTool::new(
    "echo",
    "Echo the text back",
    json!({
        "type": "object",
        "properties": { "text": { "type": "string" } },
        "required": ["text"]
    }),
    |_ctx, args| Box::pin(async move {
        Ok(rig::tool::ToolOutput::text(args["text"].as_str().unwrap_or_default()))
    }),
);

let agent = client.agent("gpt-5.5").dynamic_tool(tool).build();
```

Attach with `.dynamic_tool(t)` or `.dynamic_tools(vec)`.

## Tool servers

Sharing a mutable tool set across async tasks usually means `Arc<Mutex<T>>` (lock contention/deadlocks). Rig's tool server runs tools in a dedicated Tokio task and communicates over message passing. As long as one handle copy lives, the server keeps running:

```rust
use rig::tool::server::{ToolServer, ToolServerHandle};

let tool_server: ToolServerHandle = ToolServer::new()
    .tool(Adder)
    .run();
```

Tool servers accept static, dynamic, and MCP tools. Attach the handle with `AgentBuilder::tool_server_handle(...)`; handing several agents clones of one handle lets them share one tool set.

## Tool output & errors

- **`ToolOutput`** (`rig::tool`) — the canonical model-visible output: one or more typed `ToolResultContent` blocks. `ToolOutput::text(...)`, `ToolOutput::json(...)`, `ToolOutput::content(vec)`, `.as_text()`, `.render()`. Return it directly as `type Output = ToolOutput`, or let any owned serializable value flow through `IntoToolOutput`.
- **`ToolExecutionError`** (`rig::tool`) — the normalized runtime error. Construct with `ToolExecutionError::new(kind, message)` where `kind: ToolErrorKind` (`InvalidArgs`/`Timeout`/`Cancelled`/`NotFound`/`PermissionDenied`/`RateLimited`/`Provider`/`Network`/`Other`), or `ToolExecutionError::other(msg)` / `ToolExecutionError::from_error(e)`. Attach model-visible output with `.with_model_output(...)` and hide secrets with `.redact_model_feedback()`.

## MCP tools

Tools don't have to live in your crate. The [Model Context Protocol](https://modelcontextprotocol.io) lets an agent consume tools served by external processes (filesystem, browsers, databases, SaaS). Rig connects to MCP servers via the `rmcp` crate and exposes their tools with `AgentBuilder::rmcp_tool(...)` / `rmcp_tools(...)`, side by side with native tools. See https://rig.rs/docs/integrations/model_context_protocol.

## Tool organization

Tools are collected in a `ToolSet`, which registers tools, looks them up by name, routes calls, and (for `ToolEmbedding` tools) produces embeddings for dynamic retrieval. When you attach tools to an agent, Rig converts each `ToolDefinition` into the provider's format, parses model output into tool calls, executes them, and returns results to the model.