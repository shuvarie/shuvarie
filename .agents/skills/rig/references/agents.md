# Agents

An `Agent` is Rig's primary building block. It bundles an erased completion model (`DynModel<Completion>`, registered as a handler on the agent's own effect bus) with a system prompt (`preamble`), optional context documents, tools, and (optionally) conversation memory, then runs the **agent loop** for you: send the prompt, execute any tools the model calls, feed results back, repeat until the model answers.

Official docs: https://rig.rs/docs/concepts/agent · API: https://docs.rs/rig-agent/latest/rig_agent/agent/index.html (pinned to **0.44**: the crate sources under `rig-agent-0.44.0/src/agent/` are authoritative).

> **Changed in 0.44** — this file documents 0.44 only. Headlines: the builder is *tool-state* generic (`AgentBuilder<ToolState = NoToolConfig>`, no model parameter); generation options are typed (`GenerationOptions`, `ProviderOptions`, `ModelSpec`); `PromptResponse::output()` is a **method returning `String`** (0.43 exposed an `output: String` field) and `PromptResponse::messages` is a plain `Vec<Message>` (0.43: `Option<Vec<Message>>`); the max-turns error is `PromptError::MaxTurns` (0.43: `MaxTurnsError`), and `StreamingError` is gone (see `agent-runner.md`).

## Minimal agent

```rust
use rig_agent::AgentBuilder;
use rig_core::{Model, providers::openai::{self, OpenAI}};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let agent = rig_agent::AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a helpful assistant.")
        .temperature(0.7)
        .build();
    let response = agent.prompt("Hello!").await?; // the run folded
    println!("{}", response.output());            // 0.44: a method, not a field
    Ok(())
}
```

`AgentBuilder::new(model)` takes `impl Into<DynModel<Completion>>` — pass a client-built `Model<..>` directly (it erases on the way in) or an already-erased `DynModel<Completion>` — and registers the model on the agent's bus under the label `"default"`.

> **`Agent` is non-generic.** The built `rig_agent::Agent` is a concrete, model-agnostic type — write `Agent` (never `Agent<M>`) in signatures. Re-point one later with `agent.set_model(model)` / `agent.set_model_label(label)` / `agent.register_model(label, model)`, or per run with the runner's `.using_model_value(model)` / `.using_model(label)`.

## What an agent is made of

- **Base configuration** — the default model (a bus registration), system prompt (`preamble`), `temperature`, `max_tokens`, `additional_params`, tool choice, typed generation options (`options`/`reasoning`/`cache`/…), typed per-provider options (`provider_option(s)`), the catalog entry their values are checked against (`model_spec`), and the output mode/schema.
- **Context** — documents appended to the request. Static context (`.context(doc)`) is always sent; dynamic context (`.dynamic_context(samples, index)`) is retrieved from a vector store per request.
- **Tools** — capabilities the model can call. Typed tools (`.tool(t)`) and runtime-defined (`.dynamic_tool(s)(vec)`) are always offered; retrievable tools (`.retrieved_tools(sample, index, toolset)`) are fetched per request.
- **Conversation memory** *(optional)* — a backend that loads prior history before each prompt and appends the committed turn after it (`.memory(backend)` + `.conversation(id)`). See `memory.md`.
- **Hooks** — observed/steered boundaries (`.add_hook(h)`); see `hooks.md`.

An agent holds its own **effect bus** (`Bus`) — it serves model/tool/memory/retrieval dispatches for its runs. Hosts can drive one shared bus with `AgentBuilder::over_bus(dispatcher, registrar, owner, model_key)`; distinct `owner` labels keep the generated keys (`<owner>/model:<label>`, `<owner>/memory`, `<owner>/retrieve:context#<n>`, `<owner>/tool:…`) apart. An agent can also be handed back to a host as `AgentParts` later (`into_parts()`, below).

## Builder surface (`AgentBuilder<ToolState = NoToolConfig>`)

The builder's type parameter is the **tool state**, not the model. Every generic setter is available in every state; the tool entry points are what move you between states, and they are one-way.

