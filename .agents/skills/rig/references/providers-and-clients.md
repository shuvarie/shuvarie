# Providers & Clients

A **provider client** is the entry point to any LLM vendor in Rig. It holds a provider's **configuration** (credentials, base URL, routing/dialect policy) on a **transport** (how payloads travel) and builds the **models** you call — completion, embedding, listing, transcription, image, audio models — depending on what the provider serves.

Official docs: https://rig.rs/docs/concepts/provider_clients · API: https://docs.rs/rig-core/latest/rig_core/providers/index.html (for 0.43 also read the crate sources; the modules are heavily documented).

## Vocabulary (0.43)

- **Provider module** — the vendor's module in `rig_core::providers` (e.g. `providers::openai`). Holds the client, its config, wire types, dialect constants, model ids, and request/response normalization.
- **Config** — plain data describing the provider: `OpenAIConfig`, `AnthropicConfig`, `GeminiConfig`, `OllamaConfig`, `CopilotConfig`, `CohereConfig`, `VoyageAiConfig`. Carries the credential (`rig_core::wire::Secret`), base URL, and dialect/policy fields. Serializable and cheap to clone.
- **Client** — the config on a transport, e.g. `openai::OpenAI` (`Config::client()` / `Config::connect(http)` / `client.with_http(http)`). Builds models.
- **Wire** — what to send and how to read the reply: `openai::wire::{OpenAiWire, Chat, Embeddings, …}`, `anthropic::wire::Messages`, `gemini::GenerateContent`, `copilot::CopilotWire`, … Each wire declares its `Operation` (`Chat::Op = Completion`).
- **Model** — a `rig_core::driver::Model<Wire, Transport>` (e.g. `Model<Chat>` from `openai.chat(id)`): `call(request)` (folded), `stream(request)` (part events), `erase()` → `DynModel<Op>`.
- **Dialect** — the request/response policy of one vendor that speaks another provider's format: one `Dialect` constant per vendor in `openai::wire::dialects` (`GROQ`, `TOGETHER`, `DEEPSEEK`, `OPENROUTER`, `AZURE`, `LLAMACPP`, …). Vendors' `openai_vendor!`/`anthropic_vendor!` macros expose `from_env()`/`new(key)` constructors that build the shared client on their dialect.

Flow is always: **config → client → model → agent/extractor/index**.

## Building clients (0.43)

```rust
use rig_core::providers::{openai, anthropic, ollama};
use rig_core::providers::deepseek;

// 1. From the provider's key env var (OPENAI_API_KEY, …) + its BASE_URL var
let client = openai::OpenAI::from_env()?;

// 2. From an explicit key, on the shared reqwest transport (needs rig-core/reqwest)
let client = openai::OpenAI::new("your-api-key");
let client = anthropic::Anthropic::new("your-api-key");
let client = ollama::Ollama::new();               // local daemon, optional key

// 3. Config-style for full control (key + base URL + policy):
let client = openai::OpenAIConfig::new("key")
    .with_base_url("https://your-gateway.example.com/v1")
    .client();
let vendor = openai::OpenAIConfig::with_key(&openai::wire::GROQ, "key").client();

// 4. Bring your own transport (rig_core::http_client::HttpClientExt):
let client = openai::OpenAIConfig::new("key").connect(my_http);

// A vendor by name (returns the shared openai::OpenAI on its dialect):
let client = deepseek::new("key");                // == with_key(&DEEPSEEK, key).client()
```

The transport defaults to the shared `rig_reqwest::shared()` client (built once per process). `Config::…client()` is behind the `rig-core` `reqwest` feature — without it you must `.connect(http)`.

## Building models (0.43)

Every client builds wires with provider-appropriate names; the common ones:

