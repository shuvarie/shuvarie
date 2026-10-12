# Providers & Clients

A **provider client** is the entry point to any LLM vendor in Rig. It holds a provider's **configuration** (credentials, base URL, routing/dialect policy) on a **transport** (how payloads travel) and builds the **models** you call — completion, embedding, listing, rerank, transcription, image, audio models — depending on what the provider serves.

Official docs: https://rig.rs/docs/concepts/provider_clients · API: https://docs.rs/rig-core/latest/rig_core/providers/index.html — for 0.44 read the crate sources under `~/.cargo/registry/src/*/rig-core-0.44.0/src/providers` as well; the modules are heavily documented.

## Vocabulary

- **Provider module** — the vendor's module in `rig_core::providers` (e.g. `providers::openai`). Holds the client, its config, wire types, dialect constants, model ids, its `extension` (typed options + reply extras) and the request/response normalization.
- **Config** — plain data describing the provider: `openai::wire::OpenAIConfig` (re-exported as `openai::OpenAIConfig`), `anthropic::wire::AnthropicConfig`, `gemini::GeminiConfig`, `ollama::wire::OllamaConfig`, `copilot::wire::CopilotConfig`, `cohere::wire::CohereConfig`, `voyageai::wire::VoyageAiConfig`. Carries the credential (`rig_core::wire::Secret`, never serialized), the base URL, and dialect/policy fields. Serializable (`deny_unknown_fields`) and cheap to clone.
- **Client** — the config on a transport, e.g. `openai::OpenAI` (`Config::client()` / `Config::connect(http)` / `client.with_http(http)` / `client.config()`). Builds models.
- **Wire** — what to send and how to read the reply: `openai::wire::{OpenAiWire, Chat, Embeddings, Rerank, Transcriptions, Models, Verify, …}`, `anthropic::wire::Messages`, `gemini::completion::GenerateContent`, `copilot::wire::CopilotWire`, `ollama::Chat` (native `/api/chat`), `cohere::{CohereChat, NativeChat}`. Each wire declares its `Operation` (`Chat::Op = Completion`).
- **Model** — a `rig_core::driver::Model<W, T = rig_core::http_client::DynHttpClient>` (e.g. `Model<OpenAiWire>` from `openai.completion(id)`): `call(request)` (one whole reply, folded), `stream(request)` → `Streamed<W::Op>` (part events; `finish()`/`partial()` fold or read it), `erase()` → `DynModel<Op>`.
- **Dialect** — the request/response policy of one vendor that speaks another provider's format: one constant per vendor in `openai::wire` (`GROQ`, `TOGETHER`, `DEEPSEEK`, `OPENROUTER`, `AZURE`, `LLAMACPP`, … — plus `xai::DIALECT`, `chatgpt::DIALECT`, `copilot::wire::DIALECT`) and in `anthropic::wire` (`ANTHROPIC`, `ZAI`, `MINIMAX`, `MOONSHOT`, `XIAOMIMIMO`). Built with `Dialect::gateway(name, base_url, api_key_env)` + `Quirks`; `by_name`/`all` resolve the registered set, and a dialect serializes as its registered name only. The vendor modules' `new`/`from_env` build the shared client on their dialect.

Flow is always: **config → client → model → agent/extractor/index**.

## Building clients

