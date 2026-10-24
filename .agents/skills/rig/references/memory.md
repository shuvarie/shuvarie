# Memory

Memory is an agent's ability to retain and reuse information from earlier in a conversation (and across conversations). Without it, every prompt starts from scratch. Rig gives you both ends of the spectrum — attach a store and history is loaded/appended for you, or own the `Vec<Message>` yourself and decide exactly what the model sees.

Official docs: https://rig.rs/docs/concepts/memory · API: https://docs.rs/rig-memory/latest/rig_memory/ — **changed in 0.45**: `rig_core::memory` is gone. Conversations over time live in the **`rig-memory` crate** (sessions, compaction, stores), over the `rig_core::conversation` model; `AgentBuilder::memory` takes a **store**, not a `ConversationMemory` trait object.

> **Migrating from 0.44:** `ConversationMemory` / `InMemoryConversationMemory` / `MessageFilter` / `DemotionHook` / `Compactor` / `MemoryError` / `MemoryAppend` no longer exist. `AgentBuilder::memory(impl rig_memory::AsyncStore)` + `AgentBuilder::conversation(log)` replaces them: a *log name* (a `&str`) replaces `ConversationId`, `rig_memory::MemoryStore` replaces `InMemoryConversationMemory`, `rig_memory::DirStore` (feature `fs`) is the durable file store, `recent::{Recent, LastMessages, TokenBudget, view}` replaces `MessageFilter`, and `CompactionPolicy`/`apply_summary`/`undo_summary` replace `Compactor`. `PromptResponse::memory_append` is gone too — the writer owns persistence.

Two layers:
- **Conversation history** (short-term) — messages of the current conversation, passed back each turn and bounded so it doesn't outgrow the context window.
- **Long-term memory** — facts/observations/user profiles surviving across sessions, usually in a DB or vector store.

## Automatic conversation memory

Give the agent a memory backend + conversation id and Rig loads stored history before each prompt and appends the committed turn (the prompt, the assistant response, and tool-call/result pairs) after the run succeeds:

```rust
use rig_agent::AgentBuilder;
use rig_memory::MemoryStore;

let agent = AgentBuilder::new(model)
    .preamble("You are a helpful assistant.")
    .memory(MemoryStore::default())     // any `rig_memory::AsyncStore`
    .conversation("user-42")            // the log name the session is kept under
    .build();

let _ = agent.prompt("My name is Ada.").await?;
let reply = agent.prompt("What's my name?").await?;
println!("{}", reply.output());        // `output()` is a method
```

Rules:
- The log name can be a builder default (`AgentBuilder::conversation(log)`) or a runner override (`.conversation(log)`). No name → no memory.
- `memory(store)` takes any `impl AsyncStore + Send + Sync + 'static` (every `Store` is one, so `MemoryStore`, `DirStore`, `Arc<dyn Store>`, and your own database/IndexedDB backend all fit). `.conversation(log)` without a store warns at build and does nothing.
- Passing explicit history with `.history(iter)` **bypasses memory entirely** for that run — nothing loaded, nothing appended (the committed messages return on `PromptResponse::messages()` for caller-owned persistence).
- `.without_memory()` clears both the store and the log name for a single run (no load, no save).
- A failed load fails the run with `PromptError::Memory` before any model call.

### The store, the session and the conversation

| Piece | Where | What it is |
|-------|-------|-----------|
| `Conversation` | `rig_core::conversation` | the one conversation model rig shares: entries with stable ids, edits as `Change`s, `TurnRecord`s kept as asides, and `pairing` of tool calls with their results |
| `Files` / `Store` / `AsyncStore` | `rig_memory::{Files, Store, AsyncStore}` | where a session is kept: `Files` by name, `Store` also holds each log with a length-checked append (a stale write is `LogWriteError::Changed`), `AsyncStore` the same storage reached asynchronously (a database, an object store, IndexedDB) |
| `MemoryStore` / `DirStore` / `PluginFiles` | `rig_memory` | in-process store; `DirStore` = one directory per session (`feature = "fs"`); `PluginFiles` for a host's own named files |
| `Session` / `SavedAgent` | `rig_memory::{Session, SavedAgent}` | a session as its logs left it: agents in start order with their `Conversation`, the session's saved values, and what reading it repaired (`repairs`) |
| `load` / `save` / `load_async` / `save_async` | `rig_memory` | one log's `Conversation` in and out of a `Store`, without a running `Writer` — `save` writes only the changes not yet in the log |
| `Writer` / `SessionMeta` / `read_conversation` / `history` / `as_of` | `rig_memory` | a running conversation's writer; what a session *list* shows without folding every log; a log's records and the conversation as of record `n` |
| `recent` | `rig_memory::recent` | which newest messages a request sends: `Recent` (`start(&[Arc<Message>]) -> usize`), `LastMessages(n)`, `TokenBudget::new(tokens)` (custom `TokenCounter` if you have a real tokenizer), and `view(&[Arc<Message>], &dyn Recent)` which moves the start past results whose calls it cut off, so every result kept still follows its call |
| `FORMAT` | `rig_memory::FORMAT` | the one on-disk format; a session of another format, or that does not read whole, is refused and nothing is written |

