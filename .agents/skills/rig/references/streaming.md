# Streaming

Streaming processes an LLM response incrementally as it's generated, essential for responsive UIs and long-form output. Rig mirrors its non-streaming traits with streaming equivalents in `rig::streaming`.

Official docs: https://rig.rs/docs/concepts/streaming · API: https://docs.rs/rig/latest/rig/streaming/index.html

## Streaming an agent

`stream_prompt` (from `StreamingPrompt`) returns a stream of `MultiTurnStreamItem` values — match on them to handle text deltas and the final response:

```rust
use futures::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::client::{CompletionClient, ProviderClient};
use rig::providers::openai;
use rig::streaming::{StreamedAssistantContent, StreamingPrompt};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let openai = openai::Client::from_env()?;
    let agent = openai
        .agent("gpt-5.5")
        .preamble("You are a storyteller.")
        .temperature(0.9)
        .build();

    let mut stream = agent.stream_prompt("Tell me a short story about a robot.").await;
    while let Some(item) = stream.next().await {
        match item? {
            MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)) => {
                print!("{}", text.text);
            }
            MultiTurnStreamItem::FinalResponse(_) => println!(),
            _ => {}
        }
    }
    Ok(())
}
```

## Core traits

| Non-streaming | Streaming | Description |
|---------------|-----------|-------------|
| `Prompt` | `StreamingPrompt` | One-shot streaming prompt |
| `Chat` | `StreamingChat` | Streaming chat with history |

> The `StreamingCompletion` low-level trait was **removed in 0.42**. To stream a bare model request, call `CompletionModel::stream(req)` or `model.completion_request(...).stream()`.

### `StreamingChat`

Same `MultiTurnStreamItem` stream as `stream_prompt`, plus chat history:

```rust
use rig::streaming::StreamingChat;
let mut stream = agent.stream_chat("Continue the story", chat_history).await;
```

## Response types

**`MultiTurnStreamItem`** (`rig::agent`) — what an agent's `stream_prompt`/`stream_chat` yields across the multi-turn loop. Match:

```rust
pub enum MultiTurnStreamItem {
    StreamAssistantItem(StreamedAssistantContent),       // model-emitted content
    StreamUserItem(StreamedUserContent),                // tool results
    ToolExecutionCommitted { tool_call, internal_call_id }, // tool body ran (batched)
    CompletionCall(CompletionCall),                     // one finished completion request + usage
    ModelTurnRetried { turn },                          // hook rejected a turn for retry
    FinalResponse(PromptResponse),                      // the completed run
}
```

- `CompletionCall` items carry per-request `usage` — forward these to a usage tracker as they arrive.
- `ToolExecutionCommitted` confirms a tool actually ran (as opposed to being hook-skipped); correlate with its `ToolResult` via `internal_call_id`.
- `ModelTurnRetried` means a hook rejected the turn; discard any provisional text/reasoning deltas you rendered for `turn`.

**`StreamedAssistantContent`** (`rig::streaming`) — a single piece of streamed assistant output:

- `Text(text)` — text delta; read via `text.text`.
- `ToolCall { tool_call, internal_call_id }` — a **complete** tool call to execute; correlate its result back through `internal_call_id`.
- `ToolCallDelta { internal_call_id, content }` — partial tool name/arguments, streamed piece by piece. **Buffer until the complete `ToolCall` arrives** before executing.
- `Reasoning { reasoning, id }` — a complete reasoning block (struct variant in 0.42). Supersedes prior `ReasoningDelta`s with the same `id`; read text via `reasoning.display_text()`.
- `ReasoningDelta { id, provider_id, reasoning }` — partial reasoning text.
- `Final(StreamFinal)` — the provider's terminal record.

**`StreamedUserContent`** (`rig::streaming`):

- `ToolResult { tool_result, internal_call_id }` — a tool result; `internal_call_id` correlates with the originating `StreamedAssistantContent::ToolCall`.

> **Per-batch buffering**: rig surfaces the batch's `ToolExecutionCommitted` + `ToolResult` items only after **every** tool call of the batch settles (in call order), so a fast call's result is not surfaced while a slow sibling still runs. Shuvarie works around this per-call: the `FileChangeHook`'s early-finish channel (`shuvarie-llm`'s `with_early_finish`) surfaces each result the moment its call completes, and `provider.rs::map_agent_stream` drops the later buffered duplicates via `FileChangeHook::surfaced_early`.

> **Correlating tool calls and results**: use `internal_call_id` (a per-run rig correlator on `StreamedAssistantContent::ToolCall`, `StreamedUserContent::ToolResult`, and `MultiTurnStreamItem::ToolExecutionCommitted`). The durable provider handles live on `ToolCall::id` / `ToolResult::call` (see `completions.md`). A `ToolResult`'s `name` field is the *executed* tool's name — which can differ from the model's call when a hook repaired it.

## Streaming to stdout

```rust
use rig::agent::stream_to_stdout;
let mut stream = agent.stream_prompt("Hello!").await;
stream_to_stdout(&mut stream).await?;
```

`stream_to_stdout` prints text chunks as they arrive and ignores tool-call deltas (not meaningful to display directly).

## Streaming to stdout

```rust
use rig::agent::stream_to_stdout;
let mut stream = agent.stream_prompt("Hello!").await;
stream_to_stdout(&mut stream).await?;
```

`stream_to_stdout` prints text chunks as they arrive and ignores tool-call deltas (not meaningful to display directly).

## Pause control

`PauseControl` pauses/resumes a stream — user-controlled streaming in interactive apps:

```rust
use rig::streaming::PauseControl;
use std::sync::Arc;

let pause = Arc::new(PauseControl::new());
let pause_clone = Arc::clone(&pause);
// In another task:
pause_clone.pause();
// ...
pause_clone.resume();
```

## Practical notes

- **Handle errors per item.** Starting a stream (`stream_prompt(...).await`) always succeeds, but each item is a `Result` that can fail independently — match on `item?` rather than assuming atomic success/failure.
- **Apply backpressure** with standard stream backpressure when the consumer can't keep up.
- **Read usage at the end** — `FinalResponse.usage` aggregates across the whole run; `CompletionCall.usage` is per model request. Zero-valued usage means the provider reported no metrics.

## Project boundary (shuvarie)

Run `stream_prompt`/`stream_chat` on the **core task**, not the TUI thread. Forward `MultiTurnStreamItem` / `StreamedAssistantContent` events to the TUI via the existing `tokio::sync::mpsc` channel as `AppMessage` variants; the TUI's `update` consumes them and `view` renders current state. Never pull a stream from `handle_event`/`view` — those must not `await` (see `AGENTS.md` and the `ratatui` skill).