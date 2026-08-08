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
| `Completion` | `StreamingCompletion` | Low-level streaming completion interface |

### `StreamingChat`

Same `MultiTurnStreamItem` stream as `stream_prompt`, plus chat history:

```rust
use rig::streaming::StreamingChat;
let mut stream = agent.stream_chat("Continue the story", chat_history).await;
```

### `StreamingCompletion`

Returns a request builder you can customize before sending:

```rust
use rig::streaming::StreamingCompletion;
let builder = agent.stream_completion("prompt", chat_history).await?;
let response = builder
    .temperature(0.9)
    .stream()
    .await?;
```

## Response types

**`MultiTurnStreamItem`** (`rig::agent`) — what an agent's `stream_prompt`/`stream_chat` yields across the multi-turn loop. Match:
- `StreamAssistantItem(StreamedAssistantContent)` — per-token content deltas.
- `FinalResponse(PromptResponse)` — the completed turn (output, usage, history).

Because the whole agent loop flows through this stream, you can observe tool calls and their results in real time.

**`StreamedAssistantContent`** (`rig::streaming`) — a single piece of streamed assistant output:
- `Text(text)` — text delta; read via `text.text`.
- `ToolCall` delta — partial tool name/arguments, streamed piece by piece. **Buffer until the call is complete** before executing the tool.
- final usage event — token counts for the whole completion.

**`StreamingCompletionResponse`** — what the low-level `stream_completion(...).stream()` returns. Wraps the inner stream of chunks and, once fully consumed, exposes the aggregated message + raw provider response.

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

- **Handle errors per chunk.** Starting a stream (`stream_prompt(...).await`) always succeeds, but each item is a `Result` that can fail independently — match on `item?` rather than assuming atomic success/failure.
- **Apply backpressure** with `PauseControl` or standard stream backpressure when the consumer can't keep up.
- **Read usage at the end** — the final usage event reports token counts for the entire completion, not per chunk.

## Project boundary (shuvarie)

Run `stream_prompt`/`stream_chat` on the **core task**, not the TUI thread. Forward `MultiTurnStreamItem` / `StreamedAssistantContent` events to the TUI via the existing `tokio::sync::mpsc` channel as `AppMessage` variants; the TUI's `update` consumes them and `view` renders current state. Never pull a stream from `handle_event`/`view` — those must not `await` (see `AGENTS.md` and the `ratatui` skill).