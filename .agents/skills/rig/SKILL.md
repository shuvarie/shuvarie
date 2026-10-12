---
name: rig
description: >-
  You are an expert in Rig, the Rust library for building scalable, modular, and
  ergonomic LLM-powered applications. You help developers wire up provider
  clients (OpenAI, Anthropic, Gemini, Ollama, and 20+ others) built as configs
  on wires and transports, set typed generation and per-provider options against
  a model catalog, build agents with tools and RAG context, drive runs with
  AgentRunner streams, extract typed structured output, manage conversation
  memory, and hook the agent loop for guardrails and approvals.
  This skill covers Rig 0.44.x (`rig-core` and `rig-agent` depended on directly;
  the `rig` facade is published for 0.44 but shuvarie deliberately avoids it) as
  used by the `shuvarie-llm` crate, with `shuvarie-decision` on `rig-core` +
  `rig-typesafeai`. Docs: guide at https://rig.rs/docs and API docs at
  https://docs.rs/rig-core/0.44.0/rig_core/.
license: MIT
metadata:
  author: shuvarie
  version: 0.44.0
  category: Backend Development
  tags:
    - rust
    - llm
---

# Rig — LLM-Powered Applications in Rust

Rig is a Rust library for building LLM-powered applications and agents. It gives you unified abstractions over **model providers** (OpenAI, Anthropic, Gemini, Ollama, and 20+ others), **tools**, **memory**, and **vector stores / RAG**, so you can wire up an agent in a few lines and scale to a production system without changing frameworks.

This project (`shuvarie`) pins **Rig 0.44.0** — `rig-core` and `rig-agent` directly in `crates/llm/Cargo.toml` (`rig-core` with `features = ["reqwest"]`; the `rig` facade crate *is* published for 0.44 but shuvarie deliberately avoids it: "Do NOT include `rig`. It has too many implicit dependencies."). `crates/decision` depends on `rig-core` plus `rig-typesafeai` (its own System One wire). The `shuvarie-llm` crate is a thin wrapper over rig that exposes a `ProviderKind`-keyed `ProviderClient` (`selune::ProviderType` + an optional `selune::Dialect`) dispatching onto seven client families, plus model listing, embeddings, worker agents, and a streaming completion API; the root binary never calls `rig` directly (see `AGENTS.md`).

