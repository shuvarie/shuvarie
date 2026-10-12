# Streaming

Streaming processes an LLM response incrementally as it's generated, essential for responsive UIs and long-form output. The streaming surface is a **runner mode**: `agent.prompt(...)` builds an `AgentRunner`; driving it with `.stream()` yields one lazy, `Send` stream of `MultiTurnStreamItem`s (identical loop, hooks and history as `.run()`; only the deltas differ). The 0.42 `Prompt`/`Chat`/`StreamingPrompt`/`StreamingChat` traits are gone.

Official docs: https://rig.rs/docs/concepts/streaming · API: https://docs.rs/rig-agent/latest/rig_agent/agent/struct.AgentRunner.html (authoritative reference: `rig-agent-0.44.0/src/agent/streaming.rs` + `rig-core-0.44.0/src/streaming/`).

> **Changed in 0.44** — one error type for a run: the stream yields `Result<MultiTurnStreamItem, PromptError>`; **`StreamingError` is gone**. **`StreamUserItem`/`StreamedUserContent` are gone**: results arrive as `MultiTurnStreamItem::ToolResult { tool_result }`, a plain `ToolResult`. Tool-call arguments now stream as they arrive (`StreamEvent::Arguments`, `PartKind::Opaque`), and reasoning is a plain `Reasoning` — `Sealed`/`Issuer` no longer exist. `MultiTurnStreamItem` and `StreamEvent` serialize/deserialize again.

## Streaming an agent

```rust
use futures::StreamExt;
use rig_agent::agent::MultiTurnStreamItem;
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
            MultiTurnStreamItem::FinalResponse(res) => println!("{}", res.output()),
            _ => {}
        }
    }
    Ok(())
}
```

