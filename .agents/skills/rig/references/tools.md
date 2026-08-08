# Tools

Tools let an agent do more than generate text: they expose your Rust functions to the model so it can fetch data, run computations, or reach external systems. When the model decides a tool is needed, Rig parses the call, runs your code, feeds the result back, and continues the loop.

Official docs: https://rig.rs/docs/concepts/tools · API: https://docs.rs/rig/latest/rig/tool/index.html

## Complete example

```rust
use rig::{
    client::{CompletionClient, ProviderClient},
    completion::{Prompt, ToolDefinition},
    providers::openai,
    tool::Tool,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Deserialize)]
struct OperationArgs { x: i32, y: i32 }

#[derive(Debug)]
struct MathError;
impl std::fmt::Display for MathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "math error") }
}
impl std::error::Error for MathError {}

// Hand-written against the `Tool` trait.
#[derive(Deserialize, Serialize)]
struct Adder;
impl Tool for Adder {
    const NAME: &'static str = "add";
    type Error = MathError;
    type Args = OperationArgs;
    type Output = i32;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: "add".to_string(),
            description: "Add x and y together".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "x": { "type": "number", "description": "First number to add" },
                    "y": { "type": "number", "description": "Second number to add" }
                },
                "required": ["x", "y"]
            }),
        }
    }
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        Ok(args.x + args.y)
    }
}

// From a plain function via the macro (type name is PascalCase: subtract -> Subtract).
#[rig::tool_macro(description = "Subtract y from x", required(x, y))]
async fn subtract(x: i32, y: i32) -> Result<i32, rig::tool::ToolError> {
    Ok(x - y)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = openai::Client::from_env()?;
    let calculator = openai
        .agent("gpt-5.5")
        .preamble("You are a calculator. Use the provided tools.")
        .max_tokens(1024)
        .tool(Adder)
        .tool(Subtract)
        .build();
    let answer = calculator.prompt("What is 5 - 2?").await?;
    println!("{answer}");
    Ok(())
}
```

> A prompt that triggers **several** tool calls in sequence needs turn budget — add `.max_turns(n)` or the run fails with `MaxTurnsError`.

## The `Tool` trait

```rust
pub trait Tool: Send + Sync {
    const NAME: &'static str;
    type Args: DeserializeOwned + Send + Sync;
    type Output: Serialize + Send + Sync;
    type Error: Error + Send + Sync;
    async fn definition(&self, prompt: String) -> ToolDefinition;
    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error>;
}
```

- `const NAME` — unique id the model uses to reference the tool.
- `Args` — `Deserialize` type the model's JSON arguments parse into.
- `Output` — what your tool returns on success (serialized and sent back).
- `Error` — your error type, returned when a call fails.
- `definition(&self, prompt)` — returns a `ToolDefinition { name, description, parameters }` sent to the provider. `parameters` is a JSON-schema object.
- `call(&self, args)` — the execution logic.

Keep `parameters` descriptions clear — that text is the model's interface.

> **OpenAI Responses API requires every input parameter under `required`.** Include a `"required"` array, or use `schemars::JsonSchema` (non-`Option` fields are required), or the macro's `required(...)`.

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

async fn definition(&self, _prompt: String) -> ToolDefinition {
    let parameters = schemars::schema_for!(OperationArgs);
    ToolDefinition {
        name: "add".to_string(),
        description: "Add x and y together".to_string(),
        parameters: serde_json::to_value(parameters).unwrap(),
    }
}
```

Migration from v0.8: descriptions move from `#[schemars(description = "...")]` to `///` doc comments; `schema_for!` becomes `schemars::schema_for!` (or `T::json_schema()`); pin `schemars = "1"`.

## The `tool_macro` / `rig_tool`

For simple tools, `#[rig::tool_macro(...)]` (or `#[rig::rig_tool]`) turns a plain function into a tool type (named in PascalCase):

```rust
#[rig::tool_macro(description = "Basic arithmetic", required(x, y, operation))]
async fn calculator(x: i32, y: i32, operation: String) -> Result<i32, rig::tool::ToolError> {
    match operation.as_str() {
        "add" => Ok(x + y),
        "subtract" => Ok(x - y),
        "divide" if y == 0 => Err(rig::tool::ToolError::ToolCallError("Division by zero".into())),
        "divide" => Ok(x / y),
        _ => Err(rig::tool::ToolError::ToolCallError(format!("Unknown op: {operation}").into())),
    }
}
```

The macro derives the argument struct, the JSON schema, and the `Tool` impl. The generated type is passed to `.tool(...)` like any other. Requires the `derive` feature on `rig`.

## When a tool call fails

`call` returning `Err` does **not** abort the prompt. Rig converts the error to its string form and sends it back to the model as the tool result; the loop continues.

- **Make error messages instructive** — the model is the audience. `"Division by zero"` lets it recover; `"error 500"` doesn't.
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

Embed tools into a `VectorStoreIndex`, attach with `.dynamic_tools(n, index, toolset)`:

```rust
use rig::tool::ToolSet;

let toolset = ToolSet::builder().dynamic_tool(Adder).build();
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
    .dynamic_tools(2, index, toolset)
    .build();
```

At each turn the agent fetches the `n` most relevant tools and offers only those; called tools are executed from the toolset.

## Tool servers

Sharing a mutable tool set across async tasks usually means `Arc<Mutex<T>>` (lock contention/deadlocks). Rig's tool server runs tools in a dedicated Tokio task and communicates over message passing. As long as one handle copy lives, the server keeps running:

```rust
use rig::tool::server::{ToolServer, ToolServerHandle};

let tool_server: ToolServerHandle = ToolServer::new()
    .tool(Adder)
    .run();
```

Tool servers accept static, dynamic, and MCP tools. Attach the handle with `AgentBuilder::tool_server_handle(...)`; handing several agents clones of one handle lets them share one tool set.

## MCP tools

Tools don't have to live in your crate. The [Model Context Protocol](https://modelcontextprotocol.io) lets an agent consume tools served by external processes (filesystem, browsers, databases, SaaS). Rig connects to MCP servers via the `rmcp` crate and exposes their tools with `AgentBuilder::rmcp_tool(...)` / `rmcp_tools(...)`, side by side with native tools. See https://rig.rs/docs/integrations/model_context_protocol.

## Tool organization

Tools are collected in a `ToolSet`, which registers tools, looks them up by name, routes calls, and (for `ToolEmbedding` tools) produces embeddings for dynamic retrieval. When you attach tools to an agent, Rig converts each `ToolDefinition` into the provider's format, parses model output into tool calls, executes them, and returns results to the model.