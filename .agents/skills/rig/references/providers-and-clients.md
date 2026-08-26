# Providers & Clients

A **provider client** is the entry point to any LLM vendor in Rig. It authenticates against a provider (OpenAI, Anthropic, Gemini, Ollama, …) and creates the models you actually prompt — completion, embedding, transcription, image, and audio models — depending on what that provider supports.

Official docs: https://rig.rs/docs/concepts/provider_clients · API: https://docs.rs/rig/latest/rig/client/index.html

## Vocabulary

- **Provider** — the vendor and its module in `rig`, e.g. `rig::providers::openai`. Holds the client, model constants, and request/response types.
- **Client** — the configured entry point, e.g. `openai::Client`. Holds the API key + base URL and knows how to make requests. From a client you build models and higher-level constructs.
- **Model** — a handle produced by the client, e.g. `client.completion_model("gpt-5.5")` or `client.embedding_model("text-embedding-3-small")`. Implements `CompletionModel` / `EmbeddingModel` / etc.

Flow is always: **provider → client → model → agent/extractor/index**.

## The `Client` struct

`rig::client::Client<Ext, H>` is generic over:
- `Ext`: a provider extension implementing the `Provider` trait (provider-specific behavior).
- `H`: an API key type implementing the `ApiKey` trait.

You rarely name these generics — each provider module exposes a `Client` type alias with them filled in.

## Core traits

```rust
pub trait ProviderClient {
    type Input;
    type Error;
    fn from_env() -> Result<Self, Self::Error> where Self: Sized;
    fn from_val(input: Self::Input) -> Result<Self, Self::Error> where Self: Sized;
}

pub trait CompletionClient {
    type CompletionModel: CompletionModel;
    fn completion_model(&self, model: impl Into<String>) -> Self::CompletionModel;
    fn agent(&self, model: impl Into<String>) -> AgentBuilder; // via AgentClientExt
}

pub trait EmbeddingsClient {
    type EmbeddingModel: EmbeddingModel;
    fn embedding_model(&self, model: impl Into<String>) -> Self::EmbeddingModel;
}

// Plus: TranscriptionClient, ImageGenerationClient (image feature),
// AudioGenerationClient (audio feature), ModelListingClient, VerifyClient.
```

`AgentClientExt` (brought in by `use rig::prelude::*;`) provides the `client.agent(id)` shorthand. In 0.42 `CompletionClient::completion_model` takes `impl Into<String>` (not `&str`), and `client.agent(id)` returns a model-agnostic `AgentBuilder` that erases the model into a `ModelHandle` on `.build()`.

## Capabilities system

Compile-time capability checking via `Capabilities<H>` + `Capability`:

```rust
pub trait Capabilities<H = Client> {
    type Completion: Capability;
    type Embeddings: Capability;
    type Transcription: Capability;
    type ModelListing: Capability;
    type ImageGeneration: Capability;
    type AudioGeneration: Capability;
}
pub trait Capability { const CAPABLE: bool; }
```

Only the client traits a provider declares are implemented — e.g. calling `embedding_model(...)` on a provider without `EmbeddingsClient` is a compile error, not a runtime failure.

| Capability | Trait | Description |
|------------|-------|-------------|
| Text Completion | `CompletionClient` | Create completion models |
| Embeddings | `EmbeddingsClient` | Create embedding models |
| Transcription | `TranscriptionClient` | Speech-to-text |
| Image Generation | `ImageGenerationClient` | Requires `image` feature |
| Audio Generation | `AudioGenerationClient` | Requires `audio` feature |
| Model Listing | `ModelListingClient` | List available models |
| Verification | `VerifyClient` | Verify credentials/connectivity |

## Auth types

- `BearerAuth` — API key as bearer token in request headers.
- `NeedsApiKey` — marker: provider requires an API key.
- `Nothing` — marker: provider needs no key (e.g. local Ollama).

## Configure any provider

Three constructors on every provider client:

```rust
use rig::prelude::*;
use rig::providers::openai;

// 1. From a well-known env var (OPENAI_API_KEY, ANTHROPIC_API_KEY, …)
let client = openai::Client::from_env()?;

// 2. From an explicit key (config file / secret manager)
let client = openai::Client::new("your-api-key");

// 3. From a key + custom base URL (proxy, gateway, OpenAI-compatible endpoint)
let client = openai::Client::builder()
    .api_key("your-api-key")
    .base_url("https://your-gateway.example.com/v1")
    .build()?;
```

> Many providers (Groq, Together, Hyperbolic, OpenRouter, DeepSeek, …) expose an **OpenAI-compatible** API. Point `openai::Client::builder().base_url(...)` at them and pass their model ids, or use the dedicated module when Rig ships one.

## Build a model or agent

```rust
use rig::prelude::*;
use rig::providers::openai;

let client = openai::Client::from_env()?;
let model = client.completion_model("gpt-5.5");           // raw CompletionModel
let embedder = client.embedding_model("text-embedding-3-small");
let agent = client
    .agent("gpt-5.5")
    .preamble("You are a helpful assistant.")
    .build();
```

Swapping providers is usually just the import + the model id — the client methods are identical across providers.

## Supported providers (rig-core 0.42)

`rig::providers::*` — `anthropic`, `azure`, `chatgpt` (OAuth), `cohere`, `copilot`, `deepseek`, `doubleword`, `gemini`, `groq`, `huggingface`, `hyperbolic`, `llamafile`, `minimax`, `mira`, `mistral`, `moonshot`, `ollama`, `openai`, `openrouter`, `perplexity`, `together`, `voyageai`, `xai`, `xiaomimimo`, `zai`.

Companion provider crates behind features: `bedrock` (`rig::bedrock`), `candle` (local CPU Llama/SmolLM2/Qwen3), `gemini-grpc`, `vertexai`.

## OpenAI specifics

- Completion via Chat Completions + Responses API (default). Model ids: `gpt-5.5`, `gpt-5-mini`, … or constants `openai::GPT_5_5`, `openai::GPT_4`.
- Embedding constants + dimensions: `TEXT_EMBEDDING_3_LARGE` (3072), `TEXT_EMBEDDING_3_SMALL` (1536), `TEXT_EMBEDDING_ADA_002` (1536, legacy).
- Tool calling: tools are auto-translated to OpenAI function definitions; tool-call responses parse back into Rig types.
- Multimodal: vision-capable models accept `UserContent::Image`. Image generation (`dall-e-3`, `gpt-image-1`) and audio (`tts-1`, `whisper-1`) behind `image`/`audio` features.
- **Responses API requires every tool parameter under `required`** — use `schemars::JsonSchema` (non-`Option` → required) or the macro's `required(...)` helper.

## Writing a custom provider

1. Implement `Provider` for your extension type.
2. Implement `CompletionModel` for your completion model type (and `EmbeddingModel`/`TranscriptionModel`/… as needed).
3. Implement the relevant client traits (`CompletionClient`, `EmbeddingsClient`, …).
4. Prefer `GenericCompletionModel` + `OpenAICompatibleProvider` for OpenAI-chat-compatible APIs — never hand-roll a completion model; dialect differences go in the trait hooks.

Guide: https://rig.rs/docs/guides/extension/write_your_own_provider

## Project boundary (shuvarie)

`shuvarie-llm` wraps rig's per-provider clients behind its own `Provider` enum so the TUI never names a provider concretely. Add new providers in `crates/llm/`, not in the root binary; the root binary drives the core task which drives `shuvarie-llm`.