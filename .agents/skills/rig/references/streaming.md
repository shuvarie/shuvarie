# Streaming

Streaming processes an LLM response incrementally as it's generated, essential for responsive UIs and long-form output. In 0.43 the streaming surface is a **runner mode**: `agent.prompt(...)` builds an `AgentRunner`; driving it with `.stream()` yields one lazy, `Send` stream of `MultiTurnStreamItem`s (identical loop, hooks and history as `.run()`; only the deltas differ). The 0.42 `Prompt`/`Chat`/`StreamingPrompt`/`StreamingChat` traits are gone.

Official docs: https://rig.rs/docs/concepts/streaming · API: https://docs.rs/rig-agent/latest/rig_agent/agent/struct.AgentRunner.html (for 0.43 the authoritative reference is `rig-agent-0.43.0/src/agent/streaming.rs` + `rig-core-0.43.0/src/streaming/`).

## Streaming an agent

```rust
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
use rig_core::message::Message;
use rig_core::providers::openai::{self, OpenAI};

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let openai = OpenAI::from_env()?;
    let agent = rig_agent::AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a storyteller.")
        .build();

    // Lazy: nothing runs until polled. `.history(..)` seeds chat history.
    let mut stream = agent.prompt("Tell me a short story about a robot.").stream();
    while let Some(item) = stream.next().await {
        match item? {
            MultiTurnStreamItem::StreamAssistantItem(item) => {
                use rig_core::streaming::{Item, StreamEvent};
                if let Item::Event(StreamEvent::Text { text, .. }) = item {
                    print!("{text}");
                }
            }
            MultiTurnStreamItem::FinalResponse(res) => println!("{}", res.output),
            _ => {}
        }
    }
    Ok(())
}
```

The stream's item type is `Result<MultiTurnStreamItem, StreamingError>`; the last item is the `FinalResponse` (or an `Err`). It runs under the span it was *built* in (whatever ambient span the builder saw, else a fresh `invoke_agent`).

## The run stream: `MultiTurnStreamItem` (`rig_agent::agent`)

```rust
pub enum MultiTurnStreamItem {
    /// A provider stream item: part starts/ends, text and reasoning
    /// fragments, tool-call parts (arguments + the validated call's end),
    /// and unmodeled passthrough payloads.
    StreamAssistantItem(rig_core::streaming::Item<StreamEvent>),
    /// A tool call the model emitted, reported when the turn commits, for
    /// each call Rig routes to execution (hook-skipped calls still report).
    ToolCall { tool_call: rig_core::message::ToolCall },
    /// Rig executed and committed a tool call (with hook patches applied);
    /// surfaced together with its result after the whole batch settles.
    ToolExecutionCommitted { tool_call: rig_core::message::ToolCall },
    /// The result of an executed (or hook-skipped) tool call.
    StreamUserItem(rig_core::streaming::StreamedUserContent),
    /// One finished completion request: `call_index`, `usage`, ids,
    /// `finish_reason`, `raw`.
    CompletionCall(CompletionCall),
    /// A hook rejected the completed turn for retry; discard the provisional
    /// text/reasoning you rendered for `turn`.
    ModelTurnRetried { turn: usize },
    /// The run's final response (shared type with the blocking surface).
    FinalResponse(PromptResponse),
}
```

- `CompletionCall` items carry per-request `usage` — forward these to a usage tracker as they arrive; they also let you anchor context-occupancy displays mid-run.
- `ToolExecutionCommitted` confirms a tool actually ran (as opposed to being hook-skipped or resolved by invalid-call recovery); correlate with its result via `tool_call.id`.
- `FinalResponse` is the stream-side counterpart of the `on_run_settled` hook's success outcome; errors surface as the stream's `Err` item instead.

## Assistant part events (`rig_core::streaming`)

`StreamAssistantItem` unfolds as `Item::Event(StreamEvent)`:

```rust
pub enum StreamEvent {
    Start  { part: Part, kind: PartKind },           // PartKind::{Text, Reasoning, ToolCall, Image}
    Text       { part: Part, text: String },         // text fragment
    Reasoning  { part: Part, text: String },         // reasoning fragment
    Arguments  { part: Part, json: String },         // a tool call's raw arguments
    End        { part: Part, content: AssistantContent }, // the finalized content
}
pub enum Item<E> { Event(E), Unknown(UnknownPayload) } // Unknown: provider-native passthrough
```

