# Tools

Tools let an agent do more than generate text: they expose your Rust functions to the model so it can fetch data, run computations, or reach external systems. When the model decides a tool is needed, Rig parses the call, runs your code, feeds the result back, and continues the loop.

Official docs: https://rig.rs/docs/concepts/tools · API: https://docs.rs/rig-core/latest/rig_core/tool/index.html (0.44: the contracts live in `rig-core-0.45.0/src/tool/`, the runtime registration in `rig-agent-0.45.0/src/tool/`)

## Complete example (0.44)

Two authoring surfaces, both erased into `ErasedTool` handlers:

- **`PortableTool`** (`rig_core::tool::portable`) — context-free: `call(&self, args)`. What `#[rig_tool]` generates for a function with no context argument.
- **`Tool`** (`rig_core::tool::contextual`) — contextual: `call(&self, context: &mut ToolContext, args)`. Every `PortableTool` gets a `Tool` impl by blanket (`contextual.rs:79`), so a portable tool works everywhere the runtime needs a contextual one.

`#[rig_tool]` (the attribute macro, re-exported by both `rig_core` and `rig_agent` under the default `derive` feature) derives the impl from a function:

```rust
use rig_agent::AgentBuilder;
use rig_core::providers::openai::{self, OpenAI};

// The macro generates a PascalCase type (`subtract` -> `Subtract`) plus an
// uppercase static of that type (`SUBTRACT`) beside the function.
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
        .tool(Subtract)                  // or the generated `SUBTRACT` static
        .build();
    let answer = calculator.prompt("What is 5 - 2?").await?;
    println!("{}", answer.output());     // `output` is a method on PromptResponse
    Ok(())
}
```

> A prompt that triggers **several** tool calls in sequence needs turn budget — add `.max_turns(n)` or the run fails with `PromptError::MaxTurns { max_turns, chat_history, prompt }`.

## The `Tool` / `PortableTool` traits

```rust
// Contextual — rig_core::tool::Tool
pub trait Tool: Sized + Send + Sync {
    const NAME: &'static str;
    type Args: for<'de> Deserialize<'de> + Send + Sync;
    type Output: IntoToolOutput;                   // no Send bound here
    type Error: Error + Send + Sync + 'static;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;     // JSON Schema
    fn map_error(&self, error: Self::Error) -> ToolExecutionError; // default: from_error
    fn call(&self, context: &mut ToolContext, args: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}

// Context-free — rig_core::tool::PortableTool
pub trait PortableTool: Sized + Send + Sync {
    const NAME: &'static str;
    type Args: for<'de> Deserialize<'de> + Send + Sync;
    type Output: IntoToolOutput + Send;            // portable outputs must be Send
    type Error: Error + Send + Sync + 'static;
    fn description(&self) -> String;
    fn parameters(&self) -> serde_json::Value;
    fn map_error(&self, error: Self::Error) -> ToolExecutionError;  // default: from_error
    fn call(&self, arguments: Self::Args)
        -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}
```

(Those bounds are `WasmCompatSend`/`WasmCompatSync` in the source: relaxed on browser WASM, `Send + Sync` everywhere else.)

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

