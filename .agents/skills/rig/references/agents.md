# Agents

An `Agent` is Rig's primary building block. It bundles a completion model with a system prompt (`preamble`), optional context documents, tools, and (optionally) conversation memory, then runs the **agent loop** for you: send the prompt, execute any tools the model calls, feed results back, repeat until the model answers.

Official docs: https://rig.rs/docs/concepts/agent · API: https://docs.rs/rig/latest/rig/agent/index.html

## Minimal agent

```rust
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::Prompt;
use rig::providers::openai;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = openai::Client::from_env()?;
    let agent = openai
        .agent("gpt-5.5")
        .preamble("You are a helpful assistant.")
        .temperature(0.7)
        .build();
    let response = agent.prompt("Hello!").await?;
    println!("{response}");
    Ok(())
}
```

`client.agent(model)` returns an `AgentBuilder`; `.preamble(...)`, `.temperature(...)`, `.build()` configure it.

> **In 0.42 `Agent` is non-generic.** `client.agent(model)` erases the typed model into a `ModelHandle` when you call `.build()`. The built `rig::agent::Agent` is a concrete, model-agnostic type — write `Agent` (never `Agent<M>`) in function signatures. You can still call `.with_model(...)` / `.set_model(...)` / `.with_model_handle(...)` on an existing agent to swap its default model.

## What an agent is made of

- **Base configuration** — completion model, system prompt (`preamble`), `temperature`, `max_tokens`, `additional_params`.
- **Context** — documents appended to the request. Static context is always sent; dynamic context is retrieved from a vector store per request.
- **Tools** — capabilities the model can call. Static tools are always offered; dynamic tools are retrieved per request.
- **Conversation memory** *(optional)* — a backend that loads prior history before each prompt and saves the new turn after it. See `memory.md`.

## The agent loop

1. Build a completion request from the preamble, static context, any dynamic context retrieved from a vector store, the conversation history, and every tool's definition.
2. Send it to the model. One request/response round trip is a **turn**.
3. Inspect the response:
   - **text** → loop ends, that text is your result.
   - **tool calls** → Rig executes each (parsing JSON args into your `Args`, running `call`, appending the result as a tool-result message). A model may request several tool calls in one turn; Rig runs them and returns all results together.
4. Repeat from step 2 with the updated history until the model produces text or the turn budget runs out.

### Turns and `max_turns`

Default `max_turns = 0` = initial request + one follow-up after tool execution. If the model keeps chaining tools past the budget the prompt fails with `PromptError::MaxTurnsError` (which carries the accumulated history so you can inspect what happened).

Give headroom with `.max_turns(n)` on the prompt request, or `AgentBuilder::default_max_turns(n)` for every prompt:

```rust
let res = tool_agent
    .prompt("Calculate 2 + 5, then multiply by 3")
    .max_turns(5)
    .await?;
```

### When a tool call goes wrong

- **Your tool returns `Err`** → the error's string form is sent back to the model as the tool result and the loop continues. The model can retry with different args or explain the failure. Make tool errors descriptive.
- **The model emits an invalid call** (unknown/disallowed tool name) → fails the prompt immediately by default. A hook can recover with `InvalidToolCallAction::retry` / `repair` / `skip`; `.max_invalid_tool_call_retries(n)` bounds retry rounds (each retry also consumes turn budget). See `hooks.md`.

## Context

- `AgentBuilder::context(doc)` — static context appended to every request.
- `AgentBuilder::dynamic_context(n, index)` — fetch up to `n` relevant docs from a vector store per request (RAG).

```rust
let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a knowledge base assistant.")
    .dynamic_context(3, index) // top 3 relevant docs per request
    .temperature(0.3)
    .build();
```

See `rag-and-vector-stores.md`.

## Tools

- `AgentBuilder::tool(t)` — static tool, always offered.
- `AgentBuilder::dynamic_tool(t)` / `dynamic_tools(vec)` — runtime-defined tools (`DynamicTool`), offered every turn.
- `AgentBuilder::retrieved_tools(sample, index, toolset)` — retrieve up to `sample` relevant tools per request from a vector store (tool RAG). This is the 0.42 replacement for the old `dynamic_tools(n, index, toolset)`.