The stream's type is `StreamingResult = WasmBoxedStream<'static, Result<MultiTurnStreamItem, PromptError>>`; the last item is the `FinalResponse` (or an `Err`). It runs under the span it was *built* in (whatever ambient span the builder saw, else a fresh `invoke_agent`). Hooks fire at every observable point, including streamed text and tool-call deltas.

## The run stream: `MultiTurnStreamItem` (`rig_agent::agent`)

```rust
pub enum MultiTurnStreamItem {                 // #[non_exhaustive]
    /// A provider stream item: part starts/ends, text and reasoning fragments,
    /// a tool call's argument fragments as they stream (a call to a tool the
    /// turn does not allow is held for its resolution), and unmodeled
    /// passthrough payloads. A call's end carries the validated call.
    StreamAssistantItem(Item<StreamEvent>),
    /// A tool call the model emitted, reported when the turn commits, for each
    /// call Rig routes to execution (a hook skip still reports it).
    ToolCall { tool_call: rig_core::message::ToolCall },
    /// Rig executed and committed a call (with hook patches applied); surfaced
    /// together with its result only after the whole batch settles.
    ToolExecutionCommitted { tool_call: rig_core::message::ToolCall },
    /// The result of an executed (or hook-skipped) tool call.
    ToolResult { tool_result: rig_core::message::ToolResult },
    /// One finished completion request: `call_index`, `usage`, ids,
    /// `finish_reason`, `raw`.
    CompletionCall(CompletionCall),
    /// A hook rejected the completed turn for retry; discard the provisional
    /// text/reasoning you rendered for `turn` (one-based model-call index).
    ModelTurnRetried { turn: usize },
    /// The run's final response (shared type with the blocking surface).
    FinalResponse(PromptResponse),
}
```

- `ToolCall` is a **model-emission** item, not an execution-lifecycle event: it is surfaced when the model turn commits, for every call Rig routes to execution, whether or not the tool body ultimately runs. Two kinds of model call are *not* reported here: a call rejected and handled by invalid-tool-call recovery (surfaced through that recovery path), and a Tool-mode structured-output call, which finalizes the run directly — its structured result arrives on `FinalResponse` instead of as a completed call.
- `ToolExecutionCommitted` confirms a tool body **actually ran** (it passed its `ToolCall` hook checks) and carries the call **as executed** — the model's call with any `DispatchAction::Patch` rewrite applied. It is never emitted for a call dropped by a sibling's termination, skipped by a hook, or resolved by invalid-call recovery; the model's *original* call is the `StreamAssistantItem` `End` part.
- `ToolResult` is the batch's atomic surface: the batch commits at every `tool_concurrency` (including the sequential default), so **results are surfaced in call order only after the whole batch settles successfully**. A run that terminates mid–batch surfaces no successful tool results.
- `CompletionCall` carries per-request `usage` (the provider's final usage for that request, not per streamed token): forward these to a usage tracker as they arrive — they also let you anchor context-occupancy displays mid-run.
- `FinalResponse` is the stream-side counterpart of the `on_run_settled` hook's success outcome; error termination surfaces as the stream's `Err` item instead. Both are terminal: no retry, further turn or tool execution follows.

## Assistant part events (`rig_core::streaming`)

`StreamAssistantItem` unfolds as `Item::Event(StreamEvent)`:

```rust
pub enum StreamEvent {                                  // tag = "event"
    Start { part: Part, kind: PartKind, name: Option<ToolName> },
    Text       { part: Part, text: String },            // text fragment
    Reasoning  { part: Part, text: String },            // reasoning fragment
    Arguments  { part: Part, json: String },            // a call's raw argument JSON fragment
    End        { part: Part, content: AssistantContent }, // the finalized content
}
pub enum PartKind { Text, Reasoning, ToolCall, Image, Opaque } // Opaque: no canonical meaning
pub enum Item<E> { Event(E), Unknown(UnknownPayload) }  // Unknown: provider-native passthrough
```

- Parts are identified by position (`Part::index()`); each starts once, grows (own fragment kind), and ends once. A tool call opens only once its tool is named, so a call streams its arguments under a known tool: `Start.name` is that `ToolName` (and `None` for every other `PartKind`). `Item::Unknown` payloads always reach the consumer; `UnknownPayload::value()` opts in to the raw JSON (its `Debug` shows size only).
- **Text**: render fragments as deltas. The `End` text part duplicates them, so aggregation belongs to the consumer — `stream_to_stdout` prints fragments only. **Do not count `End` parts as extra text.**
- **Tool-call arguments stream in 0.44**: the `Arguments` fragments of one part, joined, are the argument JSON the provider sent; read an incomplete prefix for display with `parse_partial_arguments(text) -> serde_json::Map<String, Value>` (lossy, never fails — for showing a call while it streams, never for executing it). The `End { content: AssistantContent::ToolCall(call) }` carries the call **as validated** (a hook-repaired name applied), and `MultiTurnStreamItem::ToolCall` is the commit item: match that for tool-start UI and treat part `Arguments`/`End` as transcript-level detail (shuvarie's mapper ignores them to avoid double-counting).
- **Reasoning**: fragments stream on `Reasoning` events; the final `End { content: AssistantContent::Reasoning(Reasoning) }` carries the complete block. `Sealed`/`Issuer` are gone — `Reasoning { text, redacted, native }` is plain, read `text` directly (`redacted` marks a withheld block). The `End` text duplicates the fragments.
- **Transcripts**: `Transcript::{parse, parse_prefix, push, from_items}` check a serialized item sequence (each part starts once, at a fresh position, grows only while open and with its own kind, ends once) and report `SequenceError::{Shape, UnknownPart, EndedTwice, WrongKind, Unclosed}`; `items()`/`events()` read it back. `streaming::delivered(&[Item<StreamEvent>]) -> Vec<AssistantContent>` folds an item slice into the content it delivered: every ended part in start order plus the text of a text/reasoning part still open.

## Results and correlation

A result is a plain `rig_core::message::ToolResult { call: CallId, name: ToolName, content: Vec<ToolResultContent>, is_error: bool }`. Correlate everything through **`CallId`**:

- `MultiTurnStreamItem::ToolCall { tool_call }` → `tool_call.id: CallId`
- `MultiTurnStreamItem::ToolResult { tool_result }` → `tool_result.call: CallId` (the answered call's id)
- `MultiTurnStreamItem::ToolExecutionCommitted { tool_call }` → same id
- hooks: `OutcomeEvent.call_id` → the same id
- `AssistantContent::ToolCall(call)` (the `End` part) → same id

`CallId` is `Provider(ProviderCallId) | Local(LocalCallId)`; `CallId::wire()` / `Display` renders the provider's id (or the rig-issued UUID when the provider sent none), `provider()`/`is_local()` disclose which. Tool-call ids hash/compare by value, so they key correlation maps directly. A `ToolResult`'s `name: ToolName` is the *executed* tool's name — which can differ from the model's call when a hook repaired it — and `is_error` marks a tool that failed, was refused, or never ran.

> **Per-batch buffering**: rig surfaces the batch's `ToolExecutionCommitted` + `ToolResult` items only after **every** tool call of the batch settles (in call order), so a fast call's result is not surfaced while a slow sibling still runs. Shuvarie works around this per-call: the `FileChangeHook`'s early-finish channel (`shuvarie-llm`'s `with_early_finish`) surfaces each result the moment its call resolves (via the `on_outcome` hook), and `provider.rs::map_agent_stream` drops the later buffered duplicates via `FileChangeHook::surfaced_early`. Use tool hooks for live host-side start/result observation.

## Errors

```rust
pub enum PromptError {                        // #[non_exhaustive]
    Provider(ProviderError),                  // the provider call/stream failed
    Report(ErrorReport),                      // bus/handler/hook/stream-item failure
    Memory(MemoryError),                      // memory failed to load or persist history
    MaxTurns { max_turns: usize, chat_history: Vec<Message>, prompt: Message },
    Cancelled { chat_history: Vec<Message>, reason: String },
    UnknownToolCall { tool_name: String, available_tools: Vec<String>,
                      allowed_tools: Vec<String>, chat_history: Vec<Message> },
}
```

**`StreamingError` is gone**: awaited and streamed runs share one error type, `rig_agent::run::PromptError` (`MaxTurnsError` → `MaxTurns`, `PromptCancelled` → `Cancelled`, `CompletionError` → `Provider`). A memory-load failure is the stream's first (and only) item; a hook stop yields `PromptError::Cancelled { .. }` whose display carries the stop reason. Dropping the unpolled stream runs nothing (`#[must_use]`).