- `const NAME` — unique id the model uses to reference the tool. `tool_name::<T>() -> ToolName` (`contextual.rs:443`) turns it into a typed name with a compile-time non-empty assertion; `tool_definition(&tool) -> ToolDefinition` (`contextual.rs:449`) builds the provider-facing definition from a `Tool`.
- `Args` — `Deserialize` type the model's JSON arguments parse into (raw-JSON tools use `type Args = serde_json::Value`). Arguments are parsed once at the erased boundary: a literal `null` parses as `{}`, and a parse failure becomes `ToolErrorKind::InvalidArgs` with the parse error kept as the source.
- `Output` — what your tool returns on success. `IntoToolOutput` is implemented for every owned serializable value (as text) and for `ToolResultContent`/`Vec<ToolResultContent>`/`ToolOutput` (preserving rich content).
- `Error` — your error type; Rig normalizes it into `ToolExecutionError` only at the erased dispatch boundary, via `map_error` (default `from_error`, which redacts into kind-level model feedback).
- `description()` + `parameters()` — the provider-facing tool definition.
- `call(&mut ToolContext, args)` — the execution logic. Read caller-provided runtime values via `context.require::<T>()` (see `ToolContext` below); portable tools get no context by design (identity, authorization, capability state and lifecycle stay outside that boundary).
- **`ErasedTool`** (`contextual.rs:197`) — the object-safe impl-side contract the bus's `ToolAdapter` calls: `name() -> String`, `description()`, `parameters()`, `execute(args: String, context: &mut ToolContext) -> WasmBoxedFuture<ToolResult>`. Yours is derived from the `Tool` impl; you never implement it.
- **`ToolEmbedding`** / **`PortableToolEmbedding`** — the retrieval pair (see "Tool-RAG" below); `ToolEmbedding` is blanket-implemented for every `PortableToolEmbedding` (`contextual.rs:126`).

## `ToolContext`

`ToolContext` is a mutable, run-scoped value store (inbound values + published result metadata + live driver scopes) passed to every contextual `Tool::call`. Values are **`ContextValue`s**: serde-backed types with a stable slot `KEY`.

```rust
use rig_core::tool::{ContextValue, ToolContext};

// A typed context value (KEY defaults to the type name; a bare String/int cannot be stored):
#[derive(serde::Serialize, serde::Deserialize, rig_core::ContextValue)]
#[context(key = "api.token")]   // optional override
struct ApiToken(String);

// Author attaches it to the run:
let ctx = {
    let mut c = ToolContext::new();          // `const fn`, infallible
    let _ = c.insert(ApiToken("t".into()));  // Result<Option<T>, ToolContextError>
    c
};
let response = agent
    .prompt("...")
    .tool_context(ctx)                       // AgentRunner::tool_context(ToolContext)
    .run().await?;

// Tool reads it:
async fn call(&self, ctx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, Self::Error> {
    let token: ApiToken = ctx.require()?;    // Err(Missing(key)) / Err(Decode{..}) on absence/shape mismatch
    // ...
    let _ = ctx.insert_result(SomethingPublished { .. }); // host-only result metadata
}
```

