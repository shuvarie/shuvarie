# Memory

Memory is an agent's ability to retain and reuse information from earlier in a conversation (and across conversations). Without it, every prompt starts from scratch. Rig gives you both ends of the spectrum — attach a conversation-memory backend and history is loaded/appended for you, or own the `Vec<Message>` yourself and decide exactly what the model sees.

Official docs: https://rig.rs/docs/concepts/memory · API: https://docs.rs/rig-core/latest/rig_core/memory/index.html — the contracts live in `rig_core::memory` (`rig-core-0.44.0/src/memory.rs`). Reusable truncation/summarization policies live in the separate **`rig-memory` companion crate**, not in rig-core.

Two layers:
- **Conversation history** (short-term) — messages of the current conversation, passed back each turn and bounded so it doesn't outgrow the context window.
- **Long-term memory** — facts/observations/user profiles surviving across sessions, usually in a DB or vector store.

## Automatic conversation memory

Give the agent a memory backend + conversation id and Rig loads stored history before each prompt and appends the committed turn (the prompt, the assistant response, and tool-call/result pairs) after the run succeeds:

```rust
use rig_agent::AgentBuilder;
use rig_core::memory::InMemoryConversationMemory;

let agent = AgentBuilder::new(model)
    .preamble("You are a helpful assistant.")
    .memory(InMemoryConversationMemory::new())
    .conversation("user-42")            // the conversation id (a ConversationId)
    .build();

let _ = agent.prompt("My name is Ada.").await?;
let reply = agent.prompt("What's my name?").await?;
println!("{}", reply.output());        // `output()` is a method
```

Rules:
- The conversation id can be a builder default (`AgentBuilder::conversation(id)`) or a runner override (`.conversation(id)`). No id → no memory.
- `memory(backend)` takes any `B: ConversationMemory + 'static` and registers it as the agent's memory handler (`MemoryAdapter` under the key `<owner>/memory`); `memory_handler(handler)` registers any memory-family `Serve` instead — a host can serve several backends and dispatch to each by `memory:<label>`. A `.conversation(id)` without either warns at build and does nothing.
- Passing explicit history with `.history(iter)` **bypasses memory entirely** for that run — nothing loaded, nothing appended (the committed messages return on `PromptResponse::messages()` for caller-owned persistence).
- `.without_memory()` clears both the memory key and the conversation id for a single run (no load, no save).
- A failed load fails the run with `PromptError::Memory` before any model call; a failed append is **acknowledged** on `PromptResponse::memory_append` without invalidating the answer.

### What the run hands back

| Item | Notes |
|------|-------|
| `PromptResponse::messages` | `Vec<Message>` — the run's transcript: the prompt, every accepted assistant turn, every committed tool result and any corrective feedback, *excluding* the input history. This, and only this, is what memory is asked to persist. |
| `PromptResponse::messages()` | the same transcript by reference |
| `PromptResponse::memory_append` | `Option<MemoryAppend>`, set by the driver once the append dispatch resolved |

`MemoryAppend` is the append's outcome, independent of the (still valid) answer. It serializes tagged as `{"status":"acknowledged"}` / `{"status":"failed",…}`:

| Variant | Meaning |
|---------|---------|
| `Acknowledged` | the backend acknowledged the append of `messages` |
| `Failed { report: ErrorReport }` | the append dispatch failed or was refused, as reported after outcome hooks — **persistence may still have occurred** |

Helpers: `is_acknowledged()`, `failure() -> Option<&ErrorReport>`. The source's own caveats: acknowledgement gives only the *backend's* durability guarantee; failure does not prove no write occurred; dropping an in-flight append yields no response; none of this is exactly-once. `memory_append` is `None` when memory was not configured for the run, bypassed by explicit history, disabled — **or the run was resumed**.

> **Resumed runs.** `Agent::resume(run)` continues a persisted run: its prompt and history are already inside the run, so **nothing is loaded**. The sources disagree about the append — `Agent::resume`'s docs and `PromptResponse::memory_append`'s docs both say a resumed run neither loads nor appends (the driver that persisted the run owns persistence, so `memory_append` is `None`), while `AgentRunner::resumed_memory` resolves a memory handle for a resumed run and says the messages it adds "are appended once, at its end" — which is what the engine's `append_run_messages` does with any handle it is handed. Treat a resumed run's append as unspecified: persist the finished run's `messages` yourself and never depend on the append or on `memory_append` there. `history`, `max_turns` and `unhandled_invalid_tool_call` on a resumed runner are inert for the same reason — the run carries its own.

`InMemoryConversationMemory` lives in process, protected by a `std::sync::Mutex<HashMap<ConversationId, Vec<Message>>>`, and forgets on restart. It is `Clone` + `Default`; `new()` builds the empty store, `with_filter(f)` replaces the load-time view, and a poisoned lock surfaces as `MemoryError::Internal`. For durable sessions, implement `ConversationMemory` over your own store:

