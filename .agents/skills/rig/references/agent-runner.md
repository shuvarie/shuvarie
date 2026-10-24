# AgentRunner & AgentRun

`AgentRunner` is the driver behind Rig's high-level agent prompt APIs. `agent.prompt("...")` **is** the runner: it returns an explicit run value for one prompt — one set of per-run options, one call to `run()`, `stream()`, or `run_channel()`. Nothing happens until it is driven (a stream that is dropped unpolled has done nothing).

Official docs: https://rig.rs/docs/concepts/agentrunner · API: https://docs.rs/rig-agent/latest/rig_agent/agent/struct.AgentRunner.html (pinned to **0.45**: `rig-agent-0.45.0/src/agent/{runner.rs,streaming.rs,typed.rs}` is authoritative).

> **Changed in 0.44** — `AgentRunner<O = ()>` is now generic over what `.await` recovers (`O = ()` for the raw `PromptResponse`; `TypedRun<T> = AgentRunner<TypedOutput<T>>`), and it grew the typed-options, telemetry-content, unknown-finish-reason and malformed-argument setters below. `StreamingError` is **gone**: a stream yields `Result<MultiTurnStreamItem, PromptError>`, and `StreamedUserContent`/`StreamUserItem` is now `MultiTurnStreamItem::ToolResult { tool_result }`. Error renames: `CompletionError` → `Provider`, `MaxTurnsError` → `MaxTurns`, `PromptCancelled` → `Cancelled`, plus the new `UnknownToolCall`.

## The three layers

- **`Agent`** — reusable configuration: default model (a bus registration), preamble, tools, RAG context, memory, default hooks, generation options. Non-generic.
- **`AgentRunner<O = ()>`** — drives one prompt through the agent loop (built by `agent.prompt(..)`, `agent.resume(run)`, `agent.prompt_typed::<T>(..)`). Owns per-run options: request shape, turn limits, memory behavior, tool concurrency, per-call tool context, hooks. `#[must_use]`.
- **`AgentRun`** — lower-level, sans-IO *serializable* state machine. Use it to persist and resume the loop yourself (durable out-of-process approvals) and drive it with `agent.resume(run)`; it stores prompt/history/turn budget/tool-choice state, and owns no models, tools, memory or hooks.

| Want | Use |
|------|-----|
| Normal calls | `agent.prompt("...").await?` (folds to `PromptResponse`; the runner is `AgentRunner<()>`) |
| Explicit runner with per-run config | `agent.prompt("...").max_turns(5).run().await?` |
| Typed/structured output with retries | `agent.prompt_typed::<T>("...").retries(2).await?` (`TypedRun<T>`) |
| Incremental output | `agent.prompt("...").stream()` → `StreamingResult` |
| Hand-drive the driver / own executor | `agent.prompt("...").run_channel()` → `(future, RunEvents)` |
| Print a stream to stdout | `stream_to_stdout(&mut stream).await` |
| Resume a persisted run | `agent.resume(run)` (`AgentRun` deserialized) |
| No Rig IO at all | `rig_agent::run` protocol: `AgentRun` + `prepare_request` + your own model call |

## Basic runner usage

```rust
use rig_core::providers::openai::{self, OpenAI};
use rig_core::Model;

let agent = rig_agent::AgentBuilder::new(OpenAI::from_env()?.completion(openai::GPT_5_2))
    .preamble("You are a helpful assistant.")
    .build();

let response = agent
    .prompt("Find the answer and show your work.")
    .max_turns(5)
    .run()
    .await?;

println!("answer: {}", response.output());           // 0.44: output() is a method
println!("model calls: {}", response.completion_calls().len());
println!("tokens: {:?}", response.usage().total_tokens);
```

`.await` on a runner is `run()`; both return `PromptResponse`: fields `usage`, `completion_calls`, `messages` (a plain `Vec<Message>` since 0.44), `content`, plus the accessors `output() -> String`, `usage()`, `messages()`, `content()`, `completion_calls()`, `requests()` (number of completion calls), `output_tool_calls()`. (`memory_append` left in 0.45 — a run backed by a session store persists through its `Writer`.) A typed run's `.await` yields `TypedPromptResponse<T>` instead (see `agents.md`).

