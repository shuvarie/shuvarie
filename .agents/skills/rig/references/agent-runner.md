# AgentRunner & AgentRun

`AgentRunner` is the driver behind Rig's high-level agent prompt APIs. `agent.prompt("...")` **is** the runner: it returns an explicit run value for one prompt — one set of per-run options, one call to `run()`, `stream()`, or `run_channel()`. Nothing happens until it is driven (a stream that is dropped unpolled has done nothing).

Official docs: https://rig.rs/docs/concepts/agentrunner · API: https://docs.rs/rig-agent/latest/rig_agent/agent/struct.AgentRunner.html (0.43: `rig-agent-0.43.0/src/agent/runner.rs`)

## The three layers

- **`Agent`** — reusable configuration: default model (a bus registration), preamble, tools, RAG context, memory, default hooks. Non-generic.
- **`AgentRunner`** — drives one prompt through the agent loop (built by `agent.prompt(..)` or `agent.resume(run)`). Owns per-run options: request shape, turn limits, memory behavior, tool concurrency, per-call tool context, hooks.
- **`AgentRun`** — lower-level, sans-IO *serializable* state machine. Use it to persist and resume the loop yourself (e.g. durable out-of-process approvals) and drive it with `agent.resume(run)`; it stores prompt/history/turn budget/tool-choice state.

| Want | Use |
|------|-----|
| Normal calls | `agent.prompt("...").await?` (folds to `PromptResponse`) |
| Explicit runner with per-run config | `agent.prompt("...").max_turns(5).run().await?` |
| Incremental output | `agent.prompt("...").stream()` |
| Hand-drive the driver / own executor | `agent.prompt("...").run_channel()` (future + bounded `RunEvents` feed) |
| Durable/resumable runs | `AgentRun` + `agent.resume(...)`, hand-driven IO |

## Basic runner usage

```rust
use rig_core::providers::openai::{self, OpenAI};

let agent = rig_agent::AgentBuilder::new(OpenAI::from_env()?.completion(openai::GPT_5_2))
    .preamble("You are a helpful assistant.")
    .build();

let response = agent
    .prompt("Find the answer and show your work.")
    .max_turns(5)
    .run()
    .await?;

println!("answer: {}", response.output);
println!("model calls: {}", response.completion_calls.len());
println!("tokens: {:?}", response.usage.total_tokens);
```

`.await` on a runner is `run()`; both return `PromptResponse` (`output`, `usage`, `completion_calls`, `messages`, `memory_append`, accessors `requests()`/`content()`/`provider_response_body()…`).

## Per-run controls

| Method | What it changes |
|--------|-----------------|
| `max_turns(n)` | **Total** model-call budget (initial + retries + continuations) before `PromptError::MaxTurnsError`; `0` = no model calls |
| `max_invalid_tool_call_retries(n)` | Retry budget for invalid-tool-call recovery (retries also consume turns) |
| `history(iter)` | Explicit chat history — bypasses memory for the run; committed messages come back on `PromptResponse.messages` |
| `conversation(id)` | Conversation id used to load/save configured memory |
| `without_memory()` | Disable memory load and append for this run |
| `tool_concurrency(n)` | Execute up to `n` tool calls from one turn at once; `0` clamped to `1` |
| `tool_context(ctx)` | Attach the `ToolContext` every tool dispatch clones for the run |
| `add_hook(hook)` | Append a hook after any default hooks on the agent |
| `preamble(..)` / `without_preamble()` | Override/clear the agent preamble for this run |
| `document(s)(..)` | Append static context documents for this run |
| `temperature(..)` / `without_temperature()`, `max_tokens(..)` / `without_max_tokens()` | Sampling overrides |
| `additional_params(..)` / `merge_additional_params(map)` / `replace_additional_params(..)` / `without_additional_params()` | Provider passthrough parameters |
| `tool_choice(..)` / `without_tool_choice()` | Tool-choice policy override |
| `using_model(label)` / `using_model_value(model)` | Default model candidate for this run (registered routes still apply via hooks) |
| `unhandled_invalid_tool_call(policy)` | Policy for invalid calls no hook resolves |
| `record_content_telemetry(bool)` | Opt in/out of sensitive content on telemetry spans |

> The 0.41 `tool_extensions(ext)` mechanism was removed in 0.42. Inject runtime-only values (auth tokens, session ids, request metadata) with `tool_context(ctx)` — values readable via `ctx.require::<T>()` inside `Tool::call` (typed `ContextValue`s in 0.43). See `tools.md`.

### Turns and invalid-tool retries

`max_turns` is the total model-call budget — the initial call and every follow-up/retry count. Past the limit → `PromptError::MaxTurnsError` with the history so far and the prompt that could not be dispatched.

`max_invalid_tool_call_retries` is separate — only applies when a hook recovers via `InvalidToolCallAction::Retry`. Each retry re-asks the model and also consumes normal turn budget. See `hooks.md`.

### Conversation memory

With a memory backend + conversation id, the runner loads history before the first model call and appends the completed run's messages after it (the append is acknowledged on `PromptResponse.memory_append` — a failed append does not invalidate the answer):

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

`tool_context(ctx)` attaches a `ToolContext` cloned for every tool dispatch. Each tool reads caller-provided values with `ctx.require::<T>()` — the model never sees them. Use for trusted application context (auth, tenant, session); don't put secrets in the prompt just so a tool can read them.

## Blocking and streaming runs

`run()` drives the blocking path and returns one `PromptResponse`. `stream()` drives the same runner through the streaming path, yielding assistant deltas, tool activity, and a final response. Both share run construction, tool execution, memory behavior, tracing spans, and hook handling; the streaming path adds the streamed delta items and per-delta hook events (`on_text_delta`, `on_reasoning_delta`, `on_tool_call_delta`).

```rust
let stream = agent
    .prompt("Explain the result as you work.")
    .max_turns(3)
    .stream();
```

See `streaming.md` for consuming items and `hooks.md` for streaming hook events. `run_channel()` is the middle ground: `(future, RunEvents { receiver })` — the run future plus a bounded event feed for a host with its own executor or tick (`RUN_EVENTS_CAPACITY` bounds buffered items).

## Resuming a persisted run

`agent.resume(run: AgentRun)` continues a run serialized at a step boundary (e.g. while tool calls were pending). The run is authoritative for its prompt/history/turn budget; everything else (request shape, hooks, tool context, concurrency, memory-free behavior) comes from the agent and runner. Pending tool calls re-execute on resume. Conversation memory is neither loaded nor appended (the driver that persisted the run owns persistence).

## AgentRunner vs. AgentRun

Use `AgentRunner` when Rig should perform IO: send model requests, execute tools, apply memory, emit tracing spans, run hooks. Use `AgentRun` when you want a serializable state machine and will provide the IO yourself — durable human-in-the-loop systems: serialize the run while tools are pending, wait for approval in another service, then deserialize and `agent.resume(run)`. Hooks run at the `AgentRunner` layer (they are async, side-effecting driver code); a resumed run seeds its hook context with the persisted entries (`HookContext::seed_entries`).