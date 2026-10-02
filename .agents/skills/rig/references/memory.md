# Memory

Memory is an agent's ability to retain and reuse information from earlier in a conversation (and across conversations). Without it, every prompt starts from scratch. Rig gives you both ends of the spectrum — attach a conversation-memory backend and history is loaded/appended for you, or own the `Vec<Message>` yourself and decide exactly what the model sees.

Official docs: https://rig.rs/docs/concepts/memory · API: https://docs.rs/rig-core/latest/rig_core/memory/index.html (0.43: the contracts moved into `rig-core-0.43.0/src/memory.rs`; the old `rig-memory` policy crate is not published for 0.43 — write filters as closures or implement the policies yourself)

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
println!("{}", reply.output);
```

Rules:
- The conversation id can be a builder default (`AgentBuilder::conversation(id)`) or a runner override (`.conversation(id)`). No id → no memory.
- `memory(handler)` must come from the builder (or `memory_handler(...)` for a custom retrieval-family handler); a `.conversation(id)` without a backend warns and does nothing.
- Passing explicit history with `.history(iter)` **bypasses memory entirely** for that run — nothing loaded, nothing appended (the committed messages return on `PromptResponse.messages` for caller-owned persistence).
- `.without_memory()` disables memory for a single run.
- A failed load fails the run with `PromptError::MemoryError` before any model call; a failed append is **acknowledged** on `PromptResponse.memory_append` without invalidating the answer.

`InMemoryConversationMemory` lives in process, protected by a `std::sync::Mutex<HashMap<ConversationId, Vec<Message>>>`, and forgets on restart. For durable sessions, implement `ConversationMemory` over your own store:

```rust
pub trait ConversationMemory: Send + Sync {
    fn load<'a>(&'a self, conversation_id: &'a ConversationId)
        -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>>;
    fn append<'a>(&'a self, conversation_id: &'a ConversationId, messages: Vec<Message>)
        -> WasmBoxedFuture<'a, Result<(), MemoryError>>;
    fn clear<'a>(&'a self, conversation_id: &'a ConversationId)
        -> WasmBoxedFuture<'a, Result<(), MemoryError>>;
}
```

Messages are `Serialize`/`Deserialize` — persisting a conversation is ordinary serde work (a JSON column per conversation id is fine). Keep `append` cheap: it runs inline before the agent returns.

## Bounding history with filters and policies

Raw history grows without bound — long histories are worse than just expensive (models get distracted; context windows overflow). The fix is managed forgetting: shape what `load` returns.

In 0.43 `InMemoryConversationMemory::with_filter(f)` takes any **closing-over closure** `Fn(Vec<Message>) -> Vec<Message>` (a `MessageFilter`): truncation, re-ordering, token-based windows — implement the policy you need:

```rust
use rig_core::memory::InMemoryConversationMemory;

let memory = InMemoryConversationMemory::new().with_filter(|messages| {
    messages.last_chat_window(20)   // your windowing policy
});
```

For richer managed-forgetting flows, rig-core also ships the composing contracts (implement or wrap as needed; the 0.42 `rig-memory` crate's named policies are not republished for 0.43):

- **`MessageFilter`** — `Fn(Vec<Message>) -> Vec<Message>` applied on every load (drop-oldest windows, token budgeting).
- **`DemotionHook`** — `on_demote(conversation_id, messages)`: receive the messages evicted from the active window (archive to a long-tail store). Awaited inline — keep it fast; deduplicate deliveries by `(conversation_id, content hash)`.
- **`Compactor`** — `compact(conversation_id, evicted, carry_over: Option<&Artifact>) -> Artifact: Into<Message>`: replace evicted messages with a summary artifact spliced back into history (rolling-summary pattern, carry-over folds previous summaries in). Runs inline on the load path — a slow compactor delays the agent's next turn.

## Managing history by hand

Own the history yourself for custom storage/shaping. History is a `Vec<Message>` (message content is a plain `Vec<T>` — `OneOrMany` was removed):

```rust
use rig_core::message::{AssistantContent, Message, UserContent};

let mut conversation_history: Vec<Message> = Vec::new();
conversation_history.push(Message::User {
    content: vec![UserContent::Text(rig_core::message::Text::new(
        "Do you know the weather today?",
    ))],
});
conversation_history.push(Message::Assistant {
    id: None,
    content: vec![AssistantContent::text("I don't have real-time data...")],
});
```

Or use the constructors for the common text-only case:

```rust
history.push(Message::user("What is Rust?"));
history.push(Message::assistant("A systems programming language..."));
```

> **`.history(..)` on a runner does not append** the new user message or response to the history you pass (and bypasses memory). Record the new turn yourself — the messages the run committed arrive on `PromptResponse.messages`. (`chat(prompt, &mut history)` is the exception — it **does** append the committed turn, including tool calls, so don't push messages again.)

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
    if let Some(messages) = response.messages {
        history.extend(messages);
    }
    Ok(response.output)
}
```

Interactive REPL example: https://rig.rs/docs/guides/cli_chatbot

### Rolling your own compaction

Once hand-managed history grows past a threshold, ask the model for a summary and start a fresh history seeded with it:

```rust
use rig_core::completion::Message;
use rig_core::driver::DynModel;
use rig_core::message::AssistantContent;

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
            Message::Assistant { content, .. } => content.iter().find_map(|c| match c {
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
    let summary = response.choice.iter().filter_map(|c| match c {
        AssistantContent::Text(text) => Some(text.text.clone()),
        _ => None,
    }).collect::<String>();
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