> **What 0.44 gives you**
> 1. **Provider clients are named per provider** (`openai::OpenAI`, `anthropic::Anthropic`, `gemini::Gemini`, `ollama::Ollama`, `copilot::Copilot`, `cohere::Cohere`, `voyageai::VoyageAi`) and built from **configs**: `OpenAIConfig::new(key).client()` / `Config::from_env()?.client()` / `.connect(http)`. OpenAI-shaped **vendors** (Groq, Together, DeepSeek, …) are *dialects*: their modules expose constructors that return the shared `OpenAI` client on their dialect (e.g. `openai::wire::GROQ`), so an OpenAI- or Anthropic-shaped client covers all of them. 0.44 harmonized the per-provider constructor story (Anthropic dialect, Bedrock builder, Vertex errors).
> 2. **Typed generation options** — `GenerationOptions` (`reasoning`, `cache`, `service_tier`, `verbosity`, `parallel_tool_calls`, `top_p`, `seed`, `stop`, `on_unsupported`) maps every wire's request-body knobs, and **typed per-provider options** (`ProviderOptions` + each provider's `extension` module) carry what no portable option covers. Precedence, lowest first: **mapped options → `provider_options` → `additional_params`**.
> 3. **A model catalog** — `rig_core::catalog::{Catalog, ModelSpec, Pricing}` knows per-model context/output limits, modalities, the reasoning and caching a model takes, and its prices, so `ModelSpec::validate(&options)` refuses an option a model does not take *before anything is sent*, and `Pricing::cost(&usage)` prices a `Usage`.
> 4. **A model is a `Model<Wire, Transport>`** (`rig_core::driver::Model`): a wire (what to send: `Chat`, `Responses`, `Messages`, `GenerateContent`, `Embeddings`, …) on a transport (how it travels; the erased shared client by default). Clients build models: `openai.chat("gpt-5")`, `openai.completion("gpt-5")` (the dialect's default completion route — Responses for OpenAI/ChatGPT, chat completions for compat vendors), `anthropic.completion("claude-…")`. A model erases to **`DynModel<Op>`** (`model.erase()`; the agent builder takes `impl Into<DynModel<Completion>>`).
> 5. **Replies are normalized.** Every wire decodes and folds into one `CompletionResponse` (`choice`, `usage`, `origin`, `finish_reason`/`stop`, `error`, `aborted`, `provider_request_id`, `raw`) with typed accessors (`text()`, `reasoning()`, `message()`, `head()`, `tool_calls()`, `extras::<P>()`) and provider extensions on the response too (`ReplyExtras`). `FinishReason` is a closed vocabulary plus `Other(String)`; whether an unknown reason is fatal is the caller's choice (`accept_unknown_finish_reasons`), never silent.
> 6. **Assistant history is item-shaped with provenance**: `Message::Assistant(AssistantMessage { content, origin, stop })`. Reasoning is plain (`AssistantContent::Reasoning(Reasoning)` — the 0.43 `Sealed`/`Issuer` machinery is gone), and identities are typed (`ToolName`, `CallId`).
> 7. **Agent runs are `AgentRunner`s built by `agent.prompt(msg)`** — now generic (`AgentRunner<O = ()>`, with `TypedRun<T> = AgentRunner<TypedOutput<T>>`) so a typed run shares every per-run setter. Configure then drive: `.await` (blocking → `PromptResponse`), `.stream()` (→ `StreamingResult`), or `.run_channel()` (→ `RunEvents`).
> 8. **One error type for a run**: there is no `StreamingError` — a stream is `Result<MultiTurnStreamItem, PromptError>`, and `PromptError::{Provider, Report, Memory, MaxTurns, Cancelled, UnknownToolCall}` covers the loop.
> 9. **Streams are part-based and tool-call arguments stream as they arrive**: `MultiTurnStreamItem::{StreamAssistantItem, ToolCall, ToolExecutionCommitted, ToolResult, CompletionCall, ModelTurnRetried, FinalResponse}`, with `StreamEvent::{Start, Text, Reasoning, Arguments, End}` fragments inside `StreamAssistantItem`. Tool results come back as plain `ToolResult { tool_result }` (0.43's `StreamUserItem`/`StreamedUserContent` are gone).
> 10. **Tools split into `Tool` (contextual) and `PortableTool` (context-free)**; failures are typed end-to-end (no string flattening), `ToolContext` values are serde-backed `ContextValue`s, and malformed tool arguments are offered to the invalid-call hook with bounded retries (`max_consecutive_malformed_tool_calls`).
> 11. **`schemars` v1.0 is required** for `JsonSchema` derives (tool args, structured output); field descriptions are `///` doc comments and the macro is `schemars::schema_for!`.

Official docs: **Guide** at https://rig.rs/docs and **API docs** at https://docs.rs/rig-core/0.44.0/rig_core/ / https://docs.rs/rig-agent/0.44.0/rig_agent/. docs.rs now serves the 0.44 line; the crate sources under `~/.cargo/registry/src/*/rig-core-0.44.0` / `rig-agent-0.44.0` are the most precise reference (the modules are heavily documented), and `rig-core-0.44.0/TYPED_OPTIONS.md` explains the intent behind the typed-option/catalog work.

## Workspace / crate layout

Rig separates portable provider/backend contracts from the agent runtime:

- `rig-core` (**`rig_core`**) — portable contracts: `catalog`, `wire` (configs, dialects, endpoint wires, `Operation`/`Fold`/`Decoder`), `driver` (`Model`/`DynModel` + transports), `completion` (requests/responses/messages/options/provider-options/history), `streaming` (part events), `tool` (Tool/PortableTool contracts, contexts, results), `embeddings`, `rerank`, `vector_store`, `memory`, `model` (listing), `error` (`ProviderError`, `ErrorReport`, `ErrorKind`), `effect` (typed effect bus contracts), `serve`, `transcript`, `structured_output`, `observe`, and the provider modules. Default features: `derive` + `rustls`; enable `reqwest` for the shared transport behind `Config::…client()`.
- `rig-agent` (**`rig_agent`**) — the classic agent runtime: `agent` (`Agent`/`AgentBuilder`, `AgentRunner`, typed hooks, tools registry/server/catalog, `TypedRun`), `run` (the sans-IO run vocabulary: `AgentRun`, `RunSpec`, policy, patch, output mode, streamed-turn assembly), `extractor` (typed extraction), `bus`/`integrations`, and `rig_agent::core` (re-exports `rig_core::*`). Default feature: `derive`; `test-utils` enables the mock models and conformance helpers (`rig_agent::test_utils` re-exports `rig_core::test_utils`).
- Companion crates are split per backend/transport (`rig-http`, `rig-reqwest`, `rig-tungstenite`, `rig-typesafeai`, …) and per integration (lancedb, qdrant, mongodb, neo4j, milvus, postgres, sqlite, s3vectors, scylladb, vectorize, helixdb, fastembed, …). Vector-store/memory integrations **are** republished for 0.44 (unlike 0.43). MCP lives in its own crate (`rig-rmcp`).

```toml
[dependencies]
rig-core = { version = "0.44", features = ["reqwest"] }
rig-agent = "0.44"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync"] }
```

Do **not** depend on the `rig` facade in this repo; depend on `rig-core`/`rig-agent` directly. In this repo the LLM dependency lives in `shuvarie-llm` (and `rig-core`/`rig-typesafeai` in `shuvarie-decision`).

## Critical Rules

Before writing Rig code in this repo, know these constraints:

- **The root binary never calls `rig` directly.** All provider access goes through `shuvarie-llm` (see `AGENTS.md`). Put LLM/SDK glue in `crates/llm/`, not in the TUI.
- **Library crates use typed errors, not `eyre`.** `shuvarie-llm` defines `LlmError` (thiserror). Don't bubble `anyhow`/`eyre` out of `shuvarie-core` or `shuvarie-llm`; `color-eyre` is binary-only.
- **`shuvarie-llm` must stay side-effect-free w.r.t. the TUI.** Long-running LLM work runs on the spawned core task over `tokio::sync::mpsc`; never `await` inside the TUI loop. The LLM layer exposes async APIs the core task drives.
- **Streaming tokens belong on the core task.** Drive `agent.prompt(..).stream()` on the core task and forward mapped `MultiTurnStreamItem`s to the TUI via the existing channel; do not pull a stream from `handle_event`/`view`.
- **Clients are configs on transports.** Build with `<X>Config::new(key)` / `<X>Config::from_env()` / `OpenAIConfig::with_key(&DIALECT, key)` (+ `.with_base_url(..)`) then `.client()` (shared HTTP client) or `.connect(http)`. `.build()`-style fallible builders are gone.
- **OpenAI-shaped vendors are dialects of one client.** Reach them by dialect when you need a base-URL override: `OpenAIConfig::with_key(&openai::wire::GROQ, key)…` (see `references/providers-and-clients.md` for the full table).
- **Agents are built from models, not clients.** `agent.completion(id).erase()` → `AgentBuilder::new(model)…build()`; there is no `AgentClientExt`/`client.agent(..)`.
- **Options have a fixed precedence**: mapped `GenerationOptions` → `ProviderOptions` → `additional_params`. `additional_params` objects deep-merge key by key into the body and a raw `null` **value** is sent as `null` (an `additional_params` of `null` means "no raw layer"), so raw keys override typed fields.
- **`OnUnsupported::Error` is the default.** An option a model does not take fails the call unless you set `GenerationOptions::on_unsupported(OnUnsupported::Ignore)` or pre-flight with `ModelSpec::validate`/`Catalog`.
- **`agent.prompt(msg)` returns an `AgentRunner` = the run.** Set `.history(..)` (bypasses memory), `.max_turns(..)`, `.tool_concurrency(..)`, `.add_hook(h)`, `.reasoning(..)`, `.provider_option(..)`, …, then `.run()`/`.await` → `Result<PromptResponse, PromptError>` or `.stream()` → `StreamingResult`. Nothing happens until driven.
- **`max_turns` is the total model-call budget** including the initial call and every retry/continuation (1 = initial call only; 0 = no calls). Chained tool calls fail with `PromptError::MaxTurns { max_turns, chat_history, prompt }`.
- **`PromptResponse::output()` is a method returning `String`** (derived from the response's `content`); `messages` is a `Vec<Message>`, not an `Option`. `response.output` does not compile.
- **Message content is `Vec<T>`; assistant turns carry provenance.** `Message::User { content: Vec<UserContent> }`, `Message::Assistant(AssistantMessage { content: Vec<AssistantContent>, origin: Option<Origin>, stop: Option<StopReason> })`. `AssistantContent::Reasoning(Reasoning)` is plain data in 0.44 — `Sealed`/`Issuer` are gone; read `reasoning.text`.
- **`ToolCall`/`ToolResult` correlate through `CallId` and `ToolName`.** `ToolCall { id: CallId, function: ToolFunction { name: ToolName, arguments, .. }, native }`; `ToolResult { call: CallId, name: ToolName, content: Vec<ToolResultContent>, is_error }`. `ToolName` is a newtype (`EmptyToolName` for an empty one), `CallId` renders the provider id or a rig-issued UUID.
- **Tool-context values are typed and serde-backed.** `#[derive(…, rig_core::ContextValue)]` (or a manual `KEY`) is required for `ToolContext::insert`/`insert_result`/`result::<T>()`; the latter two return `Result<Option<T>, ToolContextError>`.
- **Tool results are hooked via `on_outcome`, not `on_tool_result`.** `OutcomeEvent` carries `kind`, `outcome: Result<Outcome, ErrorReport>`, `call_id: Option<&CallId>` and `context: Option<&ToolContext>`; rewrite with `OutcomeAction::rewrite_tool_result(..)`. The stack composes outcomes in hook registration order (later hooks see earlier rewrites).
- **`observes(kind)` gates the run lifecycle, the deltas and the dispatch/outcome events.** `HookStack::observes` is the *any* of its hooks, so a hook that returns `false` for `StepEventKind::ToolDispatch` sees no tool-result outcomes, and `false` for a delta means a sibling can still force the call.
- **Malformed tool arguments reach the invalid-call hook, with bounded retries.** `InvalidToolCallReason::MalformedArguments`, `InvalidToolCallContext`, and `InvalidToolCallAction::{Retry, Repair, Skip, Stop, Fail}` are the recovery surface; `max_consecutive_malformed_tool_calls(..)` bounds it and `UnhandledInvalidToolCall` governs unknown/disallowed names.
- **Streaming items are part events plus commit items.** Text/reasoning arrive as `StreamEvent::Text/Reasoning` fragments (and tool-call arguments as `StreamEvent::Arguments`, streamed as they arrive, lossy to assemble); `MultiTurnStreamItem::ToolCall` reports the model's committed calls and `ToolResult { tool_result }` reports their results (the `End` parts are for transcript/replay — count one of the two, not both).
- **A run's stream error is `PromptError`.** `StreamingResult = WasmBoxedStream<'static, Result<MultiTurnStreamItem, PromptError>>`; `StreamingError` and `StreamedUserContent` no longer exist.
- **`Usage` is `Option<u64>` per counter, plus `cost: Option<Cost>`.** Prefer `AddAssign` on `Usage` and `unwrap_or(0)` reads; `Usage::is_reported()` tells whether the provider sent any counter, and the catalog can price a usage.
- **Several enums are `#[non_exhaustive]`**: `CompletionRequest`, and the invalid-tool-call policy types (`InvalidToolCallReason`, `InvalidToolCallContext`, `InvalidToolCallAction`) — match them with a `_` arm and construct them through their constructors.
- **`schemars` v1.0 is required** for `JsonSchema` derives (tool args, structured output). Field descriptions are `///` doc comments; use `schemars::schema_for!(T)`.
- **OpenAI Responses API requires every parameter listed under `required`.** Include a `"required"` array in hand-written tool schemas, or use the macro's `required(...)` helper, or derive from `schemars::JsonSchema` (which marks non-`Option` fields required).
- **Query embeddings must match the stored model.** Vectors from different embedding models are not comparable; use the exact same model id at ingestion and query time. The embedding model is `client.embedding(id, Option<usize>)` → `Model<Embeddings>`.
- **OAuth providers (`chatgpt`, `copilot`) resolve sign-in at first use.** Build the config (empty credential) and an `Authenticator::new(AuthSource::OAuth, …, DeviceCodeHandler, allow_device_flow)`; `client.authenticate(&authenticator).await` resolves/refreshes/runs the device flow (cached credentials open without network). Without a device-code handler pass `allow_device_flow: false` for a fast, actionable failure. A pasted static token goes straight into the config.

## Quickstart

```rust
use rig_core::completion::CompletionRequest;
use rig_core::providers::openai::{self, OpenAI};
use rig_agent::AgentBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::new("sk-...");
    let answer = openai
        .completion(openai::GPT_5_2)          // dialect route (Responses for OpenAI)
        .call(CompletionRequest::new("Who are you?"))
        .await?;
    println!("{}", answer.text());           // folded, normalized reply

    let agent = AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a helpful assistant.")
        .reasoning(rig_core::completion::Effort::Low)   // typed generation option
        .build();
    let response = agent.prompt("Who are you?").await?; // .run() folded
    println!("{}", response.output());                  // a method, not a field
    Ok(())
}
```

`#[tokio::main]` needs Tokio's `macros` + `rt-multi-thread` features (or `full`). `Config::from_env()` reads the provider's key var (`OPENAI_API_KEY`, …; Ollama needs none) and its base-URL var where one exists (`OPENAI_BASE_URL`, …).

## The mental model

```
config (dialect-aware) ─→ client ─→ model (wire + transport) ─→ driver call/stream
  OpenAIConfig::with_key(&GROQ, k)   .client()   openai.completion(id)        Model<OpenAiWire>::call/stream/erase()
                                              openai.responses(id)
                                              anthropic.completion(id) → agent runs
```

The flow is always: **config → client → model/agent**. A `Model<W, T>` erases to **`DynModel<Op>`** for storage and agent construction; the agent runtime adds a bus (effects), tools, hooks, memory, and the loop. Companion: every provider operation (`Completion`, `Embedding`, `ModelListing`, `Verify`, rerank/transcription/images/audio) is an `Operation` with request/response types in `rig_core::operation`, and the catalog knows which models take which options.

### Layered agent API

| You want… | Use |
|-----------|-----|
| a whole agent loop folded to a final response | `agent.prompt(..).run().await` (or just `.await`) → `Result<PromptResponse, PromptError>` |
| an agent loop as an item stream | `agent.prompt(..).stream()` → `StreamingResult` of `MultiTurnStreamItem` |
| a stream plus a non-blocking event feed | `agent.prompt(..).run_channel()` → `(future, RunEvents)` |
| a conversation carrying caller-owned history | `agent.chat(prompt, &mut history)` (appends committed messages) or `.prompt(..).history(h)` (no write-back) |
| a typed struct instead of a string | `agent.prompt_typed::<T>(..).await?.output` or `ExtractorBuilder::<T>::new(model).build()` |
| per-run overrides (preamble, temperature, options, tools, …) | the `AgentRunner` setters (`.preamble`, `.temperature`, `.options`, `.cache`, …) |
| per-run model routing | `.using_model(label)` / `.using_model_value(model)` + `AgentBuilder::model_route` |
| a raw model call with no loop | `Model::call(CompletionRequest)` / `DynModel<Completion>::call(..)` |
| persisted/resumable runs | `AgentRun` sans-IO state machine + `agent.resume(run)` |

## Feature Decision Tree

Use this to decide which reference file to load:

**Need provider clients, configs/dialects, base URLs, auth (incl. OAuth device flow), capabilities, provider extensions, the model catalog, or the list of supported providers?**
→ Read `references/providers-and-clients.md`

**Need completion requests/responses, `CompletionRequest`, `GenerationOptions`/`ProviderOptions`, `Model`/`DynModel` calls, `Message`/`AssistantMessage`, `Usage`/`Cost`, `FinishReason`, or `ProviderError`?**
→ Read `references/completions.md`

**Need agents, `AgentBuilder`, `agent.prompt(...)`/`AgentRunner`, the agent loop, `max_turns`, `ToolChoice`, context, model routing, or manager-worker?**
→ Read `references/agents.md`

**Need `AgentRunner` per-run controls, `run()` vs `stream()` vs `run_channel()`, `PromptError`, or `AgentRun`/`RunSpec`/`RequestPatch` for durable approval flows?**
→ Read `references/agent-runner.md`

**Need `AgentHook`, per-event actions (`CompletionCallAction`/`ModelTurnAction`/`DispatchAction`/`OutcomeAction`/`InvalidToolCallAction`), malformed-tool-call recovery, guardrails, approvals, or `observes` gating?**
→ Read `references/hooks.md`

**Need tools, the `Tool`/`PortableTool` traits, `DynamicTool`/`new_with_context`, `ToolContext` + `ContextValue`, the `rig_tool` macro, `ToolEmbedding` (dynamic tools), tool servers, or MCP tools?**
→ Read `references/tools.md`

**Need streaming, `.stream()` runs, `MultiTurnStreamItem`, `StreamEvent`/`Item` parts, streamed tool-call arguments, `CallId` correlation, or `stream_to_stdout`?**
→ Read `references/streaming.md`

**Need structured output, `Extractor`/`ExtractorBuilder`, `TypedRun`/`prompt_typed`, `OutputMode`, or `StructuredOutputError`?**
→ Read `references/structured-output.md`

**Need embeddings, `Embed` (derive or manual), `EmbeddingsBuilder`, normalized embedding replies, or `ToolSchema`?**
→ Read `references/embeddings.md`

**Need vector stores, `VectorStoreIndex`, `VectorSearchResult`, `InsertDocuments`, `InMemoryVectorStore`, `dynamic_context`, tool RAG, or `VectorSearchRequest`?**
→ Read `references/rag-and-vector-stores.md`

**Need conversation memory, `InMemoryConversationMemory`, `ConversationMemory`/`MessageFilter`/`Compactor` traits, or memory via the agent builder?**
→ Read `references/memory.md`

## API Surface (Cheat Sheet)

| Item | Path | Purpose |
|------|------|---------|
| `OpenAI` / `OpenAIConfig` / `Dialect` | `rig_core::providers::openai::{OpenAI, OpenAIConfig, wire::*}` | OpenAI-shaped client + config + dialects (`OPENAI`, `OPENROUTER`, `AZURE`, `DEEPSEEK`, `GROQ`, `TOGETHER`, `VENICE`, `MISTRAL`, `MOONSHOT`, `MINIMAX`, `PERPLEXITY`, `HUGGINGFACE`, `HYPERBOLIC`, `MIRA`, `DOUBLEWORD`, `XIAOMIMIMO`, `ZAI`, `LLAMACPP`; xAI is the separate `xai::DIALECT`) |
| `Anthropic` / `AnthropicConfig` | `rig_core::providers::anthropic::{Anthropic, AnthropicConfig}` | Messages-format client (base for moonshot/minimax/xiaomi/zai anthropic dialects) |
| `Gemini` / `GeminiConfig` | `rig_core::providers::gemini::{Gemini, GeminiConfig}` | generateContent/interactions client |
| `Ollama` / `OllamaConfig` | `rig_core::providers::ollama::{Ollama, OllamaConfig}` | local daemon client (optional key); `/v1` chat and native `/api/chat` wires |
| `Copilot` / `CopilotConfig` / `auth` | `rig_core::providers::copilot::{Copilot, CopilotConfig, auth}` | Copilot client + GitHub/OAuth authenticator |
| `Cohere` / `CohereConfig` | `rig_core::providers::cohere::{Cohere, CohereConfig}` | Cohere chat (compatibility and native routes), embeddings |
| `VoyageAi` / `VoyageAiConfig` | `rig_core::providers::voyageai::{VoyageAi, VoyageAiConfig}` | embeddings + rerank only |
| `Authenticator` / `AuthSource` / `DeviceCodeHandler` | `providers::chatgpt::auth::…`, `providers::copilot::auth::…` | OAuth: `authenticate(&client).await` |
| `ChatGPT dialect` | `rig_core::providers::chatgpt` | `DIALECT` + model consts; OpenAI client + `authenticate` for OAuth |
| Provider extensions | `rig_core::providers::<p>::extension::{<P>Ext, <P>Options, <P>Extras}` | typed per-provider request/response options (`OpenAiOptions`, `AnthropicOptions`, `OllamaOptions`, …) |
| `Model` / `DynModel` | `rig_core::driver::{Model, DynModel}` | wire + transport; `call`, `stream`, `_observed` twins, `capabilities()`, `erase()` → `DynModel<Op>` |
| `Operation` family | `rig_core::operation::{Completion, Embedding, ModelListing, Rerank, Verify, …}` | the `Op` marker parameter; `wire::{Operation, Fold, Decoder, Wire}` back each endpoint |
| `CompletionRequest` | `rig_core::completion::CompletionRequest` | portable request (`chat_history`, `tools`, `options`, `provider_options`, `additional_params`, `output_schema`, …); `#[non_exhaustive]` |
| `GenerationOptions` / `ProviderOptions` | `rig_core::completion::{GenerationOptions, Reasoning, Effort, CacheRetention, ServiceTier, Verbosity, OnUnsupported, ProviderOptions, ExtensionOptions, ProviderExtension}` | typed request knobs and per-provider sections |
| `CompletionResponse` | `rig_core::completion::CompletionResponse` | folded reply: `choice`, `usage`, `origin`, `error`, `aborted`, `raw`; `text()`, `reasoning()`, `message()`, `head()`, `tool_calls()`, `finish_reason()`, `stop()`, `identity()`, `extras::<P>()` |
| `FinishReason` / `StopReason` | `rig_core::completion::{FinishReason, StopReason}` | `Stop`/`Length`/`ToolCalls`/`ContentFilter`/`Other(String)` + `reconcile_with_output`, `truncated_output` |
| `Message` / `AssistantMessage` | `rig_core::message::{Message, AssistantMessage}` | `System { content }` / `User { content: Vec<UserContent> }` / `Assistant(AssistantMessage { content, origin, stop })` |
| `AssistantContent` / `UserContent` | `rig_core::message::{AssistantContent, UserContent}` | `Text` / `ToolCall(ToolCall)` / `Reasoning(Reasoning)` / `Image` / `Opaque`; `Text` / `ToolResult(ToolResult)` / media |
| `CallId` / `ToolName` | `rig_core::message::{CallId, ToolName, EmptyToolName}` | identity newtypes for calls and tools |
| `Usage` / `Cost` | `rig_core::completion::{Usage, Cost}` | `Option<u64>` counters + `cost: Option<Cost>`; `is_reported()`, `Add`/`AddAssign` |
| `ProviderError` / `ErrorReport` / `ErrorKind` | `rig_core::error::{ProviderError, ErrorReport, ErrorKind}` | transport/JSON/request/response/provider errors, `UnsupportedOption`, `Relayed`, normalized reports |
| `Catalog` / `ModelSpec` / `Pricing` | `rig_core::catalog::{Catalog, ModelSpec, Pricing}` | per-model facts + prices; `builtin()`, `resolve("provider/model")`, `validate(&options)`, `Pricing::cost(&usage)` |
| `Agent` / `AgentBuilder` | `rig_agent::agent::{Agent, AgentBuilder}` | `AgentBuilder::new(model)` (generic over tool state only); preamble/context/tools/hooks/memory/routes/options; always non-model-generic |
| `AgentRunner` | `rig_agent::agent::AgentRunner<O = ()>` | per-run driver from `agent.prompt(..)`: `.history/.max_turns/.tool_concurrency/.tool_context/.add_hook/.using_model/.temperature/.max_tokens/.options/.reasoning/.cache/.provider_option/.additional_params/.run/.stream/.run_channel` |
| `PromptResponse` | `rig_agent::agent::PromptResponse` | `usage`, `messages: Vec<Message>`, `completion_calls`, `memory_append`, `content`; **`output() -> String`**, `usage()` |
| `TypedRun` / `TypedOutput` / `TypedPromptResponse` | `rig_agent::agent::{TypedRun, TypedOutput, TypedPromptResponse}` | `TypedRun<T> = AgentRunner<TypedOutput<T>>`; `TypedPromptResponse { output: T, … }` |
| `AgentRun` / `RunSpec` / `RequestPatch` / `OutputMode` | `rig_agent::run::{AgentRun, RunSpec, RequestPatch, OutputMode, …}` | sans-IO/drivable run vocabulary (durable + resumable); `RUN_FORMAT = 2` |
| `PromptError` | `rig_agent::completion::PromptError` (defined in `rig_agent::run::response`) | `Provider(ProviderError)` / `Report(ErrorReport)` / `Memory(MemoryError)` / `MaxTurns { .. }` / `Cancelled { .. }` / `UnknownToolCall { .. }` |
| `AgentHook` / events | `rig_agent::agent::{AgentHook, HookContext, RunStart, RunSettled, ModelSelection, CompletionCallEvent, ModelTurnFinished, TextDelta, ReasoningDelta, ToolCallDelta, DispatchEvent, OutcomeEvent, StepEventKind, Scratchpad, RunEntry, RunHandle}` | typed methods per boundary returning per-event actions |
| Actions | `…::{RunStartAction, ModelSelectionAction, CompletionCallAction, ModelTurnAction, DispatchAction, OutcomeAction, ObservationAction, InvalidToolCallAction}` | `Continue/Proceed` / `Patch/Rewrite/Select/Retry/Skip/Stop/Repair/Fail` per event |
| Invalid tool calls | `rig_agent::run::{InvalidToolCallReason, InvalidToolCallContext, InvalidToolCallAction, RetryRequest, UnhandledInvalidToolCall}` | malformed-argument recovery and unknown/disallowed policy (`#[non_exhaustive]` types) |
| `MultiTurnStreamItem` | `rig_agent::agent::MultiTurnStreamItem` | `StreamAssistantItem(Item<StreamEvent>)` / `ToolCall { tool_call }` / `ToolExecutionCommitted { tool_call }` / `ToolResult { tool_result }` / `CompletionCall(CompletionCall)` / `ModelTurnRetried { turn }` / `FinalResponse(PromptResponse)` |
| `StreamingResult` | `rig_agent::agent::StreamingResult` | `WasmBoxedStream<'static, Result<MultiTurnStreamItem, PromptError>>` |
| `RunEvents` | `rig_agent::agent::{RunEvents, RUN_EVENTS_CAPACITY}` | bounded event feed from `run_channel()`; `try_next`, `is_done`, `Stream`/`FusedStream` |
| `StreamEvent` / `Item` / `Part` | `rig_core::streaming::{StreamEvent, Item, Part, PartKind, Transcript, SequenceError, Streamed}` | `Start`/`Text`/`Reasoning`/`Arguments`/`End` parts; `Streamed<Op>`/`CompletionStream` with `finish()`/`partial()` |
| `stream_to_stdout` | `rig_agent::agent::{stream_to_stdout, StreamToStdoutError}` | `(&mut StreamingResult) -> Result<PromptResponse, StreamToStdoutError>` |
| `Tool` / `PortableTool` / `DynamicTool` | `rig_core::tool::{Tool, PortableTool, DynamicTool, ToolContext, …}`; `ToolSet` from `rig_agent::tool` | `const NAME`, `Args`, `Output`, `Error`, `description`, `parameters`, `call`; `DynamicTool::new(name, desc, params, args-only-callback)` / `new_with_context((ctx, args))` |
| `ToolOutput` / `ToolResult` / `ToolExecutionError` / `ToolErrorKind` | `rig_core::tool::{ToolOutput, IntoToolOutput, ToolResult, ToolExecutionError, ToolErrorKind}` | typed tool results and failures with output rendering |
| `ContextValue` | `rig_core::tool::ContextValue` (trait) + `#[derive(rig_core::ContextValue)]` | typed serde-backed keys for `ToolContext::insert/get/require/insert_result/result/require_result` |
| `rig_tool` | `#[rig_core::rig_tool]` / `#[rig_agent::rig_tool]` | function → `Tool` (mutable context arg) or `PortableTool`; `#[rig(context)]` names an imported context type |
| `ToolServer` / `ToolServerHandle` / `ToolCatalog` | `rig_agent::tool::{server, registry, catalog}` | shared mutable tool set via message passing; `agent.tool_server_handle()` |
| `Extractor` / `ExtractorBuilder` | `rig_agent::extractor::{Extractor, ExtractorBuilder}` | `ExtractorBuilder::<T>::new(model).retries(n).build()`, `extract(..).await?.output` |
| `Embed` (derive / trait) | `#[derive(rig_core::Embed)]` / `rig_core::embeddings::Embed` | `fn embed(&self, &mut TextEmbedder) -> Result<(), EmbedError>` |
| `EmbeddingsBuilder` | `rig_core::embeddings::EmbeddingsBuilder` | `.new(model: impl Into<DynModel<op::Embedding>>)` / `.document(s)?` / `.build().await?` |
| `Embedding` / `EmbeddingResponse` | `rig_core::embeddings::{Embedding, EmbeddingResponse}` | normalized embedding reply + `vec` |
| `VectorStoreIndex` / `VectorSearchResult` | `rig_core::vector_store::{VectorStoreIndex, VectorSearchResult, VectorSearchIdResult, InsertDocuments}` | `top_n(req)` → `Vec<VectorSearchResult<T>> { score, id, document }`; `top_n_ids` → ids only |
| `InMemoryVectorStore` | `rig_core::vector_store::in_memory_store::InMemoryVectorStore` | `from_documents`/`from_documents_with_ids`/`…_with_id_f`, `.index(model)` |
| `VectorSearchRequest` | `rig_core::vector_store::request::VectorSearchRequest` | typestate builder: `.builder().query(..).samples(n).build()` |
| `ConversationMemory` / `InMemoryConversationMemory` | `rig_core::memory::{ConversationMemory, InMemoryConversationMemory, MessageFilter, DemotionHook, Compactor}` | in-process memory contracts; `.with_filter(..)`; wired via `AgentBuilder::memory(..)` |
| `test_utils` | `rig_core::test_utils` / `rig_agent::test_utils` (feature `test-utils`) | mock completion models/embeddings/memory/relay plus streaming and trace conformance helpers |
| `schemars` / `serde` / `serde_json` | `rig_core::{schemars, serde, serde_json}` | re-exports so macro-generated code resolves through rig; **schemars v1.0** |

## Provider List (rig-core 0.44)

Modules under `rig_core::providers::*`, each with its own client/section unless marked: `openai` (`OpenAI`), `anthropic` (`Anthropic`), `gemini` (`Gemini`), `ollama` (`Ollama`), `copilot` (`Copilot`), `cohere` (`Cohere`), `voyageai` (`VoyageAi`, embeddings/rerank only), `chatgpt` (dialect + auth — the client is `openai::OpenAI`), `azure` (OpenAI-shaped resource routing — client is `openai::OpenAI`), and OpenAI/Anthropic-shaped **vendors** whose constructors build the shared client: `deepseek`, `doubleword`, `groq`, `huggingface`, `hyperbolic`, `llamacpp`, `minimax`, `mira`, `mistral`, `moonshot`, `openrouter`, `perplexity`, `together`, `venice`, `xai` (`xai::DIALECT`), `xiaomimimo`, `zai`. `providers::registry::ProviderRef` builds clients from data when the provider is chosen programmatically.

OpenAI-compatible vendors are reachable generically via `OpenAIConfig::with_key(&openai::wire::<DIALECT>, key).with_base_url(url).client()` when you need an endpoint override.

## Conventions for This Repo

- **Import shape**: `use rig_core::providers::{openai, anthropic, gemini, ollama, …};` / `use rig_agent::agent::{AgentBuilder, …};` inside `shuvarie-llm` only. The root binary never imports `rig`.
- **Provider abstraction**: `shuvarie-llm` collapses rig's per-provider client types into its own `ListImpl` enum (7 client families + the OAuth-pending gate) keyed by `shuvarie_llm::ProviderKind` — the Selune protocol kind plus an optional vendor dialect (Selune 0.4 folds the OpenAI-compatible vendors under `openai-compat` + `dialect`); the TUI never names a provider concretely. Follow that boundary for any new provider.
- **Typed errors**: `shuvarie-llm` uses `thiserror`; map rig's `ProviderError`/`PromptError`/`StructuredOutputError` into `LlmError::Provider`/`LlmError::Model` rather than bubbling them.
- **No comments** unless explicitly requested (project-wide convention; `cargo fmt` defaults; empty `rustfmt.toml`).
- **Lint gate**: `cargo fmt --check` and `cargo clippy --all-targets` must pass before a change is considered done.

## Common Pitfalls

1. **Calling `rig` from the root binary** — go through `shuvarie-llm` so the TUI stays provider-agnostic and the core task owns the async work.
2. **Awaiting a stream on the TUI thread** — drive `agent.prompt(..).stream()` on the core task and forward mapped items over `mpsc` to the TUI.
3. **Reading `response.output` as a field** — in 0.44 it is `output()`, a method returning `String`; `messages` is a `Vec<Message>`, not an `Option`.
4. **Forgetting `max_turns` on multi-tool prompts** — it is the *total* model-call budget (1 = initial only); chained tool calls fail with `PromptError::MaxTurns`.
5. **Double-appending history with `chat`** — `chat(prompt, &mut history)` already appends the committed turn (including tool calls/results). `.history(..)` on a runner does NOT (it only seeds the input — the committed messages come back on `PromptResponse.messages`).
6. **Mixing embedding models** — query embeddings must come from the exact same model id used to embed the stored documents.
7. **Hand-writing tool `required` arrays wrong for OpenAI** — the Responses API requires every input parameter under `required`; use `schemars::JsonSchema` (non-`Option` fields are required) or the macro's `required(...)`.
8. **Looking for `client.agent(..)` / `CompletionModel` / `EmbeddingsClient` / `StreamingError` / `StreamedUserContent`** — all gone. Build `AgentBuilder::new(model.erase())`, take stream errors as `PromptError`, and read tool results from `MultiTurnStreamItem::ToolResult { tool_result }`.
9. **Using `OneOrMany` or `Message::Assistant { id, content }`** — gone. Message content is `Vec<T>`, and assistant turns are `Message::Assistant(AssistantMessage { content, origin, stop })` (`AssistantMessage::new(..)` when hand-building).
10. **Reaching for `Sealed<Reasoning>` / `Issuer`** — gone in 0.44; `AssistantContent::Reasoning(Reasoning)` is plain data (`text`, `redacted`, `native`), and streaming reasoning is plain fragments.
11. **Storing bare values in `ToolContext`** — `insert_result(5u64)` does not compile; values are `ContextValue`s (typed + serde + a stable `KEY`), and both `insert_result`/`result::<T>()` return `Result`.
12. **Matching `on_tool_result` / `ToolResultAction` / `internal_call_id`** — gone. Use `on_outcome` + `OutcomeEvent.call_id`/`tool_result()` + `OutcomeAction::rewrite_tool_result`, and correlate stream items via `CallId` (`tool_call.id` ↔ `tool_result.call`).
13. **Counting streamed tool calls twice** — `MultiTurnStreamItem::ToolCall` reports the committed model calls while `StreamEvent::End` parts are for transcript/replay; count one. Likewise, tool-call arguments arriving as `StreamEvent::Arguments` are partial and lossy to assemble. A `ToolExecutionCommitted` is not a start notification; the whole batch surfaces together.
14. **Assuming `from_env()` works without the API key env var** — each provider reads a specific var (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `COHERE_API_KEY`, …); Ollama needs none.
15. **Using `schemars` 0.8 idioms** — v1.0 is required; descriptions are `///` doc comments and the macro is `schemars::schema_for!`.
16. **Building `rig-reqwest`-backed clients without the `reqwest` feature** — `Config::…client()` is behind `rig-core/reqwest`; without it you must `.connect(http)` with an HTTP transport of your own.
17. **Assuming `observes()` only gates outcomes** — in 0.44 a hook that returns `false` for `StepEventKind::ToolDispatch` stops seeing tool results, and the same gate covers the run lifecycle and delta events.
18. **Setting a typed option the model does not take** — `OnUnsupported::Error` is the default, so the call fails; validate with `ModelSpec::validate` (or the catalog) or opt into `OnUnsupported::Ignore`.
19. **Expecting `additional_params` to be replaced wholesale** — it now deep-merges into the body and raw keys beat typed options (precedence: mapped options → `provider_options` → `additional_params`).
20. **Reaching for `#[non_exhaustive]` types with an exhaustive `match`** — `CompletionRequest` and the invalid-tool-call policy types require a `_` arm.
21. **Ignoring malformed tool arguments** — they reach `on_invalid_tool_call` (`InvalidToolCallReason::MalformedArguments`) and are subject to `max_consecutive_malformed_tool_calls`; the default path answers the call with corrective feedback rather than failing the run.

## External Resources

- **Rig website / guide**: https://rig.rs/docs
- **Rig API docs (0.44)**: https://docs.rs/rig-core/0.44.0/rig_core/ · https://docs.rs/rig-agent/0.44.0/rig_agent/
- **GitHub**: https://github.com/0xPlaygrounds/rig
- **Examples**: https://github.com/0xPlaygrounds/rig/tree/main/examples
- **awesome-rig**: https://github.com/0xPlaygrounds/awesome-rig
- **Discord**: https://discord.gg/playgrounds

## Complete File Index

| File | Description |
|------|-------------|
| `SKILL.md` | Main entry point — quickstart, mental model, decision tree, cheat sheet, repo conventions |
| `references/providers-and-clients.md` | Config + wire + transport architecture, per-provider clients & dialects, model-building methods, base URLs, provider extensions, the model catalog, capabilities, `registry::ProviderRef`, the provider table |
| `references/completions.md` | `CompletionRequest`, `GenerationOptions` + `ProviderOptions`, normalized `CompletionResponse`, `FinishReason`/`StopReason`, `Usage`/`Cost`, `Message`/`AssistantMessage`/media, `ToolCall`/`ToolResult`, history shaping, `ProviderError`/`ErrorReport` |
| `references/agents.md` | `Agent`/`AgentBuilder` (models, preamble, context, tools, routes, memory, options), `agent.prompt(..)` → `AgentRunner`, the loop, `max_turns`, `ToolChoice`, manager-worker, `chat()` history write-back |
| `references/agent-runner.md` | `AgentRunner<O>` per-run controls, `run()`/`stream()`/`run_channel()`, `PromptError`, `AgentRun`/`RunSpec`/`RequestPatch`/`OutputMode` sans-IO state machine |
| `references/hooks.md` | `AgentHook` typed methods + per-event action enums, `RequestPatch`, invalid-tool-call recovery, `observes` gating, `HookStack` composition, non-exhaustive policy types |
| `references/tools.md` | `Tool`/`PortableTool`, `DynamicTool` (`new` vs `new_with_context`), `ToolContext` + `ContextValue`, typed `ToolResult`/`ToolExecutionError`, `rig_tool`, `ToolEmbedding` + `ToolSet`, tool servers, tool RAG, MCP |
| `references/streaming.md` | `.stream()` runs, `MultiTurnStreamItem`, core `Item`/`Part`/`StreamEvent` parts, streamed tool-call arguments, `CallId` correlation, `RunEvents`, `stream_to_stdout` |
| `references/structured-output.md` | shared `structured_output` policy, `OutputMode`, `Extractor`/`ExtractorBuilder`, `TypedRun`/`TypedPromptResponse`, schema/reprompt loop, `StructuredOutputError` |
| `references/embeddings.md` | `Embed` derive + manual impl, `TextEmbedder`, `EmbeddingsBuilder`, normalized embedding replies and width checks, tool-RAG `ToolSchema` |
| `references/rag-and-vector-stores.md` | RAG phases, `VectorStoreIndex`/`VectorSearchResult`, `InsertDocuments`, `InMemoryVectorStore`/LSH, `dynamic_context(n, index)`, `VectorSearchRequest`/filters, tool RAG, stores |
| `references/memory.md` | `ConversationMemory`/`MessageFilter`/`DemotionHook`/`Compactor` contracts, `InMemoryConversationMemory`, memory via `AgentBuilder::memory`, `PromptResponse::memory_append`, long-term memory |