```rust
use rig_core::providers::{openai, anthropic, ollama};
use rig_core::providers::deepseek;

// 1. From the provider's key env var (OPENAI_API_KEY, …) + its BASE_URL var
let client = openai::OpenAI::from_env()?;

// 2. From an explicit key, on the shared reqwest transport (needs rig-core/reqwest)
let client = openai::OpenAI::new("your-api-key");    // == OpenAIConfig::new(key).client()
let client = anthropic::Anthropic::new("your-api-key");
let client = ollama::Ollama::new();                  // local daemon, optional key

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

`with_key`/`new`/`from_env` read the credential the dialect's `api_key_env` names (a missing one is an `EnvError`; Azure accepts `AZURE_API_KEY` or, when only that is set, `AZURE_TOKEN`, each with its own header policy), then the base-URL override. The override lives in two places: `Dialect::base_url_env` and a second spelling in `Quirks::base_url_env_alias` (read when the first is unset).

| Dialect | `base_url_env` | `base_url_env_alias` |
|---------|----------------|----------------------|
| `OPENAI` | `OPENAI_BASE_URL` | — |
| `AZURE` | `AZURE_ENDPOINT` | — (also requires `AZURE_API_VERSION`; speech: `AZURE_AUDIO_API_VERSION`) |
| `chatgpt::DIALECT` | `CHATGPT_API_BASE` | `OPENAI_CHATGPT_API_BASE` |
| `copilot::wire::DIALECT` | `GITHUB_COPILOT_API_BASE` | `COPILOT_BASE_URL` |
| `LLAMACPP` | `LLAMACPP_API_BASE_URL` | — |
| `VENICE` | `VENICE_BASE_URL` | — |
| `DOUBLEWORD` | `DOUBLEWORD_BASE_URL` | — |
| `ZAI` / `ZAI_CODING` | `ZAI_API_BASE` | — |
| `MINIMAX` / `MINIMAX_CHINA` | `MINIMAX_API_BASE` | — |
| `MOONSHOT` / `MOONSHOT_CHINA` | `MOONSHOT_API_BASE` | — |
| `XIAOMIMIMO` | `XIAOMI_MIMO_API_BASE` | — |
| `anthropic::wire::ANTHROPIC` | `ANTHROPIC_BASE_URL` | — |
| `anthropic::wire::{ZAI, MINIMAX, MOONSHOT, XIAOMIMIMO}` | `ZAI_ANTHROPIC_API_BASE`, `MINIMAX_ANTHROPIC_API_BASE`, `MOONSHOT_ANTHROPIC_API_BASE`, `XIAOMI_MIMO_ANTHROPIC_API_BASE` | — |
| `DEEPSEEK`, `GROQ`, `TOGETHER`, `HYPERBOLIC`, `MIRA`, `PERPLEXITY`, `HUGGINGFACE`, `MISTRAL`, `OPENROUTER`, `COHERE`, `OLLAMA`, `xai::DIALECT` | none — the dialect's default stands | — |

Configs that are not OpenAI-shaped read their own variables: `OllamaConfig::from_env` (`OLLAMA_API_BASE_URL`, `OLLAMA_API_KEY`; both optional — an unset key sends no credential), `CopilotConfig::from_env` (`GITHUB_COPILOT_API_KEY` else `COPILOT_API_KEY`, plus the two base URLs above), and `GeminiConfig`/`CohereConfig`/`VoyageAiConfig::from_env` read a key only (`GEMINI_API_KEY`, `COHERE_API_KEY`, `VOYAGE_API_KEY`).

Beyond credentials: `with_base_url(..)`, `with_dialect(..)` (same credential, another dialect's defaults), `with_auth(Auth::{Bearer, OptionalBearer, ApiKeyHeader})`, `with_sub_route(SubRoute::…)` and `with_api_version(..)` on `OpenAIConfig`; `with_version`/`with_beta` on `AnthropicConfig`; `with_api_key` on `OllamaConfig`. The `providers::registry` umbrella adds the general `ProviderConfig::base_url()` / `with_base_url(..)` surface (**new in 0.44**, #2700).

## Building models

Every client builds wires with provider-appropriate names; the common ones:

| Operation | `openai::OpenAI` | Others |
|-----------|-----------------|--------|
| completion (the dialect's default route) | `openai.completion(id)` → `Model<OpenAiWire>` | `anthropic.completion(id)` → `Model<Messages>`, `gemini.completion(id)` → `Model<GenerateContent>`, `ollama.completion(id)` → `Model<Chat>` (the `/v1` API), `copilot.completion(id)` → `Model<CopilotWire>`, `cohere.completion(id)` → `Model<CohereChat>` |
| chat completions explicitly | `openai.chat(id)` → `Model<Chat>` | `ollama.native_completion(id)` → `Model<ollama::Chat>` (native `/api/chat`) |
| Responses API explicitly | `openai.responses(id)` → `Model<Responses>` | Copilot's `codex` models (through the dialect's model-route hook) |
| embeddings | `openai.embedding(id, Option<usize>)` → `Model<Embeddings>` | same signature on `gemini`, `ollama`, `copilot`, `cohere`, `voyageai`; `cohere.image_embedding()` takes no model; `anthropic` has none |
| rerank | `openai.rerank(id)` → `Model<Rerank>` (only the `LLAMACPP` dialect serves it) | `voyageai.rerank(id)` |
| transcription | `openai.transcription(id)` → `Model<Transcriptions>` | `gemini.transcription(id)` |
| image generation (`image` feature) | `openai.image_generation(id)` → `Model<Images>` | `gemini.image_generation(id)` |
| audio generation (`audio` feature) | `openai.audio_generation(id)` → `Model<Speech>` | — |
| model listing | `openai.list_models().await` → `ModelList { data: Vec<ModelInfo> }` | `anthropic`/`gemini`/`ollama`/`copilot` too; `cohere`/`voyageai` have none |
| credential check | `openai.verify().await` (401/403 → `ProviderError::InvalidAuthentication`) | `anthropic.verify()`, `gemini.verify()`; no check on `ollama`/`copilot`/`cohere`/`voyageai` |
| Gemini only | — | `gemini.interactions(id)`, `interaction(id)`/`interaction_resumed(id, last_event_id)`, `cached_contents()` |

Which clients serve less than the table suggests: **`anthropic::Anthropic`** serves `completion` (Messages), `list_models` and `verify` only; **`cohere::Cohere`** serves `completion`, `embedding` and `image_embedding`; **`voyageai::VoyageAi`** serves `embedding` and `rerank`. Calling an absent method is a compile error, not a runtime failure.

Completion routes per dialect: the `OPENAI`, xAI and ChatGPT dialects default to `Route::Responses`, and Copilot's dialect routes each model (`codex` → Responses, otherwise Chat); every other compatible dialect defaults to `Route::Chat`. An explicit `OpenAIConfig::with_route(Route::Chat | Route::Responses)` wins over both, and `.chat(id)`/`.responses(id)` force one wire; `OpenAIConfig::completion_route()` reports the configured-or-static default.

Two routes are opt-in (**changed in 0.44**):

```rust
// Cohere: the default is the OpenAI Compatibility API; the native /v2/chat API is opt-in.
// ChatRoute::Auto sends a request that carries documents to the native API, so Cohere
// grounds the answer in them and keeps its citations and tool plan.
let mut model = cohere::Cohere::from_env()?.completion(cohere::COMMAND_A_03_2025);
model.wire = model.wire.with_route(cohere::ChatRoute::Auto);