- **Inbound namespace**: `insert<T>(value) -> Result<Option<T>, ToolContextError>`, `get<T>() -> Result<Option<T>, _>`, `require<T>() -> Result<T, _>`, `remove<T>()`, `contains<T>() -> bool`.
- **Result namespace**: `insert_result<T>(value)`, `result<T>()`, `require_result<T>()`. Neither map is model-visible; `insert_result` values survive as `ToolResultContext` snapshots and are read after the call by outcome hooks via `OutcomeEvent::tool_context()` (shuvarie's `FileChange`/`ShellStreams`/`SpawnTag`).
- `is_empty()` — both maps empty. Slots are keyed by `ContextValue::KEY`, so accessors take no key argument and two types sharing a key are one slot. Every value is **data**: insert serializes it, reading deserializes it, and `ToolContext` itself is `Serialize + Deserialize`, so an explicitly serialized scene can contain sensitive inbound values (protect it host-side).
- **Dispatch isolation**: `for_dispatch()` clones the inbound values, drops result metadata and keeps the scopes — runtimes call it per tool call, so map-level mutations inside a call never leak into the caller's context. `accept_dispatch_result(dispatched)` copies a dispatch's result metadata back; `result_context() -> ToolResultContext` snapshots only what was published; `with_result_context(..)` restores durable output while keeping this dispatch's inbound values; `clear_dispatch_result()` drops the previous dispatch's metadata.
- **Live scopes**: `with_scope(Arc<dyn Any + Send + Sync>)` / `with_scopes(Vec<_>)` attach the driver's per-call state, `scope::<T>() -> Option<Arc<T>>` reads it, `clear_scope()` drops it. Scopes are excluded from serialization and equality: they belong to the call, not to the data.
- **`ToolResultContext`** — the durable, explicitly published subset, `get::<T>() -> Result<Option<T>, ToolContextError>` with no inbound credentials and no scopes. Effect logs record only this; applications own their schema versions.
- **`ToolContextError`** — `Missing(&'static str)` / `Encode { key, source }` / `Decode { key, source }` (absence and a shape mismatch are different facts). `From<ToolContextError> for ToolExecutionError` maps it to `Other` with the error as source.
- **`PublishedContext`** (`rig_core::tool::PublishedContext`) — the per-dispatch recorder the runtime threads between driver and sink: `PublishedContext::new() -> Arc<Self>`, `publish(ToolContext)` (clears scopes), `result_context() -> Option<ToolResultContext>`, `take() -> Option<ToolContext>`. Publication must finish before the sink resolves.
- **`TypeMap`** (`rig_core::tool::context::TypeMap`) — a clone-on-dispatch `TypeId`-keyed map (`insert`/`get`/`get_mut`/`remove`/`contains`/`len`/`is_empty`) for in-process per-run state that never crosses a wire. It is **not** `ToolContext`'s storage (which is serde data); rig-agent's hook `Scratchpad` wraps one.

> OpenAI Responses API requires every input parameter under `required`. Include a `"required"` array, or use `schemars::JsonSchema` (non-`Option` fields are required), or the macro's `required(...)`.

## Deriving the schema with `schemars`

**v1.0 required.** `rig-core` re-exports it as `rig_core::schemars` (with `rig_core::serde` and `rig_core::serde_json`) so generated code resolves them without a direct dependency. Derive `schemars::JsonSchema` and describe each field with a `///` doc comment:

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

- Field descriptions come from `///` doc comments; `#[schemars(description = "...")]` also works and is what the `rig_tool` macro emits for `params(...)`.
- **`ToolDefinition`** (`rig_core::completion`) is `{ name: ToolName, description: String, parameters: serde_json::Value }`, with `ToolDefinition::new(name, description, parameters)`. **`ProviderToolDefinition { kind, config }`** is the provider-hosted form (e.g. `web_search`) that wires forward from `additional_params.tools`.
- **Names are typed**: `ToolName(String)` rejects the empty name (`EmptyToolName`) and derefs to `str`; `ToolChoice::Specific { function_names: Vec<ToolName> }` (`message.rs:1299`) is how a caller pins a named subset. Build one with `ToolName::new(..)?`, or `tool_name::<T>()` for a `Tool` impl's `NAME`.

## The `rig_tool` macro

For simple tools, `#[rig_tool(...)]` turns a plain function into a tool: it derives a `{PascalCase}Parameters` argument struct (`Deserialize` + `schemars::JsonSchema`), keeps the function, and emits a PascalCase unit struct plus an uppercase static of that type. A function with no context parameter implements `PortableTool`; one that takes `&mut ToolContext` implements `Tool` with the context as the first `call` argument.

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

- `#[rig_tool(...)]` is available on both `rig_core` and `rig_agent` (feature `derive`, on by default); `rig`-qualified paths are recognized directly.
- **`name`** overrides the tool name — 1..=64 bytes, first char an ASCII letter or `_`, the rest ASCII letters/digits/`_`/`-`. **`description`** overrides the doc comment (otherwise the function's `///` docs, else `Function to {name}`). **`params(name = "description")`** describes individual fields. Unknown or duplicate options are rejected, as are names matching no parameter of the function.
- **Requiredness**: non-`Option` arguments are required by default. Passing `required(...)` makes every unlisted argument optional in both the schema and the deserializer (`#[serde(default)]`, so those types must implement `Default`), and listing an `Option` argument is rejected. Either way `parameters()` inserts a `required` array, empty when nothing is required.
- **The context argument** is detected as the last-qualified `ToolContext` type or marked by hand: an *imported* alias needs `#[rig(context)] context: &mut MyContext`, since a macro cannot resolve imported names. The parameter is host-only: it is excluded from the generated argument struct and the JSON schema, cannot be listed in `params(...)`/`required(...)`, must be `&mut`, and at most one is allowed.
- The macro's error type is whatever the function returns: `rig_core::tool::ToolExecutionError` above (the `InvalidArgs` constructor keeps "Division by zero" model-visible).

## When a tool call fails

`call` returning `Err` does **not** abort the prompt. The erased boundary maps the error with `map_error` (default `from_error`) and shapes it into `ToolResult::failed(..)`; the model-visible output of that result is what goes back, and the loop continues. Tool and MCP failures are no longer flattened into strings — dispatch, hooks and telemetry see the typed `ToolResult` / `ToolExecutionError` / `ToolErrorKind`.

- **`from_error` keeps operators informed and the model safe** — an arbitrary source's display string stays in `.message()` and is downcastable with `.downcast_ref::<E>()` / `.is::<E>()`, while the model sees stable kind-level feedback. An error that is already a `ToolExecutionError` passes through with its classification and presentation.
- **Explicit constructors are model-visible by default** — `invalid_args`, `timeout`, `cancelled`, `not_found`, `permission_denied`, `rate_limited`, `provider`, `network`, `other`, or `ToolExecutionError::new(kind, message)`. **Make error messages instructive** — the model is the audience. `"Division by zero"` lets it recover; `"error 500"` doesn't. Replace the presentation with `.with_model_feedback(..)` / `.with_model_output(ToolOutput)` for diagnostics that must not reach the model, and attach real detail with `.with_source(..)`.
- **`ToolErrorKind`** — nine normalized kinds, each with a stable name, a default retryability and default model feedback (`result.rs:69`); override with `.with_retryable(bool)`. `ToolExecutionError` also carries `.code()` and `.with_http_status(..)`.

  | kind | `as_str()` | `default_retryable()` | default model feedback |
  | --- | --- | --- | --- |
  | `InvalidArgs` | `invalid_args` | `Some(false)` | tool arguments were invalid |
  | `Timeout` | `timeout` | `Some(true)` | tool execution timed out |
  | `Cancelled` | `cancelled` | `Some(false)` | tool execution was cancelled |
  | `NotFound` | `not_found` | `Some(false)` | the requested tool or resource was not found |
  | `PermissionDenied` | `permission_denied` | `Some(false)` | the tool denied the request |
  | `RateLimited` | `rate_limited` | `Some(true)` | the tool was rate limited; try again later |
  | `Provider` | `provider` | `None` | the tool provider failed |
  | `Network` | `network` | `Some(true)` | the tool could not reach its upstream service |
  | `Other` | `other` | `None` | the tool failed |

  `None` means the tool must supply the verdict; without an override it becomes non-retryable in error reports.
- **A refusal is a disposition, not an error**: `ToolExecutionError::refused(msg)` uses the normalized `PermissionDenied` kind but `ToolResult::failed` stores it as `Refused` (`is_refused()`, `refusal()`, telemetry status `denied`). A `PermissionDenied` failure stays `Error`.
- **Budget turns for recovery** — a retry costs a turn; use `.max_turns(...)`.
- **An unresolvable call** fails the run: the default dispatch answers with `ToolExecutionError::not_found(..).with_model_feedback(..)` ("tool `<name>` not found"), and a call that no advertised tool can answer surfaces as `PromptError::UnknownToolCall { tool_name, available_tools, allowed_tools, chat_history }`. Malformed arguments reach the invalid-call hook, and their recovery is bounded by `AgentRunner::max_invalid_tool_call_retries(..)`, `max_consecutive_malformed_tool_calls(..)` and `unhandled_invalid_tool_call(..)` (see `hooks.md`).
- **Hook-steered results** — a hook can deny, patch or skip a call at the `on_dispatch` boundary (`DispatchAction::{deny, skip, stop, rewrite_tool_args, patch}`) and rewrite a tool result's model-visible output at `on_outcome` (`OutcomeAction::rewrite_tool_result(event, text)` / `rewrite_tool_output(event, ToolOutput)`).

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

The builder is typestate over the tool source: `NoToolConfig` → `WithBuilderTools` once you add a tool through the builder; a pre-existing shared registry is `WithToolServerHandle`, set with `.tool_server_handle(handle)` (only from `NoToolConfig`). Build with zero tools via plain `.build()`.

- Builder adders: `.tool(t)` (any `Tool`; portable tools qualify through the blanket impl), `.dynamic_tool(t)` / `.dynamic_tools(vec)`, `.retrieved_tools(sample, index, toolset)`, `.retrieved_tools_handler(sample, handler, toolset)`.
- Agent side: `Agent::tool_server_handle() -> &ToolServerHandle` (clone it to register tools mid-run — the agent sees them on its next turn) and `Agent::tool_definitions(prompt: Option<String>) -> Result<Vec<ToolDefinition>, ToolServerError>`, a read-only view of what a prompt would advertise.
- A `ToolSet` merges into a builder-owned server via `.add_tools(toolset)` in the tool server, or is attached wholesale as retrieval-only with `ToolSet::add_retrievable_tools`.

**Runtime-defined tools** — see "Dynamic tools" below. Attach with `.dynamic_tool(t)` / `.dynamic_tools(vec)`.

## Tool-RAG with `ToolEmbedding`

Implement `PortableToolEmbedding` (context-free) or `ToolEmbedding` (contextual) in addition to the tool trait to make a tool retrievable. Each provides the text to embed (`embedding_docs`) plus the state/context to reconstruct the tool:

```rust
use rig_core::tool::PortableToolEmbedding;

impl PortableToolEmbedding for Adder {
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

Embed the tool definitions into a `VectorStoreIndex`, attach the toolset, and the agent fetches the `sample` most relevant tools per turn:

```rust
use rig_agent::tool::ToolSet;
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

let mut toolset = ToolSet::default();
toolset.add_retrieved_tool(Adder)?;        // Result<String, serde_json::Error>: registers as retrievable
let schemas = toolset.schemas()?;         // Vec<ToolSchema>: name + serialized context + embedding docs
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

- **`ToolSchema`** (`rig_core::embeddings::tool::ToolSchema`, re-exported at `rig_core::embeddings::ToolSchema`) is the embeddable form of a tool definition: `{ name: String, context: serde_json::Value, embedding_docs: Vec<String> }`, implementing `Embed` over `embedding_docs`. `ToolSet::schemas()` (`registry.rs:499`) collects it for the retrievable registrations; `ToolSet::documents()` is the whole-set alternative.
- `.retrieved_tools(sample, index, toolset)` needs `I: VectorStoreIndex<Filter = F>` with `F: DynamicSearchFilter + Send + Sync`; the engine dispatches the index's `TopNIds` query per tool-selection turn and executes the called tools from the registrations that turn pinned. `retrieved_tools_handler(sample, handler, toolset)` is the same with any retrieval-family handler (a replayer answering a recorded index).
- **`AgentBuilder::dynamic_context(samples, index)`** is the sibling for *document* RAG: it retrieves `samples` documents for every prompt and patches them into the request as extra context (`dynamic_context_handler(..)` for a handler). Tools and context are independent — both are ordinary builder methods.

## Dynamic tools

For a tool whose name/description/callback is only known at runtime, build a `DynamicTool` directly. `new` is context-free (args only) and `new_with_context` carries the `&mut ToolContext`; `.with_liveness(..)` supports registry retirement. Both take the name as a `ToolName`:

```rust
use std::sync::Arc;

use rig_agent::tool::DynamicTool;
use rig_core::message::ToolName;
use rig_core::tool::{Tool, tool_definition};
use serde_json::json;

// Context-free callback:
let tool = DynamicTool::new(
    ToolName::new("echo")?,
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
let typed = Arc::new(Adder);
let definition = tool_definition(&*typed);          // ToolDefinition
let tool = DynamicTool::new_with_context(
    definition.name, definition.description, definition.parameters,
    move |ctx, args| {
        let typed = Arc::clone(&typed);
        Box::pin(async move { typed.call(ctx, args).await })
    },
);
```

- Accessors: `name() -> &ToolName`, `definition() -> ToolDefinition`, `handler() -> &ErasedHandler`, `into_parts() -> (ToolDefinition, ErasedHandler, Option<LivenessFn>)`.
- Execution (**renamed in 0.45**): `run(arguments) -> Result<ToolOutput, ToolExecutionError>` on an empty context, `run_with(&mut ToolContext, arguments)` on isolated inbound values (0.44's `execute_with`) — a completed call replaces only the caller's result metadata (including when the tool failed); dropping the future leaves the caller's context unchanged and publishes nothing partial.
- `is_live()` — whether the tool's owner still serves it (`true` when no probe is attached); registries check it lazily on reads/reconciliation, and a call that races retirement may fail as a transport error rather than `HandlerUnavailable`.
- The name is typed, so a dynamic tool cannot be built with an empty name.
- Attach with `.dynamic_tool(t)` / `.dynamic_tools(vec)`.
- **An agent is a tool**: `agent.into_tool() -> Result<DynamicTool, EmptyToolName>` (`agent/tool.rs:37`) wraps the whole agent as a sub-agent tool that takes a JSON `prompt` string and inherits the dispatch context; `DynamicTool::try_from(agent)` is the same conversion. The empty name is the only failure (the configured agent name is used, else `agent_tool`).

## Tool servers

Sharing a mutable tool set across async tasks usually means `Arc<Mutex<T>>` (lock contention/deadlocks). Rig's tool server runs the registrations behind a handle and communicates over message passing. As long as one handle copy lives, the server keeps serving; a replaced registration keeps its old key alive until the catalogs pinning it are dropped (a generation lease):

```rust
use rig_agent::tool::server::{ToolServer, ToolServerHandle};

let tool_server: ToolServerHandle = ToolServer::new()
    .owner("assistant")          // optional: the owner segment of every key it mints
    .tool(Adder)                 // + .dynamic_tool(s) / .registered_tool / .retrieved_tools(sample, index, toolset)
    .run();                      // no executor spawned: the bus drives registrations
```

Attach the handle with `AgentBuilder::tool_server_handle(..)`; handing several agents clones of one handle lets them share one tool set, and external sources (an MCP client handler, for example) can register/refresh tools mid-run through `agent.tool_server_handle()`, which the agent sees on its next turn.

`ToolServerHandle` is a cheap clone over an `RwLock`:

- **Writes**: `add_tool(t)`, `add_dynamic_tool(t)`, `add_registered_tool(r)`, `add_tools(toolset)`, `remove_tool(name)` — plus `add_managed_tools(..)` / `reconcile_managed_tools(..)` from the `ManagedToolSink` impl below. Each one registers under a fresh generation key (an explicitly pinned key is served verbatim), retires the name's previous generation and republishes to every attached bus.
- **Sync, retrieval-free reads** (usable outside a run): `snapshot() -> ToolCatalog`, `static_tool_defs() -> Vec<ToolDefinition>`, `toolset() -> ToolSet` (a fork), `owner()`, `retrieval_keys() -> Vec<HandlerKey>`. These retire disconnected tools first, then read.
- **Retrieval-aware reads**: `tool_defs(prompt) -> Result<Vec<ToolDefinition>, ToolServerError>` answers the registered retrieval indexes inline on the registry's own handlers, while a run instead dispatches `retrieval_effects(prompt)` and then pins the returned ids with `snapshot_with_dynamic(&ids)`.
- **Direct execution**: `execute(name, args, &mut ToolContext) -> ToolResult`, publishing the dispatch's result metadata into the context. A run's engine does the same through the bus.
- `attach(&Registrar)` publishes every registration onto a bus, now and as they change.
- **Managed sources**: `ManagedToolSink { add_managed_tools(Vec<DynamicTool>) -> HashMap<String, ManagedToolToken>, reconcile_managed_tools(expected: HashMap<String, ManagedToolToken>, Vec<DynamicTool>) -> HashMap<String, ManagedToolToken> }` (`rig_core::tool::managed`) is the refresh contract an external, reconnectable tool source implements against. Tools reporting `!is_live()` are skipped; a newer same-name local or peer registration is left intact; names the source still owns and no longer lists are removed. `ManagedToolToken` is an opaque per-generation identity (equal only to itself).

## Tool output & errors

- **`ToolOutput`** (`rig_core::tool`) — the canonical model-visible output: one or more typed `ToolResultContent` blocks. `ToolOutput::text(...)`, `ToolOutput::json(...)`, `ToolOutput::content(vec)?` / `ToolOutput::one(..)`, `.as_text()`, `.as_json()`, `.as_content()`, `.into_content()`, `.render()` (stable text for telemetry). Return it directly as `type Output = ToolOutput`, or let any owned serializable value flow through `IntoToolOutput`. Rig never reparses text as JSON to guess whether it is rich content.
- **`IntoToolOutput`** — blanket-implemented for every `Serialize + 'static` value: a value that serializes to a JSON string becomes a literal text block, everything else becomes a structured JSON block; `ToolOutput`, `ToolResultContent` and `Vec<ToolResultContent>` are detected first and preserved as rich content (an empty `Vec` is an error). Implement the trait directly only for output types that do not implement `Serialize`.
- **`ToolResultContent`** — `Text(Text)` / `Image(Image)` / `Json { value }` (`message.rs:373`), with normalized per-kind constructors `ToolResultContent::{text, json, image_base64, image_raw, image_url}` and `as_text()` / `as_json()` / `deserialize_json::<T>()`. The same spelling is used across the media constructors (`UserContent::{image_*, audio_*, video_*, document_*}`, `AssistantContent::{text, image_base64, tool_call, reasoning}`).
- **`ToolResult`** (`rig_core::tool`) — the runtime execution record: `ToolResult::success(output)` / `failed(error)` / `skipped(reason)`, `.with_output(..)`, `.output()`, `.error()` / `.refusal()`, `.is_success()` / `.is_error()` / `.is_skipped()` / `.is_refused()` / `.is_error_kind(kind)`, `.into_result()`, `.status_name()` (`success` / `error` / `denied` / `skipped`). This is what the bus dispatches and what hooks observe; the *message-layer* `rig_core::message::ToolResult { call, name, content, is_error }` (the model-visible reply, built from the call with `ToolCall::result` / `ToolCall::error_result`) is separate — see `completions.md`.
- **`ToolExecutionError`** (`rig_core::tool`) — the normalized runtime error: `ToolExecutionError::new(kind, message)`, the per-kind constructors, `from_error(e)`, or `refused(msg)`; enrich with `.with_model_feedback(..)` / `.with_model_output(..)` / `.with_retryable(..)` / `.with_code(..)` / `.with_http_status(..)` / `.with_source(..)`; inspect with `.kind()`, `.message()`, `.model_feedback()`, `.model_output()`, `.retryable()`, `.code()`, `.http_status()`, `.is_refusal()`, `.downcast_ref::<E>()`. It is `Send + Sync + Serialize` and crosses the bus (a deserialized error carries no source).
- **`ToolErrorKind`** — see the kind table above for `as_str()`, `default_retryable()` and default model feedback.

## MCP tools

Tools don't have to live in your crate. The [Model Context Protocol](https://modelcontextprotocol.io) lets an agent consume tools served by external processes (filesystem, browsers, databases, SaaS). MCP support is **not** part of `rig-agent`: it ships as the separate, native-only **`rig-rmcp`** companion crate (see https://rig.rs/docs/integrations/model_context_protocol). Wire its tools through the surfaces above: an MCP client handler registers and refreshes tools on `agent.tool_server_handle()`, using `DynamicTool::with_liveness(..)` so a disconnected server's tools report `!is_live()` (skipped on registration, retired on the next registry read) and the `ManagedToolSink` / `ManagedToolToken` pair for generation-safe refreshes.

## Tool organization

Tools are collected in a `ToolSet` (`rig_agent::tool::ToolSet`), an insertion-ordered registry: `add_tool` / `add_dynamic_tool` / `add_retrieved_tool` / `add_registered_tool` (`from_tools`, `add_tools`, `add_retrievable_tools`, `remove_tool` — **`from_dynamic_tools` is gone in 0.45**: loop `add_dynamic_tool` over a `ToolSet::default()`), reads by name (`get`, `contains`, `iter`, `names`, `len`, `is_empty`, `always_exposed_names`), `tool_definitions()` (**returns `Vec<Arc<ToolDefinition>>` in 0.45**), `catalog()`, `documents()`, `schemas()`, and inline `execute(name, args, &mut ToolContext)`. Each entry is a `RegisteredTool` — definition, optional embedding descriptor, handler `key`, erased handler, optional liveness probe — with `name()`, `key()`, `descriptor()`, `embedding()`, `is_live()`, `with_key(..)` and `execute(args, &mut ToolContext)`. `execute_tool(name, args, Option<RegisteredTool>, &ToolContext) -> ToolDispatch { result, context }` is the free dispatch function, and `ToolDispatch::publish_to(&mut ToolContext)` folds a dispatch's published metadata back into the caller's context. Malformed-argument feedback lives in `rig_core::tool::feedback` in 0.45 (`invalid_arguments_feedback(name, raw)`, 0.44's `rig_core::transcript`).

On the runtime side, `ToolCatalog` (`rig_agent::tool::ToolCatalog`) pins the registrations one request advertises — `definitions()`, `names()`, `iter()`, `get()`, `contains()`, `key()`, `take_definitions()`, inline `execute` / `execute_scoped` — and holds a `ToolLease` (an opaque `Arc`) per generation so a registry cannot retire a handler a pinned request may still need. When you attach tools to an agent, Rig converts each `ToolDefinition` into the provider's format, parses model output into tool calls, executes them from that turn's catalog, and returns the typed results to the model.

## Project boundary (shuvarie)

- `shuvarie-llm` (`crates/llm`) is the only place that names rig's tool surface. `crates/llm/src/tool.rs` re-exports `rig_agent::tool::{DynamicTool, Tool, ToolContext, ToolSet}`, `rig_core::completion::ToolDefinition`, `rig_core::message::ToolName` and `rig_core::tool::{ToolErrorKind, ToolExecutionError, ToolOutput, ToolResult}`; `crates/llm/src/lib.rs` re-exports that module plus `tool_image::ToolResultContentAlias as ToolResultContent`.
- `crates/llm/src/tool.rs::tool_name(..)` builds a `ToolName` from a roster, worker or MCP tool name, and `into_dynamic(name, tool)` wraps a typed `Tool` (Args `serde_json::Value`, Output `ToolOutput`, Error `ToolExecutionError`) with `DynamicTool::new_with_context`, so every dynamic tool here keeps the typed context path.
- Host-only result metadata goes through `ToolContext::insert_result`: `FileChange` and `ShellStreams` (`crates/llm/src/file_change.rs`) and `SpawnTag` (`crates/llm/src/agent.rs`) all derive `rig_core::ContextValue` keyed by type name. `FileChangeHook` reads them at `on_outcome` with `event.tool_context()` + `context.result::<T>()`, and uses `ToolResult::is_error()` for its `ok` verdict instead of sniffing result text.
- Multimodal tool output is built in `crates/llm/src/tool_image.rs` from `ToolResultContent::Text(Text::new(..))` plus `ToolResultContent::Image { data, media_type, detail, native }`; rig forwards those blocks into the provider's tool-result rendering.
- `crates/llm/src/agent.rs` implements `Tool` for `WorkerAgent` (`async fn call(&self, ctx: &mut ToolContext, args)`) and answers with `ToolOutput::text(..)`, minting its run's spawn id into `ctx.insert_result(SpawnTag(..))`.
- The workspace depends on `rig-agent` + `rig-core` directly, never the `rig` facade (`Cargo.toml`: "Do NOT include `rig`. It has too many implicit dependencies."), and MCP is not rig's: `shuvarie-mcp` (`crates/mcp`) is built on the `rmcp` client crate (a workspace dependency), and its tool names enter this layer only through `tool_name(..)`.
