# AgentRunner & AgentRun

`AgentRunner` is the driver behind Rig's high-level agent prompt APIs. `agent.prompt("...").await?` is the easy path; a runner gives you an explicit value for **one run**: one prompt, one set of per-run options, one call to `run()` or `stream()`.

Official docs: https://rig.rs/docs/concepts/agentrunner · Source: https://github.com/0xPlaygrounds/rig/blob/main/crates/rig-core/src/agent/runner.rs

## The three layers

- **`Agent`** — reusable configuration: model, preamble, tools, RAG context, memory, default hooks.
- **`AgentRunner`** — drives one prompt through the agent loop. Owns per-run options: turn limits, memory behavior, tool concurrency, tool extensions, hooks.
- **`AgentRun`** — lower-level, sans-IO state machine. Use only when you need to persist and resume the loop yourself (e.g. durable out-of-process approvals).

| Want | Use |
|------|-----|
| Normal calls | `agent.prompt("...").await?` |
| Explicit runner with per-run config | `agent.runner("...").run().await?` |
| Hand-drive model + tool IO | `AgentRun` |

> This page documents the `AgentRunner` API on Rig's main branch. If docs.rs latest doesn't show these symbols yet, use the linked GitHub source for exact signatures.

## Basic runner usage

`agent.runner(prompt)` builds a runner seeded from the agent's configuration. `run()` drives the loop and returns a `PromptResponse` (final output, aggregate usage, per-call usage, message history):

```rust
use rig::client::{CompletionClient, ProviderClient};
use rig::providers::openai;

let agent = openai::Client::from_env()?
    .agent("gpt-5.5")
    .preamble("You are a helpful assistant.")
    .build();

let response = agent
    .runner("Find the answer and show your work.")
    .max_turns(5)
    .run()
    .await?;

println!("answer: {}", response.output);
println!("model calls: {}", response.completion_calls.len());
println!("tokens: {}", response.usage.total_tokens);
```

Equivalent in spirit to `agent.prompt("...").max_turns(5).extended_details().await?`. Use whichever reads better; `AgentRunner` is especially useful when you build a run in stages or share config between blocking and streaming surfaces.

## Per-run controls

| Method | What it changes |
|--------|-----------------|
| `max_turns(n)` | Max multi-turn depth before `PromptError::MaxTurnsError` |
| `max_invalid_tool_call_retries(n)` | Retry budget for invalid-tool-call recovery (retries also consume turns) |
| `history(messages)` | Explicit chat history — bypasses memory for the run |
| `conversation(id)` | Conversation id used to load/save configured memory |
| `without_memory()` | Disable memory load and save for this run |
| `tool_concurrency(n)` | Execute up to `n` tool calls from one turn at once; `0` clamped to `1` |
| `tool_extensions(ext)` | Pass runtime-only values to tools via `Tool::call_with_extensions` |
| `add_hook(hook)` | Append a hook after any default hooks on the agent |

### Turns and invalid-tool retries

`max_turns` limits follow-up model calls after tool results. Past the limit → `PromptError::MaxTurnsError` with the history so far.

`max_invalid_tool_call_retries` is separate — only applies when a hook recovers via `Flow::retry(...)`. Each retry re-asks the model and also consumes normal turn budget. See `hooks.md`.

### Conversation memory

With a memory backend + conversation id, the runner loads history before the first model call and saves the completed turn after the run:

```rust
let response = agent
    .runner("What did I ask earlier?")
    .conversation("user-42")
    .run()
    .await?;
```

`history(...)` bypasses memory completely (no load, no save). `without_memory()` is the same bypass without providing manual history.

### Tool concurrency

```rust
let response = agent
    .runner("Check inventory, shipping, and pricing.")
    .max_turns(3)
    .tool_concurrency(3)
    .run()
    .await?;
```

Final message history is persisted in tool-call order. With `tool_concurrency > 1`, per-tool side effects (logs, spans, hook callbacks) may interleave in completion order — make them concurrency-safe.

### Tool extensions

`tool_extensions` passes runtime-only values to tools (auth tokens, tenant IDs, request metadata, session state) the model should not see. Tools read them from `Tool::call_with_extensions`. Use for trusted application context — don't put secrets in the prompt just so a tool can read them.

## Blocking and streaming runs

`run()` drives the blocking path and returns one `PromptResponse`. `stream()` drives the same runner through the streaming path, yielding assistant deltas, tool activity, and a final response. Both share run construction, tool execution, memory behavior, tracing spans, and hook handling; the streaming path adds streaming-specific items and hook events (text deltas, tool-call argument deltas).

```rust
let stream = agent
    .runner("Explain the result as you work.")
    .max_turns(3)
    .stream()
    .await;
```

See `streaming.md` for consuming items and `hooks.md` for streaming hook events.

## AgentRunner vs. AgentRun

Use `AgentRunner` when Rig should perform IO: send model requests, execute tools, apply memory, emit tracing spans, run hooks. Use `AgentRun` when you want a serializable state machine and will provide the IO yourself — durable human-in-the-loop systems: serialize the run while tools are pending, wait for approval in another service, then deserialize and feed tool results back. `AgentRun` has no hooks (hooks are async, side-effecting driver code; they live at the `AgentRunner` layer).