// Ollama: `completion` is the daemon's OpenAI-compatible /v1/chat/completions (it takes
// `keep_alive` and a request's `reasoning` as `reasoning_effort`, and refuses `num_ctx`
// and `options`); `native_completion` is /api/chat, which takes `think`, `keep_alive`
// and model `options`.
let native = ollama::Ollama::new().native_completion(ollama::QWEN3_5);
```

A model erases for storage/agent construction: `model.erase()` → `rig_core::driver::DynModel<Completion>` (`impl From<Model<W, T>>`), so agent builders take `impl Into<DynModel<Completion>>` and accept either form. Clone a model (or the client) to share its transport.

## Provider extensions (new in 0.44)

A provider that ships typed options has a `providers::<p>::extension` module with a `ProviderExtension` marker (`OpenAiExt`, `AnthropicExt`, `OllamaExt`, `OpenRouterExt`, `VeniceExt`, …), an `…Options` type implementing `ExtensionOptions`, and an `…Extras` type implementing `ReplyExtras`.

- **Sections.** Options serialize to an object of sections: `SHARED` (`"*"`) for fields every route spells the same, plus one section per route API (`"openai.chat"`, `"openai.responses"`, `"ollama.chat"`). A wire merges `"*"`, then the section of the route it encodes, at the top level of the body — above the mapped `GenerationOptions` and below `additional_params` — and skips sections of other routes.
- **Attaching.** `CompletionRequest::provider_option(options)` stores them under their own provider key (`<O::Ext as ProviderExtension>::PROVIDER`), keeping other providers' entries; `CompletionRequest::provider_options(ProviderOptions)` replaces every entry; `ProviderOptions::{new, set::<O>, insert::<P>, with::<P>, get::<P>, remove::<P>, contains::<P>, is_empty, overlay}` work on the store directly. A third-party provider that reuses a built-in options type must store it with `with::<P>` or its wire never reads it. Options that do not serialize fail the request's encode (`OptionsError`).
- **Precedence, lowest first: mapped typed options → `provider_options` → `additional_params`.** `additional_params` objects deep-merge key by key into the wire body and a raw `null` is sent as `null`, so raw params still win over both typed layers.
- **Refusal checks.** `ExtensionOptions::unsupported(target, request) -> Vec<(&'static str, String)>` names each field the target model cannot take; the request's `OnUnsupported` policy (`Error` by default, `Ignore`) decides what happens, surfacing `UnsupportedOption` on failure.
- **Extras.** `CompletionResponse::extras::<P>() -> Option<Result<P::Extras, serde_json::Error>>` (`None` unless `P` produced the reply) and `extras_lossy::<P>() -> P::Extras` (the empty view on any failure) read the reply's `raw` document.

```rust
use rig_core::completion::CompletionRequest;
use rig_core::providers::anthropic::extension::{AnthropicOptions, Speed, TaskBudget};
use rig_core::providers::ollama::extension::{KeepAlive, OllamaExt, OllamaOptions};

let request = CompletionRequest::new("hi")
    .provider_option(
        AnthropicOptions::default()
            .top_k(40)
            .task_budget(TaskBudget::new(50_000))   // + TASK_BUDGETS_BETA
            .speed(Speed::Fast),                   // fast mode: Opus 5.5/5/4.8, + FAST_MODE_BETA
    )
    .provider_option(
        OllamaOptions::default()
            .keep_alive(KeepAlive::duration("5m"))   // both routes
            .num_ctx(8192)                           // /api/chat only
            .top_k(40),
    );

let extras = response.extras_lossy::<OllamaExt>();   // done_reason, eval_count, logprobs, …
let done_reason = extras.done_reason;
```

| Provider | Marker | Options (selected fields) | Extras |
|----------|--------|---------------------------|--------|
| Anthropic | `AnthropicExt` | `top_k`, `metadata_user_id`, `inference_geo`, `speed`, `task_budget`, `fallbacks`, `container`, `context_management`, `mcp_server(s)`, `diagnostics_previous_message_id` (several need an `anthropic-beta` flag: `FAST_MODE_BETA`, `TASK_BUDGETS_BETA`, `CONTEXT_MANAGEMENT_BETA`, …) | `AnthropicExtras` |
| Ollama | `OllamaExt` | shared: `keep_alive`; `"ollama.chat"`: `num_ctx`, `num_keep`, `top_k`, `min_p`, `repeat_penalty`, `repeat_last_n`, `num_gpu`, `num_thread`, `logprobs`, `top_logprobs` | `OllamaExtras`: `done_reason`, `created_at`, duration/count fields, `logprobs` |
| OpenAI | `OpenAiExt` | shared: `store`, `metadata`, `prompt_cache_key`, `safety_identifier`; `"openai.chat"`: `ChatOptions` (`logit_bias`, `prediction`, penalties, …); `"openai.responses"`: `OpenAiResponsesOptions` (`reasoning_summary`, `include`, `conversation`, `truncation`, `background`, `max_tool_calls`) | `OpenAiExtras`: `service_tier`, reasoning fields, cache retention, `phases`, `incomplete_reason`, … |
| OpenRouter | `OpenRouterExt` | routing preferences/fallbacks/session id (`OpenRouterOptions`, `ProviderPreferences`, `ProviderSortStrategy`) | cost |
| Venice | `VeniceExt` | `venice_parameters` | cost + the parameters the reply echoes |

Markers ship for `openai` (`openai::extension`), `anthropic`, `azure`, `chatgpt`, `cohere`, `copilot`, `deepseek`, `gemini`, `groq`, `llamacpp`, `minimax`, `mistral`, `moonshot`, `ollama`, `openrouter`, `perplexity`, `together`, `venice`, `xai`, `xiaomimimo` and `zai`.

## Model catalog (new in 0.44)

`rig_core::catalog::{Catalog, ModelSpec, Modalities, ReasoningSupport, CacheSupport, Pricing, Sampling, CatalogError}` — model facts by provider and id: context/output limits, input modalities, the reasoning and caching a model takes, tools/structured output, list prices, sampling policy and deprecation.

| Call | What it does |
|------|--------------|
| `Catalog::builtin()` | this build's `&'static Catalog`, from the checked-in `models.json` |
| `Catalog::get(ProviderId, model)` / `find(ProviderId, model)` | exact id / as the encoders look it up (exact, else without a dated snapshot suffix) |
| `Catalog::resolve("anthropic/claude-opus-5-5")`, `resolve("deepseek:deepseek-chat")` | `vendor/model` or `vendor[/format]:model` |
| `Catalog::iter()` | every model, by vendor then id |
| `Catalog::from_json(&str)` | read models.dev-shaped data (`{provider: {models: {…}}}`; rig's hand-entered overrides sit under each row's `rig` key) |
| `Catalog::merge(overrides)` | lay an override file over the built-in one: a model in both keeps every field the override leaves out |
| `ModelSpec::validate(&GenerationOptions) -> Result<(), UnsupportedOption>` | refuse a `reasoning`/`cache` the model does not take, before anything is sent |
| `Pricing::cost(&Usage) -> Option<Cost>` | what the reported counters cost (standard-tier list price; `Some` only when input and output are both reported) |

`ModelSpec` carries `id`, `provider`, `display_name`, `context_window`, `max_output_tokens`, `input` (`Modalities { text, image, audio, video, pdf }`), `reasoning` (`ReasoningSupport { supported, levels, budget, can_disable, default }`), `caching` (`CacheSupport { retention }` — empty means unknown, so nothing is refused), `tools`, `structured_output`, `pricing: Option<Pricing>`, `deprecated`, `sampling: Option<Sampling::{Any, ReasoningOff, Never}>` and a doc-hidden `compat` record for facts no portable field holds.

The data is **generated**: `cargo xtask catalog sync` reads models.dev and rewrites `models.json` (the main providers' rows are hand-reviewed). models.dev keys are mapped to rig vendor names (`google` → `gcp.gemini`, `google-vertex` → `vertexai`, `amazon-bedrock` → `aws_bedrock`, `azure` → `azure.openai`, `github-copilot` → `copilot`, `ollama-cloud` → `ollama`, `togetherai` → `together`, `moonshotai` → `moonshot`, `xiaomi` → `xiaomimimo`).

The wires consult it wherever a per-model fact decides the request: image-input gating (`catalog::reads_images_or`, with each vendor's naming rule for an unlisted id), whether a model thinks (Cohere's `thinks`), a snapshot id resolving to its undated entry, and per-model option refusals (Anthropic's `AnthropicOptions::unsupported` reads the model's spec). `ModelSpec::validate` is the caller-side pre-flight check for the typed generation options — and the wired-in refusals only run when a request's options are not default (`GenerationOptions::is_default`), so an untouched request is never checked against the catalog.

## Capabilities

What a wire supports is run-time metadata, read before a call: `Wire::describe()` returns a `Descriptor { replay, name, model, capabilities, telemetry }`, and `model.capabilities()` / `DynModel::capabilities()` return its `wire::Capabilities`:

- `completion: ProviderCapabilities` — defined in `completion/request.rs`: `composes_native_output_with_tools` (native structured output can stay on with tool calls) and `rejects_forced_tool_choice` (rig then does not force the extractor's output tool). Build with `ProviderCapabilities::new()` + `with_native_output_tool_composition` / `with_forced_tool_choice_rejected`.
- `max_documents` — the batch limit of one embedding or rerank request; `ndims` — the width the reply must match; `declared` — the width the caller asked for (`embedding(id, Some(n))` declares one, and a reply of another width is `ProviderError::MismatchedDimensions`; zero or `None` disables the check).

Only the operation that names a field reads it: a completion reads `completion`, an embedding/rerank reads the batch and width fields, and the others leave them at their defaults. What a *client type* serves is its API surface (see the table above): calling an absent method is a compile error, not a runtime failure.

## Data-driven clients

For a provider chosen from configuration data, `rig_core::providers::registry` builds clients and models by name, credentials supplied at execution:

- **`ProviderConfig`** — a configuration tagged by family: `OpenAi(openai::wire::OpenAIConfig)` / `Anthropic(anthropic::wire::AnthropicConfig)` / `Gemini(GeminiConfig)`, serializable and self-describing. `id()`, `vendor()`, `format()`, `is_unauthenticated()` (a `Secret` serializes without its value), `with_credential(api_key)`, `base_url()`, `with_base_url(..)`, and `completion_handler(label, model, http) -> ErasedHandler` for a bus.
- **`ProviderId`** — a validated `(vendor, Format)` pair: `Format::{OpenAi, Anthropic, Gemini}` (`as_str`, `named`, `ALL`); `ProviderId::{new, resolve("vendor[/format]"), all, vendor_selections, catalog(vendor), api_key_env, requires_credential}`. A bare vendor resolves only when this build registers one family for it; otherwise `SelectionError::{Unknown, Unregistered, Ambiguous, UnknownFormat, Malformed}`.
- **`Provider` / `ProviderRef`** — a nonempty model id paired with a registered preset or an explicit configuration. `ProviderRef::{registered(id, model), configured(config, model), parse("vendor[/format]:model")}`, `model()`, `provider()`, `config(api_key)`, `completion_model()` (credential from the environment, shared reqwest) and `completion_model_with(api_key, http)`. Both fields are private in 0.44: a parsed reference is read-only, so a credential cannot be inserted afterwards.
- **`ModelSelector<'a>` → `connect`** — `ModelSelector::{Spec(&ModelSpec), Reference(&str)}` with `From<&ModelSpec>`/`From<&str>`/`From<&ModelRef>`; `connect(selector, api_key)` → `DynModel<Completion>` on the shared reqwest client, or `connect_with(selector, api_key, http)`. A provider only the catalog knows (`aws_bedrock`, `vertexai`, `gemini-grpc`, `candle`, `voyageai`) fails with `ConnectError::CatalogOnly { vendor, served_by }`; a bad reference is `ConnectError::{Malformed, Reference}`.

```rust
use rig_core::catalog::Catalog;
use rig_core::providers::registry::{ProviderRef, connect};

let model = connect("anthropic/claude-opus-5-5", "sk-ant-...")?;   // DynModel<Completion>
let spec = Catalog::builtin().resolve("openai/gpt-5.5").ok_or("listed")?;
let other = connect(spec, "sk-...")?;
let reference = ProviderRef::parse("deepseek:deepseek-chat")?;    // "deepseek/openai:deepseek-chat"
```

## Writing a custom provider

1. Define the endpoint policy (paths, auth header, quirks) as a `Dialect` and reuse the OpenAI (`openai::wire`) or Anthropic (`anthropic::wire`) wires — most new providers are a dialect, not a new wire. A dialect serializes as its registered name, so a host's own dialect must be kept out of persisted-by-name config (store the base URL on the configuration instead) or registered in the crate.
2. For a genuinely different API, implement `wire::Wire`: `type Op: Operation` (`Completion`, `Embedding`, `ImageEmbedding`, `Rerank`, `Transcription`, `ModelListing`, `Verify`, `ImageGeneration`, `AudioGeneration`), `type Payload`/`type Frame` (name `wire::Encoded` + `wire::WireFrame` to reuse the HTTP transport), `type Decoder<'id>` (`classify(frame) -> WireEvent`, `decode(event, out) -> Flow`, `eof`), `type Reassembler` (`wire::document::Reassemble<Frame> + Serves<Op>`, or `document::Unreassembled` when the decoders record `raw` themselves — which a completion wire cannot), and `describe()`/`encode()`. Then `Model::new(wire, transport)`.
3. To make a new operation, implement `wire::Operation`: `Request`, `Event`, `End`, `Response`, `Fold` and `Emit` (`Free` for events the decoder writes, `Assembled` for a completion's part handles), plus `fold(request, &mut Call)` and the optional `prepare(request, &Descriptor)` rejection hook. A whole-reply operation can use `operation::Whole<Self>` and end with `out.end(end)`; the driver folds events, the end and the reply facts into the response.
4. Any `HttpClientExt` is already a `Transport` for such a wire (`driver::http_transport` is crate-private in 0.44). Another runtime, proxy or mock implements `driver::Transport::{send(payload, Exchange) -> Opening<Frame>}` and answers with `Opened::new(frames)` plus `with_request_id`/`with_http`/`with_route`/`with_document`.
5. Implement `Serve` handlers / use `rig_core::serve` adapters to attach the provider to an agent bus (see `agent-runner.md`).

Guide: https://rig.rs/docs/guides/extension/write_your_own_provider

## Project boundary (shuvarie)

`shuvarie-llm` collapses rig's client types into its own private `ListImpl` enum (7 client families — OpenAI/Anthropic/Gemini/Ollama/Copilot/Cohere/VoyageAi — plus a `Pending` arm gated by an OAuth sign-in) keyed by `shuvarie_llm::ProviderKind` (the `selune::ProviderType` plus an optional `selune::Dialect` — Selune 0.4 folds the OpenAI-compatible vendors under `openai-compat` + `dialect`), so the TUI never names a provider concretely and OpenAI-shaped vendors share one arm. Add new providers in `crates/llm/src/provider.rs`, not in the root binary; the root binary drives the core task which drives `shuvarie-llm`.

`shuvarie-decision` (`crates/decision/Cargo.toml`) uses `rig-core` for errors/usage and `rig-typesafeai = { version = "0.44", features = ["reqwest"] }` for its own System One wire. Rig's `rig` facade **is** published for 0.44 (`rig::…` re-exports `rig_core`, `rig::agent` re-exports `rig_agent`), but this workspace still depends on `rig-core`/`rig-agent` directly by choice — the workspace manifest says `Do NOT include rig. It has too many implicit dependencies.`