## Streaming to stdout

```rust
use rig_agent::agent::{stream_to_stdout, StreamToStdoutError};
let mut stream = agent.prompt("Hello!").stream();
let response = stream_to_stdout(&mut stream).await?;   // -> Result<PromptResponse, StreamToStdoutError>
```

`stream_to_stdout` prints text fragments as they arrive, prints a reasoning part when its `End` arrives, prints a visible boundary for retried model turns, returns the final `PromptResponse`, and fails with `StreamToStdoutError::{Run(PromptError), Io(std::io::Error), Incomplete}` (`Incomplete`: the stream ended without a final response). Metadata items (`CompletionCall`) are never printed — read them off the response.

## Bounded event feeds: `run_channel`

| Item | Home | Notes |
|------|------|-------|
| `AgentRunner::stream(self) -> StreamingResult` | `AgentRunner` | lazy, `Send` on native targets, `#[must_use]` |
| `AgentRunner::run_channel(self)` | `AgentRunner` | `(impl Future<Output = Result<PromptResponse, PromptError>>, RunEvents)`, `#[must_use]` |
| `RUN_EVENTS_CAPACITY: usize = 32` | `rig_agent::agent` | capacity of the feed's queue |
| `RunEvents::try_next() -> Option<MultiTurnStreamItem>` | `RunEvents` | non-blocking; `None` means "nothing queued *or* drained" |
| `RunEvents::is_done() -> bool` | `RunEvents` | tells those two apart |
| `impl Stream<Item = MultiTurnStreamItem> for RunEvents` + `FusedStream` | `RunEvents` | note: the feed yields **plain items**, not `Result` — errors terminate the future |

The feed is a bounded queue: a slow consumer applies **back-pressure** to the run instead of losing events. Dropping the feed does not cancel the run; the future still resolves with the final `PromptResponse`. The run belongs to the span `run_channel` was called in, so spawn the future wherever the feed is consumed.

## Driver-level streams (`rig_core`)

`Model::stream(request) -> Result<Streamed<W::Op>, ProviderError>` opens a streamed reply for **any** operation (encoding errors return there; every later failure arrives in-band as the last item); `CompletionStream = Streamed<Completion>` is the streamed completion.

```rust
impl<Op: Operation> Streamed<Op> {
    pub async fn finish(self) -> Result<Op::Response, ProviderError>; // Model::call, finished
}
impl Streamed<Completion> {
    pub fn partial(&self) -> CompletionResponse;      // no `call` method: Model::call is the folded call
    pub fn relay(label, events: StreamEvents) -> Self;
    pub fn into_relay(self) -> StreamEvents;
}
```

