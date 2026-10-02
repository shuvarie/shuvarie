# Agents

An `Agent` is Rig's primary building block. It bundles an erased completion model (`DynModel<Completion>`) with a system prompt (`preamble`), optional context documents, tools, and (optionally) conversation memory, then runs the **agent loop** for you: send the prompt, execute any tools the model calls, feed results back, repeat until the model answers.

Official docs: https://rig.rs/docs/concepts/agent · API: https://docs.rs/rig-agent/latest/rig_agent/agent/index.html (for 0.43, the crate sources under `rig-agent-0.43.0/src/agent/` are authoritative).

## Minimal agent

```rust
use rig_agent::AgentBuilder;
use rig_core::{Model, providers::openai::{self, OpenAI}};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let agent =rig_agent::AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a helpful assistant.")
        .temperature(0.7)
        .build();
    let response = agent.prompt("Hello!").await?; // the run folded
    println!("{}", response.output);
    Ok(())
}
```

`AgentBuilder::new(model)` takes `impl Into<DynModel<Completion>>` — pass a client-built `Model<..>` directly (it erases on the way in) or an already-erased `DynModel<Completion>`. `.preamble(...)`, `.temperature(...)`, `.build()` configure it.

> **`Agent` is non-generic** (since 0.42). The builder registers the model on the agent's bus under the label `"default"` (`AgentBuilder::named_model(label, model)` to choose it); the built `rig_agent::Agent` is a concrete, model-agnostic type — write `Agent` (never `Agent<M>`) in signatures. Swap defaults later with `agent.set_model(model)` / `agent.set_model_label(label)` / `agent.register_model(label, model)`, or per-run with `.using_model_value(model)` / `.using_model(label)`.

## What an agent is made of

- **Base configuration** — default completion model (a bus registration), system prompt (`preamble`), `temperature`, `max_tokens`, `additional_params`, tool choice, output mode/schema.
- **Context** — documents appended to the request. Static context (`.context(doc)`) is always sent; dynamic context (`.dynamic_context(n, index)`) is retrieved from a vector store per request.
- **Tools** — capabilities the model can call. Typed tools (`.tool(t)`) and runtime-defined (`.dynamic_tools(vec)`) are always offered; retrievable tools (`.retrieved_tools(sample, index, toolset)`) are fetched per request.
- **Conversation memory** *(optional)* — a backend that loads prior history before each prompt and appends the committed turn after it (`.memory(backend)` + `.conversation(id)`). See `memory.md`.
- **Hooks** — observed/steered boundaries (`.add_hook(h)`); see `hooks.md`.

An agent holds its own **effect bus** (`Bus`) — it serves model/tool/memory/retrieval dispatches for its runs. Hosts can drive one shared bus with `AgentBuilder::over_bus(dispatcher, registrar, owner, model_key)`; distinct `owner` labels keep the generated keys (`<owner>/model:<label>`, `<owner>/memory`, …) apart.

## The agent loop

1. Hook boundaries resolve the prompt (`on_run_start`) and per-turn request shape (`on_completion_call` patches, then `on_model_select`).
2. Build the completion request from the preamble, static context, retrieved dynamic context/tools, the conversation history, and the tool definitions.
3. Send it to the model (or open its stream). One request/response round trip is a **turn**.
4. Inspect the response:
   - **text** → loop ends, that text is `PromptResponse.output`.
   - **tool calls** → Rig validates each call, applies tool hooks, executes each (running `call(&mut ToolContext, args)`, shaping the result), and appends the results as tool-result messages. A model may request several calls in one turn; execution honors `tool_concurrency` (default sequential, committed history keeps call order).
5. Repeat from step 2 with the updated history until the model produces text (or the structured-output flow finishes) or the turn budget runs out.

### Turns and `max_turns`

`max_turns` is the **total model-call budget** — including the initial call and every retry or continuation. `0` emits no model calls; `1` permits only the initial call (no tool follow-up); exceeding the budget fails with `PromptError::MaxTurnsError` (which carries the accumulated history and the undelivered prompt).

Set it per run with `.max_turns(n)` on the runner, or `AgentBuilder::default_max_turns(n)` for every run:

```rust
let res = tool_agent
    .prompt("Calculate 2 + 5, then multiply by 3")
    .max_turns(5)
    .await?;
```

### When a tool call goes wrong

- **Your tool returns `Err`** → the normalized `ToolExecutionError` (with its model-feedback text) is sent back to the model as the failed tool result and the loop continues. The model can retry with different args or explain the failure. Make tool errors descriptive (`ToolExecutionError::invalid_args(..)`/`.other(..)`).
- **The model emits an invalid call** (unknown/disallowed tool, malformed JSON arguments) → fails the prompt immediately by default. A hook can recover with `InvalidToolCallAction::Retry` / `Repair` / `Skip`; `.max_invalid_tool_call_retries(n)` bounds retry rounds (each retry also consumes turn budget). See `hooks.md`.

## Context

- `AgentBuilder::context(doc)` — static context appended to every request.
- `AgentBuilder::dynamic_context(n, index)` — fetch up to `n` relevant docs from a vector store per request (RAG), via a generated retrieval handler + the dynamic-context hook.

```rust
let agent = rig_agent::AgentBuilder::new(openai.completion(openai::GPT_5_2))
    .preamble("You are a knowledge base assistant.")
    .dynamic_context(3, index) // top 3 relevant docs per request
    .temperature(0.3)
    .build();
```

See `rag-and-vector-stores.md`.

## Tools