- Parts are identified by position (`Part::index()`); each starts once, grows (own fragment kind) and ends once. `Item::Unknown` payloads always reach the consumer.
- **Text fragments** render as deltas; the `End` text part duplicates the fragments (aggregation belongs to the consumer — rig's own `stream_to_stdout` prints fragments and only the final reasoning `End`).
- **Tool-call parts**: the `End { content: ToolCall }` event is what the agent holds until the call is validated/patched, re-emitting it with the *effective* (patch-repaired) call; `MultiTurnStreamItem::ToolCall` reports the same calls at commit. Match the `ToolCall` item for tool-start UI; treat part `Arguments`/`End` as transcript-level detail (shuvarie's mapper ignores them to avoid double-counting).
- **Reasoning**: fragments stream on `Reasoning` events; a final `End { content: AssistantContent::Reasoning(sealed) }` carries the complete block, sealed to its issuer. Open it for display with `sealed.open(sealed.issuer())` — a value opened by its own issuer always succeeds — and read `display_text()`.

## Results and correlation

**`StreamedUserContent`** (`rig_core::streaming`) — `ToolResult { tool_result: ToolResult }`, the only variant. Correlate everything through **`CallId`**:

- `MultiTurnStreamItem::ToolCall { tool_call }` → `tool_call.id: CallId`
- `StreamedUserContent::ToolResult { tool_result }` → `tool_result.call: CallId` (the answered call's id)
- `MultiTurnStreamItem::ToolExecutionCommitted { tool_call }` → same id
- hooks: `OutcomeEvent.call_id` → the same id

`CallId::wire()` / `Display` renders the provider's id (or the rig-issued UUID when the provider sent none); tool-call ids hash/compare by value, so they key correlation maps directly. A `ToolResult`'s `name` is the *executed* tool's name — which can differ from the model's call when a hook repaired it.

> **Per-batch buffering**: rig surfaces the batch's `ToolExecutionCommitted` + `StreamUserItem` results only after **every** tool call of the batch settles (in call order), so a fast call's result is not surfaced while a slow sibling still runs. Shuvarie works around this per-call: the `FileChangeHook`'s early-finish channel (`shuvarie-llm`'s `with_early_finish`) surfaces each result the moment its call resolves (via the `on_outcome` hook), and `provider.rs::map_agent_stream` drops the later buffered duplicates via `FileChangeHook::surfaced_early`.

## Errors

```rust
pub enum StreamingError {
    Completion(ProviderError),    // the provider stream failed
    Report(ErrorReport),          // structured failure from the bus/handler/hook/stream item
    Prompt(PromptError),          // same failure shape as the blocking surface (MaxTurnsError, PromptCancelled { chat_history, reason }, ...)
}
```

A memory-load failure is the stream's first (and only) item; a hook stop yields `Prompt(PromptError::PromptCancelled { .. })` whose display carries the stop reason. Dropping the unpolled stream runs nothing ([`must_use`]).

## Streaming to stdout

```rust
use rig_agent::agent::stream_to_stdout;
let mut stream = agent.prompt("Hello!").stream();
let response = stream_to_stdout(&mut stream).await?;
```

`stream_to_stdout` prints text fragments as they arrive, prints a reasoning part when its `End` arrives (via the issuer self-open), prints a visible boundary for retried model turns, returns the final `PromptResponse`, and maps `StreamingError` to `std::io::Error`.

## Practical notes

- **Handle errors per item.** Building the stream always succeeds; each item is a `Result` — match on `item?` rather than assuming atomic success/failure.
- **Apply backpressure** with standard stream backpressure when the consumer can't keep up.
- **Read usage at the end** — `FinalResponse.usage` aggregates across the run (`Option<u64>` counters; an unreported counter is `None`); `CompletionCall.usage` is per model request.
- **Bounded event feeds** — for hosts with their own executor, `agent.prompt(..).run_channel()` splits the run into a future plus a bounded `RunEvents` feed instead of an unbounded stream.

## Project boundary (shuvarie)

Run `.stream()` on the **core task**, not the TUI thread. Forward mapped items (`shuvarie-llm::StreamItem`) to the TUI via the existing `tokio::sync::mpsc` channel; the TUI's `update` consumes them and `view` renders current state. Never pull a stream from `handle_event`/`view` — those must not `await` (see `AGENTS.md` and the `ratatui` skill). The mapping lives in `crates/llm/src/provider.rs` (`map_agent_stream`): text/reasoning fragments, `MultiTurnStreamItem::ToolCall` → tool/worker starts (worker names pre-queued for result correlation), `StreamUserItem` results (skipping early-surfaced ones), `CompletionCall` → usage, `FinalResponse` → `Done`, and error classification (`retry.rs`) mapping overflow/connection failures onto shuvarie's retry flow.