| Typestate | Entered by | Additional surface |
|-----------|-----------|--------------------|
| `NoToolConfig` *(default)* | `AgentBuilder::new(model)`, `named_model(label, model)`, `over_bus(dispatcher, registrar, owner, model_key)` | all tool entry points + `build()` (an agent with no builder tools) |
| `WithBuilderTools` | `.tool(t)`, `.dynamic_tool(t)`, `.dynamic_tools(vec)`, `.retrieved_tools(..)`, `.retrieved_tools_handler(..)` | more of the same; `build()` with the builder's own registry |
| `WithToolServerHandle` | `.tool_server_handle(handle)` | `build()` only — a pre-existing shared registry supplies the tools |

**Construction** (`NoToolConfig` only):

| Call | What it does |
|------|--------------|
| `new(model)` | agent over its own bus, `model` registered under label `"default"` |
| `named_model(label, model)` | same, with the label of your choice (`impl Into<ModelRef>`); size the owned bus with `configure_bus(ServingPolicy)` |
| `over_bus(dispatcher, registrar, owner, model_key)` | build over a host-driven bus, using a completion key the host registered (register it before running); `#[track_caller]` — panics at the host's line for a wrong-family binding at that key, or when `record_to` is asked over a host's bus |

**Identity & prompt** (every state):

| Call | What it does |
|------|--------------|
| `name(impl Into<String>)` / `description(impl Into<String>)` | agent name and description (the name is also the default owner label and the `into_tool()` tool name) |
| `preamble(impl Into<String>)` / `without_preamble()` / `append_preamble(&str)` | set / clear / append the system prompt |
| `owner(impl Into<String>)` | key namespace; defaults to the agent's name, else a process-local `agent#<n>` |

**Request defaults** (every state):

| Call | What it does |
|------|--------------|
| `default_max_turns(usize)` | model-call budget for runs that don't override it (default `1`) |
| `temperature(f64)` / `max_tokens(u64)` | sampling overrides |
| `additional_params(serde_json::Value)` | raw provider passthrough, merged into the request body |
| `options(GenerationOptions)` + the shortcut family | typed generation options (table below) |
| `provider_options(ProviderOptions)` / `provider_option::<O: ExtensionOptions>(o)` | typed per-provider extras |
| `model_spec(ModelSpec)` | the catalog entry this agent's model call options are validated against (see below) |
| `tool_choice(ToolChoice)` | tool-choice policy |
| `output_schema::<T>()` / `output_schema_raw(schemars::Schema)` / `output_mode(OutputMode)` | structured-output schema and how it is conveyed |
| `record_content_telemetry(bool)` | sensitive prompt/response/tool content on telemetry spans (off by default) |
| `accept_unknown_finish_reasons(bool)` | treat `FinishReason::Other` as a normal stop instead of failing the turn (off by default) |

**Context, memory, routing, hooks** (every state):

