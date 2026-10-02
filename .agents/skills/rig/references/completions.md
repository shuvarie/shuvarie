# Completions

Completions are the layer beneath agents: the call to send a single request to a language model and handle what comes back. In 0.43 there are no completion *traits* — a model is a concrete `Model<Wire, Transport>` value and the response types are portable. One-line prompting lives on the **agent** (`agent.prompt(..)`); full request control is building a `CompletionRequest` and calling the model.

Official docs: https://rig.rs/docs/concepts/completion · API: https://docs.rs/rig-core/latest/rig_core/completion/index.html (0.43: `rig-core-0.43.0/src/completion/`)

## Choosing an interface

| You want… | Use |
|-----------|-----|
| a text answer through the agent loop | `agent.prompt("...").await` → `PromptResponse` (`rig_agent`) |
| a conversation that carries caller-owned history | `agent.chat(prompt, &mut history)` (appends committed turns) or `.prompt(..).history(h)` + `PromptResponse.messages` |
| a typed struct instead of a string | `agent.prompt_typed::<T>(..)` or `ExtractorBuilder` (see `structured-output.md`) |
| tokens as they arrive | `agent.prompt(..).stream()` (see `streaming.md`) |
| to configure and send one request yourself | `CompletionRequest::new(..)` + setters → `model.call(req)` / `model.stream(req)` |

> The 0.42 `Prompt`/`Chat`/`TypedPrompt` traits and the `CompletionModel`/`CompletionRequestBuilder` surface were removed in 0.43 together with the client traits. Agents and bare `Model`s cover both altitudes.

## The model layer

```rust
use rig_core::completion::{CompletionRequest, Message};
use rig_core::providers::openai::{self, OpenAI};

let openai = OpenAI::from_env()?;
let model = openai.completion(openai::GPT_5_2);   // dialect's default route

let response = model
    .call(CompletionRequest::new("What is Rust?"))  // prompt; last message is the request
    .await?;
println!("{:?}", response.choice);

// Full control: build the request from messages directly.
let request = CompletionRequest::new("What is Rust?")
    .preamble("You are a helpful assistant.")
    .temperature(0.7)
    .max_tokens(1000);
let response = model.call(request).await?;

// One-model streaming (part events):
let streamed = model.stream(CompletionRequest::new("Hello")).unwrap(); // Streamed<Completion>
```

`CompletionRequest` fields: `model: Option<String>` (override), `chat_history: Vec<Message>` (the last message is the prompt; non-empty text required), `documents`, `tools`, `temperature`, `max_tokens`, `tool_choice`, `additional_params`, `output_schema` (structured output), `record_telemetry_content`. `CompletionRequest::new(prompt)` starts from one user message; builder-style setters (`with_*`) adjust it. The wire decodes the provider reply and folds it into a portable `CompletionResponse`.

`CompletionResponse`:

```rust
pub struct CompletionResponse {
    pub choice: Vec<AssistantContent>,   // the answer content
    pub usage: Usage,
    pub message_id: Option<String>,      // provider assistant-message id (replayable)
    pub response_id: Option<String>,     // provider response-scoped id
    pub provider_request_id: Option<String>,
    pub finish_reason: Option<FinishReason>,    // read via .finish_reason()
    pub provider: String,                // "openai", "anthropic", ...
    pub raw_response: Option<serde_json::Value>, // raw provider payload for debugging
}
```

## Messages

Message content is a plain `Vec<T>` (the `OneOrMany` container was removed in 0.42):

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

`AssistantContent`:

```rust
pub enum AssistantContent {
    Text(Text),
    ToolCall(ToolCall),
    Reasoning(rig_core::message::Sealed<Reasoning>), // sealed to its issuer (see below)
    Image(Image),
}
```

Reasoning blocks (`Reasoning { id, content: Vec<ReasoningContent> }` — `Text { text, signature }` / `Encrypted` / `Redacted` / `Summary`) are **sealed** — open them with `sealed.open(&issuer)` (`Issuer::accepts`; opening with the value's own `issuer()` always succeeds) and read `display_text()` / `first_text()` / `first_signature()`. Providers replay only reasoning sealed to them.

## Tool calls and results (0.43 identity model)

```rust
pub struct ToolCall {
    pub id: CallId,            // THE id: the provider's, or one rig minted (CallId::Local)
    pub function: ToolFunction,// { name: ToolName, arguments: serde_json::Value }
    pub signature: Option<String>,
    pub additional_params: Option<serde_json::Value>,
}

pub struct ToolResult {
    pub call: CallId,          // echoes the answered ToolCall::id
    pub name: ToolName,        // executed tool's name (may differ after hook repair)
    pub content: Vec<ToolResultContent>,
}
```

- `CallId` is an enum `{ Provider(ProviderCallId { call_id, item_id? }), Local(LocalCallId::new() uuid) }` — minted via `CallId::from_wire("..")` / `from_dual_wire(item_id, call_id)` (OpenAI Responses' two handles). `CallId::wire()`/`Display` renders the id string on the wire; `CallId::provider()` tells whether the provider issued it.
- `ToolName` is a non-empty newtype (`.as_str()`, `Display`, `ToolName::new(..)?`).
- Correlate a result with its call via `result.call == call.id`, and build the reply with `call.result(content)` (sets both).
- `ToolResultContent` is `Text(Text) | Image(Image) | Json { value }` with helpers `as_text()` / `as_json()` / `deserialize_json::<T>()`.

## Token usage

Every completion response carries a `Usage`; the agent loop aggregates across turns (`PromptResponse.usage`).

```rust
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,          // input + output; absent unless both reported
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
    pub tool_use_prompt_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}
```

A counter the provider did not send is `None`; a reported zero is `Some(0)`. `Usage::is_reported()` says whether the provider sent anything. `Add`/`AddAssign` sum per counter, treating an unreported side as zero but never "un-reporting" a reported one.

## Errors

```rust
pub enum ProviderError {
    Http(Arc<http_client::Error>),     // transport failure, no reply
    Json(Arc<serde_json::Error>),
    Url(url::ParseError),
    Request(SharedError),              // request could not be built
    Response(String),
    Provider(String),                  // provider reported failure, reply not preserved
    ProviderResponse(ProviderResponseError), // preserved reply (status, body, headers, request id)
    InvalidAuthentication(ProviderResponseError), // 401/403
    CacheExpired { name, response },
    MismatchedDimensions { provider, requested, returned },
    MalformedToolInput(..),
    Relayed(Box<ErrorReport>),         // a relayed report (bus/handler/hook)
    Truncated,                         // reply ended before the provider ended it
    DuplicateCallId(CallId),
}
```

Classification helpers: `error.kind() → ErrorKind`, `error.is_retryable()` (transport status/`is_retryable` policy; `Truncated` is retryable), and `ErrorReport` (a serde-able error with `kind`, `retryable`, `message`, `code`, `http_status`) crossing boundaries. Runtime prompt failures wrap into `rig_agent::completion::PromptError` (`CompletionError(ProviderError)`, `Report(ErrorReport)`, `MemoryError`, `MaxTurnsError`, `PromptCancelled`, `UnknownToolCall`); typed-output paths add `StructuredOutputError`. See https://rig.rs/docs/concepts/error_handling for transient-vs-fatal handling and retry.

## Project boundary (shuvarie)

`shuvarie-llm` exposes its own async completion/streaming API and maps `ProviderError`/`PromptError` into `LlmError::Provider`/`LlmError::Model` (thiserror). The root binary never imports `rig_core`'s completion types directly; the mapping (`classify_connection_error`, `is_context_length_error`) lives in `crates/llm/src/retry.rs`.