## Per-run controls

Every setter takes and returns the runner (chainable), and never mutates the source agent.

| Method | What it changes |
|--------|-----------------|
| `max_turns(n)` | **Total** model-call budget (initial + retries + continuations) before `PromptError::MaxTurns`; `0` = no model calls, `1` = initial call only |
| `max_invalid_tool_call_retries(n)` | Retry budget for invalid-tool-call recovery (retries also consume turns) |
| `max_consecutive_malformed_tool_calls(impl Into<Option<usize>>)` | Consecutive turns allowed to answer a call whose arguments are not a JSON object before the run fails; `None` (default) = no limit. A resumed run keeps the limit it persisted |
| `unhandled_invalid_tool_call(UnhandledInvalidToolCall)` | Policy for an unknown/disallowed call no hook resolves: `Fail` (default) or `Ignore` (drop the call, continue) |
| `history(iter)` | Explicit chat history (`Vec<Message>`) — bypasses memory for the run; committed messages come back on `PromptResponse.messages` |
| `conversation(id)` / `without_memory()` | Conversation id used to load/save configured memory / disable memory load and append for this run |
| `tool_concurrency(n)` | Execute up to `n` tool calls from one turn at once; `0` clamped to `1` |
| `tool_context(ctx)` | Attach the `ToolContext` every tool dispatch clones for the run |
| `add_hook(hook)` | Append a hook after any default hooks on the agent |
| `preamble(..)` / `without_preamble()` | Override/clear the agent preamble for this run |
| `document(doc)` / `documents(iter)` | Append static context documents for this run (`Document`s) |
| `temperature(..)` / `without_temperature()`, `max_tokens(..)` / `without_max_tokens()` | Sampling overrides |
| `replace_additional_params(value)` / `merge_additional_params(map)` / `without_additional_params()` | Provider passthrough parameters: replace all / shallow-merge object fields (later wins) / clear |
| `options(GenerationOptions)` | Overlay typed generation options on the agent's (only the fields set win) |
| `reasoning(..)` / `cache(..)` / `service_tier(..)` / `verbosity(..)` / `parallel_tool_calls(bool)` / `top_p(f64)` / `seed(u64)` / `stop(iter)` / `on_unsupported(policy)` | One `GenerationOptions` field each; `stop` is `.options(GenerationOptions::new().stop(stop))`, so an empty list means "not set" (it does not clear the agent's) |
| `provider_options(ProviderOptions)` / `provider_option(o)` | Typed per-provider extras; here they **overlay** the agent's entry per provider (the builder's variants replace) |
| `tool_choice(..)` / `without_tool_choice()` | Tool-choice policy override |
| `using_model(label)` / `using_model_value(model)` | Default model candidate for this run (registered routes still apply via hooks) |
| `record_content_telemetry(bool)` | Opt in/out of sensitive content on GenAI telemetry spans |
| `accept_unknown_finish_reasons(bool)` | Treat `FinishReason::Other` as a normal stop for this run (off by default) |

Order matters for the options shortcuts: `options(opts)` overlays only the fields `opts` sets — it wins over a shortcut called before it for those fields, and a shortcut called after it sets its one field on top.

### Turns and invalid tool calls

`max_turns` is the total model-call budget — the initial call and every follow-up/retry count. Past the limit → `PromptError::MaxTurns { max_turns, chat_history, prompt }` with the history so far and the prompt that could not be dispatched.

`max_invalid_tool_call_retries` is separate — only applies when a hook recovers via `InvalidToolCallAction::Retry`. Each retry re-asks the model and also consumes normal turn budget. Malformed arguments (not a JSON object) additionally count against `max_consecutive_malformed_tool_calls`; a turn whose calls all parse resets that count, and a recovered turn's completion-outcome hook is suppressed. If no hook resolves the call, `unhandled_invalid_tool_call` decides: fail the run with `PromptError::UnknownToolCall`, or ignore the call and continue. See `hooks.md`.

### Conversation memory