```rust
pub trait ConversationMemory: WasmCompatSend + WasmCompatSync {
    fn load<'a>(&'a self, conversation_id: &'a ConversationId)
        -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>>;
    fn append<'a>(&'a self, conversation_id: &'a ConversationId, messages: Vec<Message>)
        -> WasmBoxedFuture<'a, Result<(), MemoryError>>;
    fn clear<'a>(&'a self, conversation_id: &'a ConversationId)
        -> WasmBoxedFuture<'a, Result<(), MemoryError>>;
}
```

| Method | Contract |
|--------|----------|
| `load` | the full history for the id, or an empty `Vec` when nothing is stored; called before the first model call |
| `append` | called after a successful turn with the prompt, the assistant response and the tool-call/tool-result pairs; runs inline before the agent returns. Writes are **not** transactional or exactly-once — an error may arrive after the backend already persisted |
| `clear` | remove every stored message for the id (`MemoryOp::Clear`); the agent itself never calls it — a host or tool does |

`MemoryError`:

| Variant | Meaning |
|---------|---------|
| `Backend(MemoryBackendError)` | the store failed to load, append or clear. `MemoryBackendError` is the cfg-split boxed error alias (`Box<dyn std::error::Error + Send + Sync + 'static>`, without `Send + Sync` on wasm); `MemoryError::backend(e)` wraps it |
| `Policy(String)` | a history-shaping filter or policy rejected the loaded history |
| `Internal(String)` | an invariant broke — a poisoned in-process lock, an unreachable handler, a report of another family |

`Arc<M>` and `Box<M>` forward `ConversationMemory` in rig-core (`Arc<M>` likewise for `DemotionHook` and `Compactor`), so an adapter can hold either a concrete backend or a shared trait object.

Messages are `Serialize`/`Deserialize` — persisting a conversation is ordinary serde work (a JSON column per conversation id is fine). Keep `append` cheap: it runs inline before the agent returns.

> **Memory is a bus effect, not a special case.** Load and append are `EffectKind::Memory { op: MemoryOp::Load { conversation } | MemoryOp::Append { conversation, messages } | MemoryOp::Clear { .. } }` answered with `MemoryOutcome::{Loaded { messages }, Appended, Cleared}`, so every load and append is a dispatch that hooks can observe, record and replay. A failed dispatch is mapped back for you: `ErrorKind::MemoryPolicy` (and a layer `Denied`) → `MemoryError::Policy`, `ErrorKind::MemoryBackend` → `MemoryError::Backend`, anything else → `MemoryError::Internal`.

## Bounding history with filters and policies

Raw history grows without bound — long histories are worse than just expensive (models get distracted; context windows overflow). The fix is managed forgetting: shape what `load` returns.

`InMemoryConversationMemory::with_filter(f)` takes any **closing-over closure** `Fn(Vec<Message>) -> Vec<Message>` (a `MessageFilter`): truncation, re-ordering, token-based windows — implement the policy you need:

```rust
use rig_core::memory::InMemoryConversationMemory;

let memory = InMemoryConversationMemory::new().with_filter(|messages| {
    messages.last_chat_window(20)   // your windowing policy
});
```

The filter runs on every load, after the store lock is released, and never modifies stored messages: it is a *view*, not a write, so evicted messages stay in the store unless a `DemotionHook` or `Compactor` takes them somewhere.

For richer managed-forgetting flows, rig-core also ships the composing contracts:

- **`MessageFilter`** — `Fn(Vec<Message>) -> Vec<Message> + WasmCompatSend + WasmCompatSync`, auto-implemented for any closure with that shape, applied on every load (drop-oldest windows, token budgeting).
- **`DemotionHook`** — `on_demote<'a>(&'a self, conversation_id: &'a ConversationId, messages: Vec<Message>) -> WasmBoxedFuture<'a, Result<(), MemoryError>>`: receives the messages evicted from the active window, in original conversation order (archive to a long-tail store). Awaited inline — keep it fast. Implementations must be idempotent on `(conversation_id, messages)`: delivery watermarks are not persisted, so a restart or a newly built adapter can redeliver, and a durable hook should deduplicate by a stable key such as conversation id + content hash. `NoopDemotionHook` is the do-nothing default for adapters that require a hook value.
- **`Compactor`** — `compact<'a>(&'a self, conversation_id: &'a ConversationId, evicted: &'a [Message], carry_over: Option<&'a Self::Artifact>) -> WasmBoxedFuture<'a, Result<Self::Artifact, MemoryError>>` where `Artifact: Into<Message> + Clone + WasmCompatSend + WasmCompatSync + 'static`: replaces evicted messages with a summary artifact spliced back into history (rolling-summary pattern; `carry_over` folds the previous summary in, or ignore it to summarize only what was newly evicted). Runs inline on the load path — a slow compactor delays the agent's next turn. Errors are propagated unchanged by the composing adapter, so pick the variant that describes the failure (`Backend` for I/O or remote-LLM faults, `Policy`/`Internal` otherwise).

