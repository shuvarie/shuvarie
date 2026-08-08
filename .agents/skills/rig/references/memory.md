# Memory

Memory is an agent's ability to retain and reuse information from earlier in a conversation (and across conversations). Without it, every prompt starts from scratch. Rig gives you both ends of the spectrum — attach a conversation-memory backend and history is loaded/saved for you, or own the `Vec<Message>` yourself and decide exactly what the model sees.

Official docs: https://rig.rs/docs/concepts/memory · API: https://docs.rs/rig/latest/rig/memory/index.html

Two layers:
- **Conversation history** (short-term) — messages of the current conversation, passed back each turn and bounded so it doesn't outgrow the context window.
- **Long-term memory** — facts/observations/user profiles surviving across sessions, usually in a DB or vector store.

## Automatic conversation memory

Give the agent a memory backend + conversation id and Rig loads stored history before each prompt and appends the new turn (including tool calls/results) after it succeeds:

```rust
use rig::memory::InMemoryConversationMemory;

let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a helpful assistant.")
    .memory(InMemoryConversationMemory::new())
    .build();

let _ = agent.prompt("My name is Ada.").conversation("user-42").await?;
let reply = agent.prompt("What's my name?").conversation("user-42").await?;
println!("{reply}");
```

Rules:
- The conversation id can be set per request (`.conversation("...")`) or as a builder default (`AgentBuilder::conversation_id(...)`). No id → no memory.
- Passing explicit history with `.with_history(...)` **bypasses memory entirely** for that request — nothing loaded, nothing saved.
- `.without_memory()` disables memory for a single request.

`InMemoryConversationMemory` lives in process — ideal for tests and short-lived agents but forgets on restart. For durable sessions, implement `ConversationMemory` over your own store:

```rust
pub trait ConversationMemory {
    async fn load(&self, conversation_id: &str) -> Result<Vec<Message>, MemoryError>;
    async fn append(&self, conversation_id: &str, messages: Vec<Message>) -> Result<(), MemoryError>;
    async fn clear(&self, conversation_id: &str) -> Result<(), MemoryError>;
}
```

`Message` is `Serialize`/`Deserialize` — persisting a conversation is ordinary serde work (a JSON column per conversation id is fine). Keep `append` cheap: it runs inline before the agent returns.

## Bounding history with policies

Raw history grows without bound — long histories are worse than just expensive (models get distracted; context windows overflow). The fix is managed forgetting: shape what `load` returns.

The `rig-memory` companion crate ships reusable policies (`cargo add rig-memory`):

```rust
use rig::memory::InMemoryConversationMemory;
use rig_memory::{IntoFilter, SlidingWindowMemory};

let memory = InMemoryConversationMemory::new()
    .with_filter(SlidingWindowMemory::last_messages(20).into_filter());
```

`TokenWindowMemory` bounds by estimated token cost instead of message count:

```rust
use rig::memory::InMemoryConversationMemory;
use rig_memory::{HeuristicTokenCounter, IntoFilter, TokenWindowMemory};

let memory = InMemoryConversationMemory::new().with_filter(
    TokenWindowMemory::new(4_000, HeuristicTokenCounter::openai()).into_filter(),
);
```

Both policies drop a leading orphaned tool result when its paired tool call is truncated away (most providers reject unpaired tool results). Truncation silently discards dropped turns. Two composing adapters turn that loss into something useful:

- **`DemotingPolicyMemory`** — hands evicted messages to a `DemotionHook`, so you can archive them into a long-tail store (vector store for semantic recall, cold storage for audit).
- **`CompactingMemory`** — replaces evicted messages with a summary artifact spliced back into history (rolling-summary pattern):

```rust
use rig::memory::InMemoryConversationMemory;
use rig_memory::{CompactingMemory, SlidingWindowMemory, TemplateCompactor};

let memory = CompactingMemory::new(
    InMemoryConversationMemory::new(),
    SlidingWindowMemory::last_messages(20),
    TemplateCompactor::new(), // deterministic textual rollup, no model call
);

let agent = openai
    .agent("gpt-5.5")
    .preamble("You are a helpful assistant.")
    .memory(memory)
    .build();
```