| Call | What it does |
|------|--------------|
| `context(impl Into<String>)` | append a static context document (id `static_doc_<n>`) to every request |
| `dynamic_context(samples, index)` / `dynamic_context_handler(samples, handler)` | retrieve up to `samples` documents per request through a generated `retrieve:context#<n>` index handler and a built-in completion-call hook |
| `memory(B: ConversationMemory)` / `memory_handler(impl Serve)` / `conversation(id)` | register memory under `<owner>/memory` and the conversation id it loads/saves under; `conversation(..)` without a backend only warns at build |
| `model_route(label, model)` / `model_route_handler(label, handler)` | register an extra completion model a run may select by label |
| `add_hook(H: AgentHook)` | append to the agent's default hook stack |
| `configure_bus(ServingPolicy)` | size the agent's own bus (warns over a host's bus) |
| `record_to(impl Recorder + Send + Sync)` | install a dispatch recorder (panics at build over a host's bus — the host records through its driver) |

**Tools & build** (typestate-gated):

| Call | What it does |
|------|--------------|
| `tool_server_handle(handle)` | `NoToolConfig` → `WithToolServerHandle`: build over a pre-existing shared registry |
| `tool(t)` / `dynamic_tool(t)` / `dynamic_tools(vec)` | add typed / runtime-defined tools (`NoToolConfig` → `WithBuilderTools`, then additive) |
| `retrieved_tools(sample, index, toolset)` / `retrieved_tools_handler(sample, handler, toolset)` | tool RAG: advertise up to `sample` tools per request |
| `build() -> Agent` | build with the tools of the current state (the builder's registry, the shared handle, or none) |

### Generation options

`GenerationOptions` is the typed, provider-neutral half of the request shaping; each builder/runner shortcut sets one field of it.

| Shortcut | Sets | Values |
|----------|------|--------|
| `reasoning(impl Into<Reasoning>)` | `reasoning` | `Reasoning::{Off, Effort(..), Budget{..}}` |
| `cache(CacheRetention)` | `cache` | `None` / `Short` / `Long` |
| `service_tier(ServiceTier)` | `service_tier` | `Auto` / `Default` / `Flex` / `Priority` |
| `verbosity(Verbosity)` | `verbosity` | `Low` / `Medium` / `High` |
| `parallel_tool_calls(bool)` | `parallel_tool_calls` | whether the model may call several tools per turn |
| `top_p(f64)` / `seed(u64)` | `top_p` / `seed` | nucleus sampling, deterministic sampling |
| `stop(iter)` | `stop` | stop sequences; an empty list means "not set" on the runner |
| `on_unsupported(OnUnsupported)` | `on_unsupported` | `Error` (default) or `Ignore` for an option the wire/model can't take |

Two precedence rules worth knowing (documented on the source):

- **Builder order matters**: `options(opts)` *replaces every field*, so a `seed(..)` called before it is lost, while one called after it sets its single field on top. The runner's `options(opts)` instead **overlays** the agent's fields (each field `opts` sets wins, the rest keep the agent's).
- **Provider options differ too**: `AgentBuilder::provider_options` replaces every provider entry; `AgentRunner::provider_options` overlays. Both `provider_option(o)` variants replace their own provider's entry.

`model_spec(spec: ModelSpec)` (the built-in catalog entry — `rig_core::catalog::Catalog::builtin()`) makes every model call check the options it is about to send against what that model actually takes: under `OnUnsupported::Error` a refused option fails the run with `ProviderError::UnsupportedOption` *before* the call is sent; under `Ignore` it is dropped from that call with a warning. Calls to a model the catalog does not list (a host's key, a `model_route_handler`) are not checked and log a warning.

## The built agent

| Call | Returns |
|------|---------|
| `prompt(prompt: impl Into<Message>)` | `AgentRunner` — the run (see `agent-runner.md`) |
| `prompt_typed::<T>(prompt)` | `TypedRun<T>` — a run that deserializes its output as `T` |
| `resume(run: AgentRun)` | `AgentRunner` continuing a persisted run |
| `chat(prompt, &mut Vec<Message>)` | `Result<PromptResponse, PromptError>`; runs `prompt` over caller-owned history and appends what the run committed |
| `name()` / `description()` | `Option<&str>` |
| `owner()` | `&str` — the key-namespace label |
| `model_key()` / `model_label()` / `model_descriptor()` | `&Key<family::Completion>` (`.raw()` for the wire string) / `Option<ModelRef>` / `Option<HandlerDescriptor>` |
| `register_model(label, model)` | `ModelRef` — register `model` on the agent's bus under `label` |
| `set_model(model)` / `set_model_label(label)` | re-point this agent *value*'s default; `set_model` registers anonymously, scoped to the values that select it |
| `with_model(model)` / `with_model_label(label)` | the same by value |
| `run_spec()` | `RunSpec` — serializable request settings + run policy (no models, tools, hooks or memory) |
| `required_row()` | `EffectRow` — the model, every registered tool, memory and retrieval index the program can dispatch to |
| `program_names()` | `Vec<String>` — the hook/stack names that decide on this agent's behalf (replay identity) |
| `bus_config()` | `Option<ServingPolicy>` — the agent's own bus policy, `None` over a host's bus |
| `handler_descriptor(&HandlerKey)` | `Option<HandlerDescriptor>` |
| `owns_bus()` | `bool` |
| `tool_server_handle()` | `&ToolServerHandle` — the shared registry (`add_tool` / `add_dynamic_tool` / `add_registered_tool` / `attach` / `owner`) |
| `tool_definitions(Option<String>)` | `Result<Vec<ToolDefinition>, ToolServerError>` — read-only view of what a prompt would advertise (no dispatch) |
| `into_tool()` | `Result<DynamicTool, EmptyToolName>` — the agent as a tool a manager can call |
| `into_parts()` | `Result<AgentParts, Agent>` |

`into_parts()` takes the agent apart: the dispatcher and the driver leave together, so whoever gets the dispatcher must drive it. It fails — returning the agent back — when the agent was built over a host's bus or when another clone still shares the driver. `AgentParts { dispatcher, registrar, driver, agent }` is the four pieces: the bus client half, its registration handle, the serving half to spawn or drive, and the same agent, now over the bus as a host would hold it.

Runs are explicit values: `AgentRunner` (the per-run options plus how it is driven), `TypedRun<T>` (a runner whose output deserializes as `T`) and `AgentRun` (the sans-IO, serializable state machine that `resume` takes, for durable runs you persist yourself). `agent-runner.md` documents all three.

## The agent loop

1. Hook boundaries resolve the prompt (`on_run_start`) and per-turn request shape (`on_completion_call` patches, then `on_model_select`).
2. Build the completion request from the preamble, static context, retrieved dynamic context/tools, the conversation history, and the tool definitions.
3. Send it to the model (or open its stream). One request/response round trip is a **turn**.
4. Inspect the response:
   - **text** → loop ends; that text is the response's `output()`.
   - **tool calls** → Rig validates each call, applies tool hooks, executes each (running `call(&mut ToolContext, args)`, shaping the result), and appends the results as tool-result messages. A model may request several calls in one turn; execution honors `tool_concurrency` (default sequential, committed history keeps call order).
5. Repeat from step 2 with the updated history until the model produces text (or the structured-output flow finishes) or the turn budget runs out.

### Turns and `max_turns`

`max_turns` is the **total model-call budget** — including the initial call and every retry or continuation. `0` emits no model calls; `1` (the agent default) permits only the initial call (no tool follow-up); exceeding the budget fails with `PromptError::MaxTurns { max_turns, chat_history, prompt }` (the history so far plus the prompt that could not be dispatched). `RunSpec::effective_max_turns()` is the budget a spec resolves to — `max_turns.unwrap_or(1)`.

```rust
let res = tool_agent
    .prompt("Calculate 2 + 5, then multiply by 3")
    .max_turns(5)
    .await?;
```

Set it per run with `.max_turns(n)` on the runner, or `AgentBuilder::default_max_turns(n)` for every run.

### When a tool call goes wrong

- **Your tool returns `Err`** → the normalized `ToolExecutionError` (with its model-feedback text) is sent back to the model as the failed tool result and the loop continues. The model can retry with different args or explain the failure. Make tool errors descriptive (`ToolExecutionError::invalid_args(..)`/`.other(..)`).
- **The model emits an invalid call** (unknown/disallowed tool, arguments that are not a JSON object) → fails the run as `PromptError::UnknownToolCall` by default. A hook can recover with `InvalidToolCallAction::Retry` / `Repair` / `Skip` / `Stop`; `.max_invalid_tool_call_retries(n)` bounds retry rounds (each retry also consumes turn budget), `.max_consecutive_malformed_tool_calls(limit)` bounds consecutive turns that answered a call with non-object arguments, and `.unhandled_invalid_tool_call(UnhandledInvalidToolCall::Ignore)` drops a call no hook resolved instead of failing the run. See `hooks.md`.

## Context

- `AgentBuilder::context(doc)` — static context appended to every request.
- `AgentBuilder::dynamic_context(samples, index)` — fetch up to `samples` relevant docs from a vector store per request (RAG), via a generated retrieval handler + the dynamic-context hook. `dynamic_context_handler(samples, handler)` does the same over a retrieval-family handler you supply (a replayer, say).

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
- `AgentBuilder::retrieved_tools(sample, index, toolset)` — retrieve up to `sample` relevant tools per request from a vector store (tool RAG); `retrieved_tools_handler(sample, handler, toolset)` for a handler-supplied index.
- `AgentBuilder::tool_server_handle(handle)` — build over a pre-existing shared registry instead of the builder's own (`AgentBuilder<WithToolServerHandle>`).

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
- `Specific { function_names }` — must call one of the named tools (`Vec<ToolName>`; a `ToolName` can't be empty).

## Model routing

| Want | Use |
|------|-----|
| Another model available to runs of one agent | `AgentBuilder::model_route(label, model)` (or `model_route_handler(label, handler)` for a handler) |
| Pick it for a whole run | `agent.prompt(..).using_model(label)`, or `ModelSelectionAction::select(label)` in an `on_model_select` hook |
| Pick a one-off model for a run | `agent.prompt(..).using_model_value(model)` |
| Change the agent's default | `agent.set_model(model)` / `agent.set_model_label(label)` / `agent.register_model(label, model)` |
| Ask what a run is pointed at | `agent.model_key()`, `agent.model_label()`, `agent.model_descriptor()`, `agent.required_row()` |

`ModelRef` is the serializable label a route is named by (`ModelRef::new(..)` / `as_str()`; `&str` and `String` convert into it). `ModelHandle` is the live typed view of a completion model that a run actually dispatches to — the driver binds it from the selected key; hooks reach handlers through `HookContext` (`ctx.model(key)`, `ctx.bind(key)`).

`.using_model(label)` sets the run's **default candidate**; it does not suppress registered model-selection hooks, which may replace the candidate before each call (including retries) — append an unconditional selecting hook last when a run must always use one model. `.using_model_value(model)` registers the model anonymously on the agent's bus, scoped to the runner and the run it produces.

## Agents as tools (manager-worker)

An `Agent` converts into a `DynamicTool` with `into_tool()`: the tool takes a JSON `prompt` string, runs the worker for it (`agent.prompt(args.prompt)`), inherits the caller's dispatch context, and returns the worker's `output()` as text. The tool name is the worker's configured `name()` (or `agent_tool`), and its description carries the worker's name, description and preamble.

```rust
let bob = AgentBuilder::new(model)
    .name("Bob")
    .description("Handles admin tasks at FooBar Inc.")
    .preamble("You are Bob, an admin employee.")
    .build();

let alice = AgentBuilder::new(model)
    .name("Alice")
    .description("A manager at FooBar Inc.")
    .preamble("You are Alice, a manager.")
    .dynamic_tool(bob.into_tool()?) // Bob becomes a tool Alice can call
    .build();
```

The manager's registry is live: `alice.tool_server_handle().add_dynamic_tool(worker.into_tool()?)` (or `add_tool(..)` / `add_registered_tool(..)`) registers a tool the manager sees on its next turn — the hook a host-side dispatcher plugs into (a spawn id, a session, a forwarded stream). The same shape covers a custom host dispatcher wrapped as a `DynamicTool` and handed to the manager, like shuvarie's worker tool in `shuvarie-llm::agent`, which mints a spawn id, runs a fresh single-shot worker per brief and forwards worker activity to the manager's stream.

For swarm/actor architectures see https://rig.rs/docs/guides/advanced/multi_agent_systems.

## Conversations and history

An agent is **stateless**: each `.prompt(...)` starts from scratch unless you supply earlier turns. Three ways, manual → automatic:

**Pass history explicitly** with `.history(iter)` on the runner. `Vec<Message>` — Rig does NOT append; the messages the run committed come back on `PromptResponse.messages` (a plain `Vec<Message>`, empty for a response built without a run) for you to record:

```rust
let response = agent.prompt("What's my name?").history(history.iter()).await?;
history.push(Message::user("What's my name?"));
history.extend(response.messages.iter().cloned()); // prompt + accepted assistant content
```

**Use `chat`** for the common text-in/text-out case — it takes `&mut Vec<Message>` and **appends** the committed turn (including tool calls/results) for you. It is `prompt(..).history(..)` plus the write-back, and a failed run appends nothing:

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

println!("answer: {}", response.output());
println!(
    "tokens: {:?} in / {:?} out across {} requests",
    response.usage().input_tokens,   // Option<u64>: None = provider didn't report
    response.usage().output_tokens,
    response.requests(),
);
```

- `usage` — `Usage` aggregated across every turn; fields `input_tokens`, `output_tokens`, `total_tokens`, `cached_input_tokens`, `cache_creation_input_tokens`, `tool_use_prompt_tokens`, `reasoning_tokens` (all `Option<u64>`; `None` means the provider didn't report, a reported zero is `Some(0)`) and `cost: Option<Cost>` when the provider or the catalog priced it.
- `completion_calls` — one `CompletionCall` per completed request (index, usage, response/request ids, `finish_reason`, raw provider payload); `requests()` is its length.
- `messages` — the run's transcript (the prompt, accepted assistant turns, committed tool results, retry feedback), what memory is asked to persist.
- `content` — the final turn as `Vec<AssistantContent>` (text, reasoning, images); `output()` is the concatenated text of it. In output-tool mode `content` holds the serialized structured output as one text block.
- `memory_append` — `Option<MemoryAppend>` (`Acknowledged` / `Failed { report }`), only for runs that had memory to append to.
- `output_tool_calls()` — how many synthetic output-tool calls finalized the response.

## Structured output and typed runs

- `AgentBuilder::output_schema::<T>()` (schema from `T`, which needs `schemars::JsonSchema`) or `output_schema_raw(schemars::Schema)` sets the schema; `output_mode(OutputMode)` picks how it is conveyed: `Auto` (default: tool output when a schema and function tools are present and native output can't compose with tools, else native), `Tool` (a synthetic final-answer tool the model must call), `Native` (a provider-native constraint), `Prompted` (the schema goes in the system prompt and the final text must be parsed).
- `agent.prompt_typed::<T>("...")` returns a `TypedRun<T>` = `AgentRunner<TypedOutput<T>>`: the same runner with the schema for `T` set and native output mode, whose `.await` yields `TypedPromptResponse<T> { output: T, usage, completion_calls, messages, memory_append }`. It shares `AgentRunner`'s setters (`max_turns`, `history`, `using_model`, …) plus `retries(n)`: a failed attempt (run error, empty output, unparseable output) is retried from scratch and usage accumulates across attempts.

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WeatherForecast { city: String, temperature_f: f64 }

let forecast = agent
    .prompt_typed::<WeatherForecast>("What's the weather in NYC?")
    .max_turns(3)
    .await?
    .output; // T
```

- `TypedOutput<T>` is the output marker (`TypedRun<T> = AgentRunner<TypedOutput<T>>`), `TypedPromptResponse<T>` the response; `completion_calls()` / `requests()` read the accepted attempt's calls.
- `Extractor<T>` / `ExtractorBuilder<T>` (`rig_agent::extractor`) is the same machinery with everything fixed for one job — see `structured-output.md`.

## Additional parameters

Provider-specific knobs Rig doesn't model directly pass through with `additional_params` on the builder, per-run via `.replace_additional_params(..)` / `.merge_additional_params(map)` / `.without_additional_params()`, and merge into the request body. Precedence inside one request, lowest first: mapped typed options → `provider_options` → `additional_params` — and a JSON-object `additional_params` deep-merges key by key into the wire body, while a completion-call hook's `RequestPatch` applied later still has final precedence. Prefer the typed knobs (`reasoning`, `cache`, `service_tier`, `top_p`, …) when they exist: they are validated against the model catalog and rejected or dropped predictably.

## AgentRunner and Hooks

Every `.prompt(..)` *is* an `AgentRunner` — per-run controls (history, turn limits, tool concurrency/context, model routing, generation options, hook overrides) chain on it. See `agent-runner.md`. For observing/steering the loop (audit, guardrails, approvals, invalid-tool recovery) use hooks — see `hooks.md`.

## Agent or hand-written workflow?

An agent lets the *model* decide the control flow — right for open-ended tasks. When you already know the steps, plain Rust calling models/agents in sequence is simpler, cheaper, and deterministic. See https://rig.rs/docs/concepts/chains#agent-or-workflow.