| Item | Meaning |
|------|---------|
| `Streamed<Op>: Stream<Item = Result<Item<Op::Event>, ProviderError>>` | the events, then the error that ends the stream; stop polling to pause, drop it to cancel |
| `Streamed::finish()` | reads the rest of the reply and folds it with the provider's end — the same response `Model::call` returns; a reply the provider never ended is `ProviderError::Truncated` |
| `Streamed::partial()` | what arrived so far (ended parts in start order + the text of an open part); valid after an error and after the caller stopped polling |
| `Relayed::{Origin(Origin), Item(Item<StreamEvent>), Done(Box<CompletionResponse>)}` / `StreamEvents = WasmBoxedStream<'static, Result<Relayed, ErrorReport>>` | the bus-shaped relay: who the reply is from, its items, then the folded response (or the error) |
| `UnknownPayload` | an unmodeled JSON payload (`new`, `value()`; redacted `Debug`) — an `Item::Unknown` always reaches the consumer |
| `parse_partial_arguments(text)` | the best object a call's argument text states so far, repaired; display only |
| `streaming::delivered(&items)` | the assistant content a run of items delivered |

## Streamed-turn assembly (`rig_agent::run::streamed`)

The accumulators for the streaming path are methods on **`StreamedTurnAssembler`** (the sans-IO assembler a driver feeds), not on `AgentRunner`:

| Call | Returns |
|------|---------|
| `new(executable_tool_names, allowed_tool_names)` | an assembler for one streamed turn |
| `ingest(&Item<StreamEvent>)` | `Result<Vec<StreamedTurnEvent>, ProviderError>`: forward the item, forward a tool-call delta, hold a disallowed call, emit the validated call at its end, or surface an invalid call |
| `aggregated_text()` / `aggregated_reasoning(index)` | the turn's text / the reasoning of the part at `index` so far |
| `aggregated_arguments(index)` | the argument text the streaming call at `index` has sent so far |
| `streaming_tool_name(index)` | the tool that call named when it started |
| `excluded_assistant_content()` | replayed assistant blocks excluded from assembly (non-zero means transcript content was lost) |
| `resolve_pending_invalid(&StreamedResolution)` | forwards a repaired call under its new name; a skip/ignore adds nothing |
| `partial_turn(&CompletionResponse)` / `finish(&CompletionResponse)` | the partial turn for diagnostics / the assembled `StreamedTurn` for `AgentRun::streamed_turn` |

## Practical notes

- **Handle errors per item.** Building the stream always succeeds; each item is a `Result` — match on `item?` rather than assuming atomic success/failure.
- **Apply backpressure** with standard stream backpressure when the consumer can't keep up (or use `run_channel`'s bounded feed).
- **Read usage at the end** — `FinalResponse.usage` / `PromptResponse::usage()` aggregates across the run (`Option<u64>` counters; an unreported counter is `None`); `CompletionCall.usage` is per model request and `PromptResponse::requests()` counts them.
- **Don't double-count**: text `End` parts duplicate the text fragments, reasoning `End` parts duplicate the reasoning fragments, and the tool-call `Arguments`/`End` parts duplicate the `ToolCall` commit item.
- **`tool_concurrency(n)`** (runner setter, `1`–sequential by default) bounds concurrent tool bodies; committed stream order stays call order.

## Project boundary (shuvarie)

Run `.stream()` on the **core task**, never on the TUI thread: all side effects happen in `update` and `view` is pure (`AGENTS.md`), the TUI loop must not `await` (the `ratatui` skill), and streaming tokens belong on the core task (this skill's rules). Forward mapped items (`shuvarie-llm::StreamItem`, `StreamStream` in `crates/llm/src/stream.rs`) to the TUI over the existing `tokio::sync::mpsc` channel; the TUI's `update` consumes them and `view` renders current state. Never pull a stream from `handle_event`/`view`.

The mapping lives in `crates/llm/src/provider.rs` (`map_agent_stream`, and the worker-run twin in `run_worker_agent`): `StreamEvent::Text`/`Reasoning` fragments → `StreamItem::Delta`/`Reasoning` (a `TurnText` accumulator inserts the paragraph break at tool gaps), `MultiTurnStreamItem::ToolCall` → `ToolStart`/`WorkerStart` with `tool_call.function.arguments_value()` (worker names pre-queued for result correlation), `ToolResult` → `ToolResult`/`WorkerResult` (skipping the calls `FileChangeHook::surfaced_early` already delivered, `ToolResultContent::as_text` joined), `CompletionCall` → `Usage`, `FinalResponse` → `Done { text, usage }`, and the error item classified (`crates/llm/src/retry.rs`: `is_context_length_error`/`OVERFLOW_REASON` → `StreamItem::Overflow`, else `classify_connection_error` → `StreamItem::Error { reason }`) onto shuvarie's retry flow. Every other item (including `ToolExecutionCommitted` and `ModelTurnRetried`) falls through the catch-all and is ignored.
