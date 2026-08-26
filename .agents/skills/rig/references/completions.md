# Completions

Completions are the layer beneath agents: traits and types for sending a single request to a language model and handling what comes back. Rig layers the API so you can work at whatever altitude the task needs — one-line prompting at the top, full request control at the bottom — and every layer speaks the same `Message` and response types.

Official docs: https://rig.rs/docs/concepts/completion · API: https://docs.rs/rig/latest/rig/completion/index.html

## Choosing an interface

| You want… | Use |
|-----------|-----|
| a text answer to a one-off prompt | `Prompt` (`.prompt(...)`) |
| a conversation that carries history | `Chat` (`.chat(prompt, &mut history)`) |
| a typed struct instead of a string | `TypedPrompt` (`.prompt_typed(...)`) |
| tokens as they arrive | `StreamingPrompt`/`StreamingChat` (see `streaming.md`) |
| to configure the request before dispatch | `CompletionRequestBuilder` (via `model.completion_request(...)`) |
| to bypass the agent loop entirely | `CompletionModel` directly (`.completion_request(...).send()`) |

Most apps reach for an **Agent**, which implements the high-level traits *and* runs the agent loop. Drop to a bare `CompletionModel` when you need control over individual requests.

> The `Completion` and `StreamingCompletion` high-level traits were **removed in 0.42**. Configure a single request through `CompletionModel::completion_request(prompt)` → `CompletionRequestBuilder` (a type alias for `CompletionRequestBuilder::builder(model, prompt)`), or build a `CompletionRequest` directly and call `CompletionModel::completion(req)` / `CompletionModel::stream(req)`.

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
    pub choice: Vec<AssistantContent>,
    pub raw_response: T, // raw provider payload for debugging
}
```

`AssistantContent` — the things a model can answer with:

```rust
pub enum AssistantContent {
    Text(Text),
    ToolCall(ToolCall),     // correlation id + function name + JSON args
    Reasoning(Reasoning),   // chain-of-thought (models that support it)
    Image(Image),
}
```

With an agent, tool calls are executed for you; at the bare-model layer you decide what to do with them.

## Messages

In 0.42, message content is a plain `Vec<T>` — the `OneOrMany` container was removed:

```rust
pub enum Message {
    System { content: String },
    User { content: Vec<UserContent> },
    Assistant { id: Option<String>, content: Vec<AssistantContent> },
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

`ToolCall` and `ToolResult` carry correlation handles instead of bare string ids:

```rust
pub struct ToolCall {
    pub id: ToolCallId,                        // always present (minted if the provider gave none)
    pub provider: Option<ProviderCallId>,      // provider-issued id, if any
    pub function: ToolFunction,                // { name, arguments: serde_json::Value }
    pub signature: Option<String>,
    pub additional_params: Option<serde_json::Value>,
}

pub struct ToolResult {
    pub call: ToolCallId,                      // echoes the answered ToolCall::id
    pub provider: Option<ProviderCallId>,
    pub name: String,                          // executed tool's name
    pub content: Vec<ToolResultContent>,
}
```

`ToolCallId`/`ProviderCallId` are `String`-like (`.as_str()`, `Display`). Correlate a result with its call via `call == answered_call.id`. Streaming additionally exposes an `internal_call_id` correlator (see `streaming.md`).

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

Zero-valued usage means the provider didn't report metrics. (The `GetTokenUsage` trait was removed in 0.42 — providers now surface usage directly on `Usage`.)

## Errors

```rust
pub enum CompletionError {
    HttpError(reqwest::Error),
    JsonError(serde_json::Error),
    UrlError(url::ParseError),
    RequestError(Box<dyn Error>),
    ResponseError(String),
    ProviderError(String),
}
```

Typed-output paths add `StructuredOutputError` wrapping `PromptError` (which carries `CompletionError`, `MaxTurnsError`, `PromptCancelled`, `UnknownToolCall`, or a tool failure) or a deserialization failure. See https://rig.rs/docs/concepts/error_handling for transient-vs-fatal handling and retry.

## Project boundary (shuvarie)

`shuvarie-llm` should expose its own async completion/streaming API and map `CompletionError`/`PromptError` into `LlmError::Provider`/`LlmError::Model` (thiserror). The root binary never imports `rig::completion::*` directly.