# Completions

Completions are the layer beneath agents: traits and types for sending a single request to a language model and handling what comes back. Rig layers the API so you can work at whatever altitude the task needs — one-line prompting at the top, full request control at the bottom — and every layer speaks the same `Message` and response types.

Official docs: https://rig.rs/docs/concepts/completion · API: https://docs.rs/rig/latest/rig/completion/index.html

## Choosing an interface

| You want… | Use |
|-----------|-----|
| a text answer to a one-off prompt | `Prompt` (`.prompt(...)`) |
| a conversation that carries history | `Chat` (`.chat(prompt, &mut history)`) |
| a typed struct instead of a string | `TypedPrompt` (`.prompt_typed(...)`) |
| tokens as they arrive | `StreamingPrompt`/`StreamingChat`/`StreamingCompletion` (see `streaming.md`) |
| to configure the request before dispatch | `Completion` (`.completion(...)` → `CompletionRequestBuilder`) |
| to bypass the agent loop entirely | `CompletionModel` directly (`.completion_request(...).send()`) |

Most apps reach for an **Agent**, which implements the high-level traits *and* runs the agent loop. Drop to a bare `CompletionModel` when you need control over individual requests.

## High-level traits

`Prompt` — one prompt in, one `String` out:

```rust
async fn prompt(&self, prompt: &str) -> Result<String, PromptError>;
```

`Chat` — conversation-aware; takes prior messages and **appends** the new turn (including tool calls/results) to the history you pass:

```rust
async fn chat(&self, prompt: impl Into<Message>, chat_history: &mut Vec<Message>)
    -> Result<String, PromptError>;
```

> Do not push the user message / assistant reply yourself after `chat` — it already did. (`with_history` on `prompt` does NOT append — see `memory.md`.)

`TypedPrompt` — returns deserialized structured data. Target type derives `serde::Deserialize` + `schemars::JsonSchema`:

```rust
use rig::schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct SentimentAnalysis {
    /// Sentiment score from -1.0 to 1.0
    score: f64,
    label: String,
}

let result: SentimentAnalysis = agent
    .prompt_typed("Analyze: 'I love this product!'")
    .await?;
```

For whole-job structured extraction prefer an `Extractor` (see `structured-output.md`).

## Low-level control

### `Completion` trait

Returns a `CompletionRequestBuilder` you can adjust before dispatch. Fields pre-populated by the implementing type (e.g. an agent's preamble) can be overwritten:

```rust
pub trait Completion<M: CompletionModel> {
    fn completion(
        &self, prompt: &str, chat_history: Vec<Message>,
    ) -> impl Future<Output = Result<CompletionRequestBuilder<M>, CompletionError>> + Send;
}
```

### Calling a `CompletionModel` directly

`CompletionModel` is the provider interface — the trait each LLM backend implements with `completion` (and `stream`). Calling it directly is how you take full control of one request:

```rust
use rig::client::{CompletionClient, ProviderClient};
use rig::providers::openai::Client;

let client = Client::from_env()?;
let model = client.completion_model("gpt-5.5");

let response = model
    .completion_request("What is Rust?")
    .preamble("You are a helpful assistant.".to_string())
    .temperature(0.7)
    .max_tokens(1000)
    .send()
    .await?;
```

The builder also accepts `documents(...)` (context) and `tools(...)`. Call `.build()` to get a `CompletionRequest` and pass it to `CompletionModel::completion()` yourself; `.send()` is just those two steps fused.

## Responses

```rust
pub struct CompletionResponse<T> {
    pub choice: OneOrMany<AssistantContent>,
    pub raw_response: T, // raw provider payload for debugging
}
```

`AssistantContent` — the three things a model can answer with:

```rust
pub enum AssistantContent {
    Text(Text),
    ToolCall(ToolCall),     // id + function name + JSON args
    Reasoning(Reasoning),   // chain-of-thought (models that support it)
}
```

With an agent, tool calls are executed for you; at the bare-model layer you decide what to do with them.

## Messages

```rust
pub enum Message {
    User { content: OneOrMany<UserContent> },
    Assistant { content: OneOrMany<AssistantContent> },
}
```

`UserContent` supports text, tool results, and multimodal parts:

```rust
pub enum UserContent {
    Text(Text),
    ToolResult(ToolResult),
    Image(Image),
    Audio(Audio),
    Document(Document),
    Video(Video),
}
```

Use the constructors for the common text-only case rather than building the enum by hand:

```rust
history.push(Message::user("What is Rust?"));
history.push(Message::assistant("A systems programming language..."));
```

## Token usage

Every completion response carries a `Usage`; the agent loop aggregates across turns. Read aggregated usage with `.extended_details()` on a prompt request (see `agents.md`).

```rust
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub tool_use_prompt_tokens: u64,
    pub reasoning_tokens: u64,
}
```

Implement `GetTokenUsage` on your raw response type when writing a provider. Zero-valued usage means the provider didn't report metrics.

## Errors

```rust
pub enum CompletionError {
    HttpError(reqwest::Error),
    JsonError(serde_json::Error),
    RequestError(Box<dyn Error>),
    ResponseError(String),
    ProviderError(String),
}
```

Typed-output paths add `StructuredOutputError` wrapping `PromptError` (which carries `CompletionError`, `MaxTurnsError`, or a tool failure) or a deserialization failure. See https://rig.rs/docs/concepts/error_handling for transient-vs-fatal handling and retry.

## Project boundary (shuvarie)

`shuvarie-llm` should expose its own async completion/streaming API and map `CompletionError`/`PromptError` into `LlmError::Provider`/`LlmError::Model` (thiserror). The root binary never imports `rig::completion::*` directly.