With a session store (`rig_memory::AsyncStore`) + log name (`.memory(store).conversation(log)`), the runner loads history before the first model call (a load failure fails the run with `PromptError::Memory` before any completion) and appends the completed run's messages after it, through the session's `Writer` (**0.45**: there is no per-run append receipt on `PromptResponse`):

```rust
let response = agent
    .prompt("What did I ask earlier?")
    .conversation("user-42")
    .run()
    .await?;
```

`history(...)` bypasses memory completely (no load, no append). `without_memory()` is the same bypass without providing manual history.

### Tool concurrency

```rust
let response = agent
    .prompt("Check inventory, shipping, and pricing.")
    .max_turns(3)
    .tool_concurrency(3)
    .run()
    .await?;
```

Final message history is persisted in tool-call order. With `tool_concurrency > 1`, per-tool side effects (logs, spans, hook callbacks) may interleave in completion order — make them concurrency-safe (hook state: the run's `Scratchpad` is shared). Streamed runs surface model tool calls at turn commit, but execution confirmations/results only after the whole batch settles (see `streaming.md`).

### Tool context

`tool_context(ctx)` attaches a `ToolContext` cloned for every tool dispatch. Each tool reads caller-provided values with `ctx.require::<T>()` — the model never sees them. Use for trusted application context (auth, tenant, session); don't put secrets in the prompt just so a tool can read them. See `tools.md`.

## Prompting errors: `PromptError`

One error type for a run, on both the awaited and streamed surfaces (`#[non_exhaustive]`):

| Variant | Carries | Renamed from (0.43) |
|---------|---------|---------------------|
| `Provider(ProviderError)` | a provider completion failure | `CompletionError` |
| `Report(ErrorReport)` | a structured bus/handler/hook/stream failure; a relayed provider failure arrives here | — |
| `Memory(MemoryError)` | conversation memory failed to load or persist | — |
| `MaxTurns { max_turns: usize, chat_history: Vec<Message>, prompt: Message }` | the budget and what could not be dispatched | `MaxTurnsError` |
| `Cancelled { chat_history: Vec<Message>, reason: String }` | the run was stopped (hook, protocol violation) | `PromptCancelled` |
| `UnknownToolCall { tool_name: String, available_tools: Vec<String>, allowed_tools: Vec<String>, chat_history: Vec<Message> }` | the model called a tool the turn did not allow | *(new)* |

`PromptError::cancelled(history, reason)` builds the cancellation variant; `From<ProviderError>` maps a relayed provider error to `Report` and everything else to `Provider`. Provider response details are forwarded through the wrapped error: `provider_response_body()`, `provider_response_json()`, `provider_request_id()`, `provider_response_status()`, `provider_response_headers()`. `stream_to_stdout` has its own `StreamToStdoutError { Run(PromptError), Io(std::io::Error), Incomplete }`.

## Blocking and streaming runs

`run()` drives the blocking path and returns one `PromptResponse`. `stream()` drives the same runner through the streaming path, yielding assistant deltas, tool activity, and a final response. Both share run construction, tool execution, memory behavior, tracing spans and hook handling; the streaming path adds the streamed delta items and per-delta hook events (`on_text_delta`, `on_reasoning_delta`, `on_tool_call_delta`).

```rust
let stream = agent
    .prompt("Explain the result as you work.")
    .max_turns(3)
    .stream();
```

`StreamingResult` is `WasmBoxedStream<'static, Result<MultiTurnStreamItem, PromptError>>` (a `Send` boxed stream on native targets): its items are `Result<MultiTurnStreamItem, PromptError>` — **never** `StreamingError`. `MultiTurnStreamItem` is `#[non_exhaustive]`:

| Item | Meaning |
|------|---------|
| `StreamAssistantItem(Item<StreamEvent>)` | provider stream item: part starts/ends, text and reasoning fragments, a tool call's argument fragments as they stream |
| `ToolCall { tool_call }` | a call the **model** emitted, reported at turn commit for each call routed to execution |
| `ToolExecutionCommitted { tool_call }` | Rig executed and committed the call, with any `DispatchAction::Patch` rewrite applied |
| `ToolResult { tool_result }` | the result of an executed (or hook-skipped) call — 0.44 replaced `StreamedUserContent` with this |
| `CompletionCall(CompletionCall)` | one finished completion request (per-call usage, ids, finish reason, raw) |
| `ModelTurnRetried { turn }` | the completed turn was rejected for retry; text already emitted for it was provisional |
| `FinalResponse(PromptResponse)` | terminal: the unified response shared with the blocking surface (the stream's `Err` item is the other terminal) |

`run_channel()` is the middle ground: `(future, RunEvents)` — the run future plus a bounded event feed for a host with its own executor or tick. `RUN_EVENTS_CAPACITY` is `32` buffered items; a slow consumer applies back-pressure, dropping the feed lets the run finish unobserved. `RunEvents` implements `Stream` (so async code can poll it) and `FusedStream`, plus non-blocking `try_next()` and `is_done()` for a synchronous tick.

`stream_to_stdout(&mut stream)` prints assistant text and reasoning, marks model-turn retries, and returns the final `PromptResponse`; metadata items such as `CompletionCall` are not printed (they stay available on the response).

## Resuming a persisted run

`agent.resume(run: AgentRun)` continues a run serialized at a step boundary (e.g. while tool calls were pending) and returns an `AgentRunner` like any other — drive it with `.await`, `stream()` or `run_channel()`.

The run is authoritative for what it persisted: its prompt, history, turn budget, invalid-tool-call retry budget and unhandled-invalid-call policy — so `history(..)`, `max_turns(..)` and `unhandled_invalid_tool_call(..)` on the returned runner have no effect. Everything else comes from the agent and the runner: request shape (preamble, documents, sampling, additional params, tool choice, output mode), `add_hook`, `tool_context`, `tool_concurrency`, model selection and telemetry settings (inbound tool context is driver state and is never persisted). Two run-side values still show through: the run's persisted tool choice is what invalid-call hooks see and what gates a `Skip`, while the request's tool choice is the runner's; and an output tool the run committed stays committed.

Pending tool calls re-execute on resume: a tool that ran before the suspension and answered nothing runs again. History is never *loaded* — the run carries it; when a session store and log name are configured, the run's messages are persisted once, at its end, by the session's `Writer`. Treat that append as the persisting driver's job rather than something to depend on (and in 0.45 there is no `memory_append` receipt to check). A run suspended by another rig version is refused (see `RUN_FORMAT` below).

## Durable runs without the driver: `rig_agent::run`

`rig_agent::run` is the protocol the runner is built on — plain, owned, serde-friendly data you can drive yourself with your own IO, hooks, tools and memory.

| Type | What it is |
|------|-----------|
| `AgentRun` | the sans-IO loop state machine (`Serialize` + `Deserialize`, `#[serde(deny_unknown_fields)]`) |
| `AgentRunStep` | the next action you must perform: `CallModel { prompt, history, turn }`, `CallTools { calls: Vec<PendingToolCall> }`, `Done(PromptResponse)` |
| `ModelTurn` | one completed model turn fed back in: `head`, `response_id`, `provider_request_id`, `choice`, `usage`, `executable_tool_names`, `allowed_tool_names`, `finish_reason`, `raw` |
| `ModelTurnOutcome` | what happens after ingesting it: `Continue { response_hook_suppressed }`, `NeedsResolution(InvalidToolCallContext)`, `TurnRetried` |
| `PendingToolCall` | `tool_call` plus an optional `preresolved_result` (a call suppressed by recovery: return it without executing or hooking) |
| `TurnTools` | the definitions advertised for one call (`turn`, `definitions`, `contains(name)`), recorded with `advertise_tools(turn, defs)` and read with `advertised_tools()` |
| `RunEntry` | an append-only host record: `kind`, `turn`, `value` (JSON), stored verbatim and never sent to the provider |
| `RunSpec` | serializable request settings + policy (table below) |
| `RequestPatch` | a non-sticky per-turn override a completion-call hook returns |
| `PreparedRequest` / `prepare_request` / `PrepareError` | the pure request-shaping step, with no IO |

**Protocol.** `next_step() -> Result<AgentRunStep, PromptError>` advances the machine; feed a `ModelTurn` back with `model_response(turn)`, tool results with `tool_results(Vec<UserContent>)` (one result per pending call, matched by call id), and answer a `NeedsResolution` with `resolve_invalid_tool_call(InvalidToolCallAction)` or `ignore_invalid_tool_call()`. A call whose arguments are not a JSON object is offered to your hook through `malformed_tool_call_context(&tool_call, is_streaming)`. `Retry` re-asks the model and so also consumes the model-call budget; `Repair` renames the call and revalidates the new name against the allowed set; `Skip` records a synthetic result and suppresses execution of *every* call in the turn (rejected under `ToolChoice::None`); `Stop` cancels the run.

**Persistence.** `RUN_FORMAT: u32 = 2` is written on every serialization and checked on every deserialization: state written by another format is refused *by name* (an error saying so: `resume refused: the run is format …, this rig reads format 2`) rather than loaded with defaults, and an unknown key is refused. A run is plain owned data (`Send + Sync` on native targets), so it can live in shared state, a database row, or an ECS component.

**Building and inspecting a run.**

| Call | Effect |
|------|--------|
| `AgentRun::new(prompt)` | one prompt, a one-call budget, no retries, no input history |
| `AgentRun::from_spec(&RunSpec, prompt, history: Option<Vec<Message>>)` | applies the spec's budget, retry budgets, unhandled-call policy, output validation (`RunSpec::DEFAULT_OUTPUT_RETRIES` = `1`), tool choice and output-tool name |
| `max_turns(n)`, `max_invalid_tool_call_retries(n)`, `max_consecutive_malformed_tool_calls(limit)`, `with_unhandled_invalid_tool_call(policy)` | run policy |
| `with_tool_choice(ToolChoice)` | the choice invalid-call handling reports and `Skip` is checked against |
| `with_output_validation(schema, max_output_retries)` / `with_output_tool_name(name)` | structured-output validation and the synthetic output tool |
| `with_history(history)` / `with_validated_history(history)` | input history; the latter checks it is a canonical transcript (`TranscriptError`) |
| `initial_prompt()` / `input_chat_history()` | the pending prompt (before the first `CallModel`) and the history it was created with |
| `turn()`, `usage()`, `completion_calls()`, `messages()`, `full_history()`, `previous_model()`, `set_previous_model(label)` | inspection (`messages()` excludes the input history) |
| `is_done()`, `response()`, `accepted_turn_choice()`, `pending_invalid_tool_call()` | terminal state and what is parked awaiting your call |
| `replace_accepted_turn_choice(choice)`, `retry_model_turn(RetryRequest)`, `cancel_error(reason)` | steer an accepted tool-free turn, retry it, or build the cancellation error your hooks should return |
| `advertise_tools(turn, defs)`, `append_entry(entry)`, `entries()`, `entries_of(kind)`, `last_entry_of(kind)` | protocol data and host records |

Hooks attached to an `AgentRunner` keep their state across a suspension through `HookContext::append_entry(kind, value)` (stored on the run, excluded from requests) and read it back with `entries(kind)` / `last_entry(kind)` — the driver seeds the resumed run's entries into the hook context before any hook fires.

**Streamed turns, driven by hand.** `record_streamed_completion_call(usage, identity, finish_reason, raw)` then `streamed_turn(turn)` feed an assembled streamed turn in; `StreamedTurnAssembler` (with `PartialStreamedTurn`, `StreamedInvalidToolCall`, `StreamedTurn`, `StreamedTurnEvent`, `StreamedResolution`) is the sans-IO assembler for the provider's stream items, and `streamed_invalid_tool_call_context(..)` / `resolve_streamed_invalid_tool_call(..)` / `ignore_streamed_invalid_tool_call()` mirror the buffered recovery path.

**`RunSpec`** — `#[serde(default)]`, so a missing field takes `RunSpec::new()`'s value:

| Field | Meaning |
|-------|---------|
| `preamble: Option<String>`, `static_context: Vec<Document>` | system prompt and always-attached documents |
| `temperature: Option<f64>`, `max_tokens: Option<u64>`, `additional_params: Option<serde_json::Value>` | sampling and raw provider passthrough |
| `options: GenerationOptions`, `provider_options: ProviderOptions` | typed options (omitted from the serialized spec while default/empty) |
| `tool_choice: Option<ToolChoice>` | tool-choice policy |
| `max_turns: Option<usize>` | total model-call budget; `None` = the protocol default of one call (`effective_max_turns()` resolves it) |
| `max_invalid_tool_call_retries: usize`, `max_consecutive_malformed_tool_calls: Option<usize>`, `unhandled_invalid_tool_call: UnhandledInvalidToolCall` | invalid-call and malformed-argument policy |
| `output_schema: Option<serde_json::Value>`, `output_mode: OutputMode`, `output_tool_name` / `output_tool_description: Option<String>`, `augment_output_preamble: bool` | structured output: the schema, how it is conveyed (`Auto`/`Tool`/`Native`/`Prompted`), the synthetic tool's name/description, and whether the driver may inject the structured-output instructions into the preamble |
| `accept_unknown_finish_reasons: bool` | whether `FinishReason::Other` ends a turn as a normal stop (carried into every prepared request) |

**`UnhandledInvalidToolCall`** — `Fail` (default: fail the run when every `on_invalid_tool_call` hook declines to resolve an unknown/disallowed call) or `Ignore` (drop the call and continue the turn).

**`RequestPatch`** — a per-turn override that never mutates the agent's baseline and is not carried into later turns: `preamble`, `temperature`, `max_tokens`, `tool_choice`, `active_tools: Option<Vec<String>>` (narrows what is advertised), `additional_params`, `extra_context: Vec<Document>`, `history: Option<Vec<Message>>`. Build it with `new()` plus one setter per field, or `merge(later)` — `extra_context` appends, object `additional_params` shallow-merge (later keys win), `active_tools` intersect, and scalars/`history` are last-writer-wins (with a warning when two hooks set the same field). `is_empty()` reports a patch that sets nothing.

**Pure preparation.**

```rust
use rig_agent::run::{PrepareError, RunSpec, prepare_request};
use rig_core::completion::{CompletionRequest, ProviderCapabilities};

fn prepare(
    spec: &RunSpec,
    capabilities: &ProviderCapabilities,
) -> Result<CompletionRequest, PrepareError> {
    let prepared = prepare_request(
        spec,
        capabilities,
        &[],    // ordered history
        vec![], // tools in advertisement order
        None,   // committed output-tool name, if the run pinned one
        None,   // an optional merged RequestPatch
    )?;
    Ok(prepared.apply(CompletionRequest::new("Who are you?")))
}
```

`PreparedRequest` is what that call returns: `chat_history` (the effective preamble as a leading system message, then history), `documents`, `tools` (executable tools plus, in Tool mode, the synthetic output tool last), `temperature`, `max_tokens`, `additional_params`, `tool_choice`, `options`, `provider_options`, `output_schema`, `output_mode` (never `Auto`), `executable_tool_names`, `allowed_tool_names`, `output_tool_name`, `accept_unknown_finish_reasons`. Apply it with `apply(CompletionRequest)` in the protocol's canonical order, then add only what is yours (telemetry flags) and send. `PrepareError` has two local, pre-IO variants: `Request(String)` (an impossible tool choice, an `active_tools` name that is not available, an output-tool collision) and `InvalidOutputSchema(serde_json::Error)`; it converts into a `ProviderError`.

## AgentRunner vs. AgentRun

Use `AgentRunner` when Rig should perform IO: send model requests, execute tools, apply memory, emit tracing spans, run hooks. Use `AgentRun` when you want a serializable state machine and will provide the IO yourself — durable human-in-the-loop systems: serialize the run while tools are pending, wait for approval in another service, then deserialize and `agent.resume(run)`. Hooks run at the `AgentRunner` layer (they are async, side-effecting driver code); a resumed run seeds its hook context with the persisted `RunEntry` log, which hooks read through `HookContext::{entries, last_entry}`. A run suspended by another `RUN_FORMAT` is refused rather than resumed.