```rust
let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a tool-using assistant.")
    .tool(calculator)
    .tool(web_search)
    .retrieved_tools(2, tool_store_index, toolset)
    .build();
```

See `tools.md`.

### Steering tool use with `ToolChoice`

```rust
use rig::message::ToolChoice;

let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a calculator. Always compute with tools.")
    .tool_choice(ToolChoice::Required) // must call a tool before answering
    .build();
```

- `Auto` (default) — model may call tools or answer directly.
- `None` — tools visible but must not be called.
- `Required` — must call at least one tool.
- `Specific { function_names }` — must call one of the named tools.

## Agents as tools (manager-worker)

An agent implements the tool interface, so you can hand one agent to another as a tool. Give each worker a `name` and `description` (both used when the agent is exposed as a tool), then attach with `.tool(...)`:

```rust
let bob = openai.agent("gpt-5.5")
    .name("Bob")
    .description("An employee who handles admin tasks at FooBar Inc.")
    .preamble("You are Bob, an admin employee. Your manager Alice may ask you to do things.")
    .build();

let alice = openai.agent("gpt-5.5")
    .name("Alice")
    .description("A manager at FooBar Inc.")
    .preamble("You are Alice, a manager. You manage Bob.")
    .tool(bob)
    .build();

let res = alice
    .prompt("Ask Bob to draft a welcome email and tell me what he wrote.")
    .max_turns(5)
    .await?;
```

For swarm/actor architectures see https://rig.rs/docs/guides/advanced/multi_agent_systems.

## Conversations and memory

An agent is **stateless**: each `.prompt(...)` starts from scratch unless you supply earlier turns. Three ways, manual → automatic:

**Pass history explicitly** with `.with_history(...)`. You own the `Vec<Message>` and must record each new turn yourself (Rig does NOT append):

```rust
let reply = agent.prompt("What's my name?").with_history(history.iter()).await?;
history.push(Message::user("What's my name?"));
history.push(Message::assistant(&reply));
```

**Use `chat`** for the common text-in/text-out case — it takes `&mut Vec<Message>` and **appends** the new turn for you:

```rust
let response = chat_agent.chat("Hello!", &mut previous_messages).await?;
```

**Attach conversation memory** — with a memory backend + conversation id, Rig loads stored history before each prompt and appends the new turn after:

```rust
use rig::memory::InMemoryConversationMemory;

let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a helpful assistant.")
    .memory(InMemoryConversationMemory::new())
    .build();

let _ = agent.prompt("My name is Ada.").conversation("user-42").await?;
let reply = agent.prompt("What's my name?").conversation("user-42").await?;
```

See `memory.md` for bypass rules (`with_history` skips memory for that request; `.without_memory()` disables it) and durable backends.

## Token usage & run details

`.prompt(...)` returns just the answer text. Add `.extended_details()` for a `PromptResponse` with aggregate usage, per-call usage, and full history:

```rust
let response = agent
    .prompt("What is 2 + 2?")
    .max_turns(3)
    .extended_details()
    .await?;

println!("answer: {}", response.output);
println!("tokens: {} in / {} out across {} requests",
    response.usage.input_tokens,
    response.usage.output_tokens,
    response.requests(),
);
```

`usage` aggregates across every turn; `completion_calls` breaks down per model request; `messages` holds the full run history. Zero-valued usage means the provider didn't report.

## Additional parameters

Provider-specific knobs Rig doesn't model directly (reasoning settings, etc.) pass through with `additional_params` and are merged into the completion request:

```rust
let agent = openai_client
    .agent("gpt-5.5")
    .preamble("You are a helpful agent.")
    .additional_params(serde_json::json!({ "foo": "bar" }))
    .build();
```

## AgentRunner and Hooks

For per-run controls (turn limits, tool concurrency, tool extensions, hooks) use `agent.runner(prompt)...run()` — see `agent-runner.md`. For observing/steering the loop (audit, guardrails, approvals, invalid-tool recovery) use hooks — see `hooks.md`.

## Agent or hand-written workflow?

An agent lets the *model* decide the control flow — right for open-ended tasks. When you already know the steps, plain Rust calling agents in sequence is simpler, cheaper, and deterministic. See https://rig.rs/docs/concepts/chains#agent-or-workflow.