| Operation | OpenAI-shaped | Others |
|-----------|---------------|--------|
| completion (dialect's default route) | `openai.completion(id)` → `Model<OpenAiWire>` | `anthropic.completion(id)` (Messages), `gemini.completion(id)` (GenerateContent), `ollama.completion(id)`, `copilot.completion(id)`, `cohere.completion(id)` |
| chat completions explicitly | `openai.chat(id)` → `Model<Chat>` | — |
| Responses API explicitly | `openai.responses(id)` → `Model<Responses>` | — |
| embeddings | `openai.embedding(id, Some(ndims)??)` → `Model<Embeddings>` | same signature on `Gemini`/`Ollama`/`Copilot`/`Cohere`/`VoyageAi` |
| model listing | `openai.list_models().await` → `ModelList { data: Vec<ModelInfo> }` | `anthropic`/`gemini`/`ollama`/`copilot` too (`cohere`/`voyageai` have none) |
| credential check | `openai.verify().await` (401/403 → `ProviderError::InvalidAuthentication`) | — |

Completion routes per dialect: the `OPENAI` and `ChatGPT`-style reasoning dialects default to `Route::Responses`; OpenAI-compatible gateways and Azure's deployment routing default to `Route::Chat`; override with `OpenAIConfig::with_route(route)` or force one wire with `.chat(id)`/`.responses(id)`.

A model erases for storage/agent construction: `model.erase()` → `rig_core::driver::DynModel<Completion>` (`impl From<Model<W, T>>`), so agent builders take `impl Into<DynModel<Completion>>` and accept either form. Clone a model (or the client) to share its transport.

## OpenAI-shaped vendors (0.43)

Each is a dialect of the shared `openai::OpenAI` client; constructors at `providers::<name>::{new, from_env}`:

| Vendor | Dialect | Base URL default |
|--------|---------|------------------|
| DeepSeek | `openai::wire::DEEPSEEK` | api.deepseek.com |
| Groq | `openai::wire::GROQ` | api.groq.com |
| Together | `openai::wire::TOGETHER` | api.together.xyz |
| Hyperbolic | `openai::wire::HYPERBOLIC` | api.hyperbolic.xyz |
| OpenRouter | `openai::wire::OPENROUTER` | openrouter.ai |
| Venice | `openai::wire::VENICE` | api.venice.ai |
| Perplexity | `openai::wire::PERPLEXITY` | api.perplexity.ai |
| Hugging Face | `openai::wire::HUGGINGFACE` (sub-routes via `with_sub_route`) | router.huggingface.co |
| MiniMax | `openai::wire::MINIMAX` (anthropic variant too) | api.minimax.io |
| Mira | `openai::wire::MIRA` | api.mira.network |
| Mistral | `openai::wire::MISTRAL` | api.mistral.ai |
| Moonshot | `openai::wire::MOONSHOT` (anthropic variant too) | api.moonshot.ai |
| Doubleword | `openai::wire::DOUBLEWORD` | api.doubleword.ai |
| xAI | `xai::DIALECT` | api.x.ai |
| Xiaomi MiMo | `openai::wire::XIAOMIMIMO` (anthropic variant too) | api.xiaomimimo.com |
| Z.AI | `openai::wire::ZAI` (also `ZAI_CODING`) | api.z.ai |
| llama.cpp | `openai::wire::LLAMACPP` (optional key: `Auth::OptionalBearer`) | localhost:8080/v1 |
| Azure OpenAI | `openai::wire::AZURE` (deployment routing, `api-version`, `api-key` header or bearer token) | *required* — the resource endpoint |
| ChatGPT (subscription) | `chatgpt::DIALECT` (Codex Responses route, originator/identity headers, device-flow OAuth) | chatgpt.com/backend-api/codex |

Moonshot/MiniMax/Xiaomi/Z.AI additionally expose `anthropic_from_env`/`anthropic_new` constructors for their Messages-format endpoints.

## Auth (OAuth-backed transports)

`chatgpt` and `copilot` resolve tokens through an **`Authenticator`** passed to `openai::OpenAI::authenticate(&authenticator).await` / `copilot::Copilot::authenticate(&authenticator).await` (consumes the client, returns the signed-in one):

```rust
use rig_core::providers::chatgpt::{self, auth::{AuthSource, Authenticator, DeviceCodeHandler}};

// OAuth device flow with a callback that surfaces the prompt:
let auth = Authenticator::new(
    AuthSource::OAuth,
    None,                                  // auth file; None = platform config dir
    DeviceCodeHandler::new(|prompt| {      // prompt.{verification_uri, user_code}
        println!("Visit {} and enter {}", prompt.verification_uri, prompt.user_code);
    }),
    true,                                  // allow_device_flow
);
let chatgpt = openai::OpenAIConfig::with_key(&chatgpt::DIALECT, "")
    .client()
    .authenticate(&auth)
    .await?;

// Pasted credential instead of OAuth:
AuthSource::AccessToken { access_token: "..".into(), account_id: None }     // chatgpt
AuthSource::GitHubAccessToken("..".to_owned())                              // copilot (exchanges for a session)
```

The authenticator reuses its on-disk cache: a valid credential opens without network, an expired one refreshes. ChatGPT requests carry caller identity (`CallerIdentity { originator, user_agent }` on the config — overridable, the dialect reads `CHATGPT_ORIGINATOR` from env) and default instructions (`OpenAIConfig::with_instructions(..)`).

## Capabilities (0.43)

Capabilities moved from compile-time client traits to run-time metadata: each wire's `describe()` reports `Capabilities` (a `wire::Capabilities` value read via `model.capabilities()` / `DynModel::capabilities()`) — maximum batch sizes, supported media, `ProviderCapabilities { composes_native_output_with_tools, rejects_forced_tool_choice }`, etc. What a *client type* supports is its API surface (e.g. `VoyageAi` has only `embedding`/`rerank`); calling an absent method is a compile error, not a runtime failure.

## Data-driven clients

For a provider chosen from configuration data, `rig_core::providers::registry::ProviderRef` builds clients by name (the transport registry). Useful for config-driven apps that don't want a match statement per provider.

## Writing a custom provider (0.43)

1. Define the endpoint policy (paths, auth header, quirks) as a `Dialect` and reuse the OpenAI (`openai::wire`) or Anthropic (`anthropic::wire`) wires — most new providers are a dialect, not a new wire.
2. For genuinely different APIs, implement `Wire` (what to send: `Config`, `Request`/`Decoder`, `Op`) and a `Transport` (or reuse the HTTP transport via `driver::http_transport`), then construct `Model::new(wire, transport)`.
3. Implement `Serve` handlers / use `rig_core::serve` adapters to attach the provider to an agent bus (see `agent-runner.md`).

Guide: https://rig.rs/docs/guides/extension/write_your_own_provider

## Project boundary (shuvarie)

`shuvarie-llm` collapses rig's client types into its own `ListImpl` enum (7 client families — OpenAI/Anthropic/Gemini/Ollama/Copilot/Cohere/VoyageAi — plus an OAuth-pending gate) keyed by `shuvarie_llm::ProviderKind` (the `selune::ProviderType` plus an optional `selune::Dialect` — Selune 0.4 folds the OpenAI-compatible vendors under `openai-compat` + `dialect`), so the TUI never names a provider concretely and OpenAI-shaped vendors share one arm. Add new providers in `crates/llm/src/provider.rs`, not in the root binary; the root binary drives the core task which drives `shuvarie-llm`.