### What the run hands back

| Item | Notes |
|------|-------|
| `PromptResponse::messages` | `Vec<Message>` — the run's transcript: the prompt, every accepted assistant turn, every committed tool result and any corrective feedback, *excluding* the input history. This, and only this, is what memory is asked to persist. |
| `PromptResponse::messages()` | the same transcript by reference |

In 0.45 there is no per-run append receipt (`memory_append` is gone): the `Writer` a session keeps is what persists, and a save that raced another writer is refused (`SessionError::Changed`) rather than merged.

> **Resumed runs.** `Agent::resume(run)` continues a persisted run: its prompt and history are already inside the run, so **nothing is loaded**. Whether the run's added messages are appended at the end is the persisted run's business; in 0.45 there is no per-run receipt to inspect. Persist the finished run's `messages` yourself and never depend on the append there. `history`, `max_turns` and `unhandled_invalid_tool_call` on a resumed runner are inert for the same reason — the run carries its own.

`MemoryStore` lives in process (a `Mutex` over an in-memory file map) and forgets on restart; `DirStore::new(dir)` persists a session per directory (native, `feature = "fs"`). For a database or IndexedDB, implement `AsyncStore` — the same read/write/length-checked-append surface reached with futures — or wrap a synchronous store in `BlockingStore`/`PluginFiles`-style shims. `Arc<dyn Store>` and `Box<dyn Store>` are themselves a `Store`, so a host can hold either a concrete store or a shared trait object.

Messages are `Serialize`/`Deserialize` — persisting a conversation is ordinary serde work (a JSON column per conversation id is fine). Keep `append` cheap: it runs inline before the agent returns.

> **Memory is a bus effect, not a special case.** Load and append are `EffectKind::Memory { op: MemoryOp::Load { conversation } | MemoryOp::Append { conversation, messages } | MemoryOp::Clear { .. } }` answered with `MemoryOutcome::{Loaded { messages }, Appended, Cleared}`, so every load and append is a dispatch that hooks can observe, record and replay. A failed dispatch is mapped back for you: `ErrorKind::MemoryPolicy` (and a layer `Denied`) → `MemoryError::Policy`, `ErrorKind::MemoryBackend` → `MemoryError::Backend`, anything else → `MemoryError::Internal`.

## Bounding history with filters and policies

Raw history grows without bound — long histories are worse than just expensive (models get distracted; context windows overflow). The fix is managed forgetting: shape what `load` returns.

In 0.45 the window is chosen when a request is built, not by a filter on `load`. `rig_memory::recent` gives the rule:

```rust
use rig_memory::recent::{LastMessages, TokenBudget, view};

let window = view(&conversation.messages(), &LastMessages(20));   // newest 20
let window = view(&conversation.messages(), &TokenBudget::new(32_000)); // newest that fit
```

`view` runs `pairing::safe_start` past the start the rule picked, so a `ToolResult` is never kept without its call (a request whose result has no call is rejected by the wires). Because it is a *view*, the store keeps everything — nothing is evicted, so nothing needs archiving.

For managed forgetting of a whole conversation, `rig_memory` ships the compaction policy rather than adapter traits:

- **`CompactionPolicy`** fields: `summarizer: Summarizer` (whose `reserve` is how much of the model's window is kept free), `tracked: Vec<TrackArgument>`, `keep_recent` (tokens of the newest messages an *automatic* compaction keeps verbatim — default 20k, a quarter of the window at most), `keep_asked` (the same for a compaction the user asked for; the newest reply or message always stays), `clearing: ClearToolOutputs` (how old tool outputs are cleared before a summary is asked for) and `near_window: NearWindow` (what happens to the outputs a near-window clearing drops). `CompactionPolicy::default()` is `Summarizer::DEFAULT`, nothing tracked, 20k kept, default clearing, and notes on what clearing dropped.
- `apply_summary` splices a summary artifact back in as the conversation's prefix (`summary_in`/`summarized_prefix` read it back, `summary_author`), and `undo_summary` takes it out again — the rolling-summary pattern, on the `Conversation` rather than a filter closure.
- `estimate_tokens`, `summary_state`, `TrackedSet`, `ClearPlan`/`Cleared`, `SummaryPlan`/`SummaryState`/`SummaryLimits`/`SummaryPrompts`, `CompactReason`, `Compacted`/`Uncompacted` are the rest of the policy surface; `SummaryError`/`UndoError`/`SessionError` are its failures.

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

The mechanics are the same for all three: after a significant exchange, use an `Extractor` (see `structured-output.md`) or a plain prompt to distill the relevant info, then persist it. When a new conversation starts, retrieve the most relevant items and add them to the system prompt or opening messages. For semantic retrieval, use a vector store (see `rag-and-vector-stores.md`), and keep the distilled facts there rather than in the session log — `rig-memory` keeps the conversation, not a fact store.