> **Named policies are not in this crate.** Ready-made filters, and the demotion/compaction adapters that wire a filter, a hook and a compactor together, ship in the **`rig-memory` companion crate** — a separate published crate from rig-core, with its own policy and error types. `rig_core::memory` gives you the contracts (and the closure route above); reach for `rig-memory` for the batteries.

## Managing history by hand

Own the history yourself for custom storage/shaping. History is a `Vec<Message>` (message content is a plain `Vec<T>` — `OneOrMany` was removed):

```rust
use rig_core::message::{AssistantContent, AssistantMessage, Message, Text, UserContent};

let mut conversation_history: Vec<Message> = Vec::new();
conversation_history.push(Message::User {
    content: vec![UserContent::Text(Text::new("Do you know the weather today?"))],
});
// `AssistantMessage` is #[non_exhaustive]: build the turn through its constructors.
conversation_history.push(Message::Assistant(AssistantMessage::new(vec![
    AssistantContent::text("I don't have real-time data..."),
])));
```

Or use the constructors for the common text-only case (`Message::system`, `Message::user`, `Message::assistant`, `Message::tool_result`, `Message::tool_results`):

```rust
history.push(Message::user("What is Rust?"));
history.push(Message::assistant("A systems programming language..."));
```

> **`.history(..)` on a runner does not append** the new user message or response to the history you pass (and it bypasses memory). Record the new turn yourself — the messages the run committed arrive on `PromptResponse::messages()`, a `Vec<Message>` (no longer an `Option`). (`chat(prompt, &mut history)` is the exception — it **does** append the committed turn, including tool calls, so don't push messages again.)

```rust
use rig_agent::AgentBuilder;
use rig_core::message::Message;

async fn call_agent_with_chat_history(
    model: rig_core::DynModel<rig_core::operation::Completion>,
    prompt: &str,
    history: &mut Vec<Message>,
) -> Result<String, Box<dyn std::error::Error>> {
    let agent = AgentBuilder::new(model)
        .preamble("You are a helpful assistant. Be concise.")
        .name("Bob")
        .build();

    let response = agent.prompt(prompt).history(history.iter()).await?;
    history.push(Message::user(prompt));
    history.extend(response.messages.iter().cloned());
    Ok(response.output())
}
```

Interactive REPL example: https://rig.rs/docs/guides/cli_chatbot

### Rolling your own compaction

Once hand-managed history grows past a threshold, ask the model for a summary and start a fresh history seeded with it:

```rust
use rig_core::completion::{CompletionRequest, Message};
use rig_core::driver::DynModel;
use rig_core::message::{AssistantContent, UserContent};

async fn compact_history(
    model: DynModel<rig_core::operation::Completion>,
    history: &[Message],
) -> Result<Vec<Message>, Box<dyn std::error::Error>> {
    let transcript = history
        .iter()
        .filter_map(|msg| match msg {
            Message::User { content } => content.iter().find_map(|c| match c {
                UserContent::Text(text) => Some(format!("User: {}", text.text)),
                _ => None,
            }),
            Message::Assistant(turn) => turn.content.iter().find_map(|c| match c {
                AssistantContent::Text(text) => Some(format!("Assistant: {}", text.text)),
                _ => None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let summary_prompt =
        format!("Provide a concise summary of the following conversation:\n\n{transcript}");
    let response = model.call(CompletionRequest::new(summary_prompt)).await?;
    let summary = response.text();     // the text parts of `response.choice`, concatenated
    Ok(vec![Message::user(format!("Context from previous conversation:\n{summary}"))])
}
```

Trigger compaction after every turn, on a schedule, or once a token budget is exceeded (needs a tokenizer — shuvarie's own compaction layer, `crates/core/src/compaction.rs`, drives this from persisted usage).

## Long-term memory

Bounded history keeps a single conversation healthy; many apps need memory surviving across sessions. Common strategies:

- **Conversation observations** — insights extracted from an exchange (decisions, open questions, strong interests).
- **User observations / profile** — persistent facts about the user (preferences, location, communication style). Keep separate from conversation history; update incrementally; re-verify before use.
- **Grounded facts** — objectively verifiable data pulled from external sources during a session (retrieved docs, computed results, API responses), stored with source + timestamp.

The mechanics are the same for all three: after a significant exchange, use an `Extractor` (see `structured-output.md`) or a plain prompt to distill the relevant info, then persist it. When a new conversation starts, retrieve the most relevant items and add them to the system prompt or opening messages. For semantic retrieval, use a vector store (see `rag-and-vector-stores.md`). A `DemotionHook` (above) is a natural place to feed evicted conversation turns into such a store.