- `AgentBuilder::tool(t)` — typed tool, always offered (typestate: once you add one, further `tool`/`dynamic_tool(s)` calls continue on `AgentBuilder<WithBuilderTools>`).
- `AgentBuilder::dynamic_tool(t)` / `dynamic_tools(vec)` — runtime-defined tools (`DynamicTool`), offered every turn.
- `AgentBuilder::retrieved_tools(sample, index, toolset)` — retrieve up to `sample` relevant tools per request from a vector store (tool RAG).
- `AgentBuilder::tool_server_handle(handle)` — build over a pre-existing shared registry instead of the builder's own.

```rust
let agent = rig_agent::AgentBuilder::new(openai.completion(openai::GPT_5_2))
    .preamble("You are a tool-using assistant.")
    .tool(calculator)
    .dynamic_tool(worker_tool)
    .retrieved_tools(2, tool_store_index, toolset)
    .build();
```

See `tools.md`.

### Steering tool use with `ToolChoice`

```rust
use rig_core::message::ToolChoice;

let agent = AgentBuilder::new(model)
    .preamble("You are a calculator. Always compute with tools.")
    .tool_choice(ToolChoice::Required) // must call a tool before answering
    .build();
```

- `Auto` (default) — model may call tools or answer directly.
- `None` — tools visible but must not be called.
- `Required` — must call at least one tool.
- `Specific { function_names }` — must call one of the named tools.

## Agents as tools (manager-worker)

A worker agent's default surface is a tool — either register the worker agent itself (its `Agent` implements tool-ish registration via its tool server) or wrap a custom host-side dispatcher as a `DynamicTool` and hand it to the manager, like shuvarie does: the worker's *host tool* (e.g. `WorkerAgent` in `shuvarie-llm::agent`) mints a spawn id, runs a fresh single-shot `client.completion(model) → AgentBuilder → agent.prompt(task).stream()` per brief, and forwards worker activity to the manager's stream.

```rust
// Rig-native shape: register the worker as a model route / tool on the manager.
let bob = AgentBuilder::new(model).name("Bob")
    .description("Handles admin tasks at FooBar Inc.")
    .preamble("You are Bob, an admin employee.")
    .build();
let alice = AgentBuilder::new(model).name("Alice")
    .description("A manager at FooBar Inc.")
    .preamble("You are Alice, a manager.")
    .build();
// ...and expose `bob` to `alice` through the tool surface that fits your host
// (`alice.tool_server_handle()` registers external tools the manager sees next turn).
```

For swarm/actor architectures see https://rig.rs/docs/guides/advanced/multi_agent_systems.

## Conversations and history

An agent is **stateless**: each `.prompt(...)` starts from scratch unless you supply earlier turns. Three ways, manual → automatic:

**Pass history explicitly** with `.history(iter)` on the runner. `Vec<Message>` — Rig does NOT append; the messages the run committed come back on `PromptResponse.messages` for you to record:

```rust
let response = agent.prompt("What's my name?").history(history.iter()).await?;
history.push(Message::user("What's my name?"));
if let Some(messages) = response.messages {
    history.extend(messages);           // prompt + accepted assistant content
}
```

**Use `chat`** for the common text-in/text-out case — it takes `&mut Vec<Message>` and **appends** the committed turn (including tool calls/results) for you:

```rust
let response = chat_agent.chat("Hello!", &mut previous_messages).await?;
```

**Attach conversation memory** — a memory backend + conversation id loads stored history before each prompt and appends the committed turn after (the append is acknowledged on `PromptResponse.memory_append`):

```rust
use rig_core::memory::InMemoryConversationMemory;

let agent = rig_agent::AgentBuilder::new(model)
    .preamble("You are a helpful assistant.")
    .memory(InMemoryConversationMemory::new())
    .conversation("user-42")
    .build();

let _ = agent.prompt("My name is Ada.").await?;
let reply = agent.prompt("What's my name?").await?;
```

See `memory.md` for bypass rules (`.history(..)`/`chat` bypass memory for that run) and durable backends.

## Token usage & run details

`agent.prompt(...).await` returns a full `PromptResponse` (output text, aggregate usage, per-call breakdown, committed messages):

```rust
let response = agent
    .prompt("What is 2 + 2?")
    .max_turns(3)
    .await?;

println!("answer: {}", response.output);
println!(
    "tokens: {:?} in / {:?} out across {} requests",
    response.usage.input_tokens,   // Option<u64>: None = provider didn't report
    response.usage.output_tokens,
    response.requests(),
);
```

`usage` aggregates across every turn; `completion_calls` breaks down per model request; `messages` holds what the run committed. `None` counters mean the provider didn't report (a reported zero is `Some(0)`).

## Additional parameters

Provider-specific knobs Rig doesn't model directly (reasoning settings, etc.) pass through with `additional_params` on the builder, per-run via `.additional_params(..)` / `.merge_additional_params(map)` / `.replace_additional_params(..)`, and merge into the request body.

## AgentRunner and Hooks

Every `.prompt(..)` *is* an `AgentRunner` — per-run controls (history, turn limits, tool concurrency/context, model routing, hook overrides) chain on it. See `agent-runner.md`. For observing/steering the loop (audit, guardrails, approvals, invalid-tool recovery) use hooks — see `hooks.md`.

## Agent or hand-written workflow?

An agent lets the *model* decide the control flow — right for open-ended tasks. When you already know the steps, plain Rust calling models/agents in sequence is simpler, cheaper, and deterministic. See https://rig.rs/docs/concepts/chains#agent-or-workflow.