`TemplateCompactor` produces a plain-text rollup without any model call. For higher-quality summaries, implement the `Compactor` trait with an LLM call — its `carry_over` parameter hands you the previous summary so each compaction folds in what came before. Compactors run inline on the load path, so a slow one delays the agent's next turn.

## Managing history by hand

Own the history yourself for custom storage/shaping. History is a `Vec<Message>`:

```rust
use rig::completion::Message;
use rig::OneOrMany;
use rig::message::{AssistantContent, UserContent};

let mut conversation_history: Vec<Message> = Vec::new();
conversation_history.push(Message::User {
    content: OneOrMany::one(UserContent::text("Do you know the weather today?")),
});
conversation_history.push(Message::Assistant {
    id: None,
    content: OneOrMany::one(AssistantContent::text("I don't have real-time data...")),
});
```

Or use the constructors for the common text-only case:

```rust
history.push(Message::user("What is Rust?"));
history.push(Message::assistant("A systems programming language..."));
```

> **As of Rig 0.38, `with_history` on a prompt request no longer appends** the new user message and assistant response to the message list you pass in. Record the new turn yourself after each call. (`chat(prompt, &mut history)` is the exception — it **does** append the new turn, including tool calls, so don't push messages again.)

```rust
use rig::agent::Agent;
use rig::completion::Message;
use rig::prelude::*;

async fn call_agent_with_chat_history(
    prompt: &str,
    history: &mut Vec<Message>,
) -> Result<String, Box<dyn std::error::Error>> {
    let openai_client = openai::Client::from_env()?;
    let agent = openai_client
        .agent("gpt-5.5")
        .preamble("You are a helpful assistant. Be concise.")
        .name("Bob") // used in logging
        .build();

    let response_text = agent.prompt(prompt).with_history(history.iter()).await?;
    history.push(Message::user(prompt));
    history.push(Message::assistant(&response_text));
    Ok(response_text)
}
```

Interactive REPL example: https://rig.rs/docs/guides/cli_chatbot

### Rolling your own compaction

Once hand-managed history grows past a threshold, ask the model for a summary and start a fresh history seeded with it:

```rust
use rig::agent::Text;
use rig::completion::{CompletionModel, Message};
use rig::message::{AssistantContent, UserContent};

async fn compact_history<M: CompletionModel>(
    model: &M,
    history: &[Message],
) -> Result<Vec<Message>, Box<dyn std::error::Error>> {
    let transcript = history
        .iter()
        .filter_map(|msg| match msg {
            Message::User { content } => content.iter().find_map(|c| match c {
                UserContent::Text(Text { text, .. }) => Some(format!("User: {text}")),
                _ => None,
            }),
            Message::Assistant { content, .. } => content.iter().find_map(|c| match c {
                AssistantContent::Text(Text { text, .. }) => Some(format!("Assistant: {text}")),
                _ => None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let summary_prompt = format!(
        "Provide a concise summary of the following conversation:\n\n{transcript}"
    );
    let response = model.completion_request(&summary_prompt).send().await?;
    let AssistantContent::Text(Text { text, .. }) = response.choice.first() else {
        return Err("Model returned non-text response".into());
    };
    Ok(vec![Message::user(format!("Context from previous conversation:\n{text}"))])
}
```

Trigger compaction after every turn, on a schedule, or once a token budget is exceeded (needs a tokenizer or `rig-memory`'s `HeuristicTokenCounter`).

## Long-term memory

Bounded history keeps a single conversation healthy; many apps need memory surviving across sessions. Common strategies:

- **Conversation observations** — insights extracted from an exchange (decisions, open questions, strong interests).
- **User observations / profile** — persistent facts about the user (preferences, location, communication style). Keep separate from conversation history; update incrementally; re-verify before use.
- **Grounded facts** — objectively verifiable data pulled from external sources during a session (retrieved docs, computed results, API responses), stored with source + timestamp.

The mechanics are the same for all three: after a significant exchange, use an `Extractor` (see `structured-output.md`) or a plain prompt to distill the relevant info, then persist it. When a new conversation starts, retrieve the most relevant items and add them to the system prompt or opening messages. For semantic retrieval, use a vector store (see `rag-and-vector-stores.md`). A `DemotionHook` (above) is a natural place to feed evicted conversation turns into such a store.