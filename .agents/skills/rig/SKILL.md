---
name: rig
description: >-
  You are an expert in Rig, the Rust library for building scalable, modular, and
  ergonomic LLM-powered applications. You help developers wire up provider
  clients (OpenAI, Anthropic, Gemini, Ollama, and 20+ others) built as configs
  on wires and transports, build agents with tools and RAG context, drive runs
  with AgentRunner streams, extract typed structured output, manage
  conversation memory, and hook the agent loop for guardrails and approvals.
  This skill covers Rig 0.43.x (`rig-core` and `rig-agent` depended on directly;
  the `rig` facade is not published for 0.43) as used by the `shuvarie-llm`
  crate. Docs: guide at https://rig.rs/docs and API docs at
  https://docs.rs/rig/latest/rig/.
license: MIT
metadata:
  author: shuvarie
  version: 0.43.0
  category: Backend Development
  tags:
    - rust
    - llm
---

# Rig — LLM-Powered Applications in Rust

Rig is a Rust library for building LLM-powered applications and agents. It gives you unified abstractions over **model providers** (OpenAI, Anthropic, Gemini, Ollama, and 20+ others), **tools**, **memory**, and **vector stores / RAG**, so you can wire up an agent in a few lines and scale to a production system without changing frameworks.

This project (`shuvarie`) pins **Rig 0.43.0** — `rig-core` and `rig-agent` directly in `crates/llm/Cargo.toml` (`rig-core` with `features = ["reqwest"]`; the `rig` facade crate is not published for 0.43 and shuvarie deliberately avoids it). The `shuvarie-llm` crate is a thin wrapper over `rig` that exposes a `ProviderType`-keyed `ProviderClient` (27 transports), model listing, embeddings, worker agents, and a streaming completion API; the root binary never calls `rig` directly (see `AGENTS.md`).

> **Rig 0.43 replaced the "client trait" architecture with configs + wires + transports, and rebuilt the agent/streaming surface.** The biggest breaking changes vs. 0.42:
> 1. **Provider clients are named per provider** (`openai::OpenAI`, `anthropic::Anthropic`, `gemini::Gemini`, `ollama::Ollama`, `copilot::Copilot`, `cohere::Cohere`, `voyageai::VoyageAi`) and built from **configs**: `OpenAIConfig::new(key).client()` / `Config::from_env()?.client()` / `.connect(http)`. OpenAI-shaped **vendors** (Groq, Together, DeepSeek, …) are *dialects*: their modules expose `from_env()`/`new(key)` constructors that return the shared `OpenAI` client on their dialect (e.g. `openai::wire::GROQ`), so an OpenAI- or Anthropic-shaped client covers all of them.
> 2. **A model is a `Model<Wire, Transport>`** (`rig_core::driver::Model`): a wire (what to send: `Chat`, `Responses`, `Messages`, `GenerateContent`, `Embeddings`, …) on a transport (how it travels; the shared reqwest client by default). Clients build models: `openai.chat("gpt-5")`, `openai.completion("gpt-5")` (the dialect's default completion route — Responses for OpenAI/ChatGPT, chat completions for compat vendors), `anthropic.completion("claude-…")`. A model erases to **`DynModel<Op>`** (`model.erase()`; agent builder takes `impl Into<DynModel<Completion>>`).
> 3. **`CompletionClient`/`EmbeddingsClient`/`AgentClientExt`/`CompletionModel` are gone** — there is no `client.agent(model)` helper; build agents from models: `AgentBuilder::new(model)…build()`.
> 4. **Agent runs are `AgentRunner`s built by `agent.prompt(msg)`** — configure (`.history(..)`, `.max_turns(..)`, `.tool_concurrency(..)`, `.add_hook(h)`, …) then drive: `.await` (blocking → `PromptResponse`), `.stream()` (→ `StreamingResult` of `MultiTurnStreamItem`, terminal item `FinalResponse(PromptResponse)`), or `.run_channel()`. The `Prompt`/`Chat`/`StreamingPrompt`/`StreamingChat` traits are gone (`agent.chat(prompt, &mut history)` remains as a convenience method).
> 5. **Hooks changed**: `on_tool_result` is gone — tool results are observed/steered on the **`on_outcome`** bus boundary (`OutcomeEvent`, `OutcomeAction::rewrite_tool_result/…`); completion calls use **`CompletionCallEvent`** ({prompt, history, turn}) and `CompletionCallAction`. `observes(kind)` now also gates `on_outcome` (return `true` for `StepEventKind::ToolDispatch` to run).
> 6. **Streams are part-based**: `MultiTurnStreamItem::StreamAssistantItem(rig_core::streaming::Item<StreamEvent>)` with `StreamEvent::{Start, Text, Reasoning, Arguments, End}`; model-emitted tool calls surface as `MultiTurnStreamItem::ToolCall { tool_call }` when the turn commits, results as `StreamUserItem(StreamedUserContent::ToolResult { tool_result })` — correlate everything through `CallId` (`tool_call.id` ↔ `tool_result.call`; no more `internal_call_id`).
> 7. **`ToolContext::insert_result`/`result::<T>()` are serde-backed and typed**: values must implement **`ContextValue`** (derive `rig_core::ContextValue`, optionally `#[context(key = "…")]`) — bare `String`/integers cannot be stored. `insert_result` returns `Result<Option<T>, ToolContextError>`.
> 8. **`Usage` counters are `Option<u64>`** (unreported = `None`), with `Add`/`AddAssign` that preserve reportedness. `rig_tool` is the only tool macro name (`tool_macro` renamed); `PortableDynamicTool` is gone (`DynamicTool` covers it, `DynamicTool::new` is now context-free, use `new_with_context` for tools that need the `ToolContext`).

Official docs: **Guide** at https://rig.rs/docs and **API docs** at https://docs.rs/rig/latest/rig/. (docs.rs `rig` still shows 0.42-era docs; for 0.43 read the sources under `~/.cargo/registry/src/*/rig-core-0.43.0` / `rig-agent-0.43.0` or the agent crate's doc comments — they are authoritative.)

## Workspace / crate layout

Rig separates portable provider/backend contracts from the agent runtime:

- `rig-core` (**`rig_core`**) — portable contracts: `wire` (configs, dialects, endpoint wires), `driver` (`Model`/`DynModel` + transports), `completion` (requests/responses/messages), `streaming` (part events), `tool` (Tool contracts, contexts, results), `embeddings`, `vector_store`, `memory`, `model` (listing), `error` (`ProviderError`, `ErrorReport`, `ErrorKind`), `effect` (typed effect bus contracts), `serve`, and provider modules. Default features: `derive` + `rustls`; enable `reqwest` for the shared `rig_reqwest::shared()` transport behind `Config::...client()`.
- `rig-agent` (**`rig_agent`**) — the classic agent runtime: `Agent`/`AgentBuilder`, `AgentRunner` (blocking/streaming/run-channel driving), `AgentRun` (sans-IO durable state machine), typed hooks, tools (ToolSet/registry/catalog + `tool::server`), typed extraction, re-runs, and `rig_agent::core` (re-exports `rig_core::*` under that path). Default feature: `derive`; `test-utils` enables `rig_core::test_utils` (mock models) re-exported at `rig_agent::test_utils`.
- Companion crates are split out per backend (`rig-lancedb`, …) / transports (`rig-http`, `rig-reqwest`, `rig-tungstenite` — published for 0.43); vector-store/memory/storage integrations (lancedb/qdrant/mongo/memory policies, …) were **not republished for 0.43 at the time of writing** — their contracts live in `rig-core`, so you can implement the traits yourself when needed.

```toml
[dependencies]
rig-core = { version = "0.43", features = ["reqwest"] }
rig-agent = "0.43"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "sync"] }
```

Do **not** depend on the `rig` facade for 0.43 (not yet published); depend on `rig-core`/`rig-agent` directly. In this repo the dependency lives in `shuvarie-llm` only.

## Critical Rules

Before writing Rig code in this repo, know these constraints:

- **The root binary never calls `rig` directly.** All provider access goes through `shuvarie-llm` (see `AGENTS.md`). Put LLM/SDK glue in `crates/llm/`, not in the TUI.
- **Library crates use typed errors, not `eyre`.** `shuvarie-llm` defines `LlmError` (thiserror). Don't bubble `anyhow`/`eyre` out of `shuvarie-core` or `shuvarie-llm`; `color-eyre` is binary-only.
- **`shuvarie-llm` must stay side-effect-free w.r.t. the TUI.** Long-running LLM work runs on the spawned core task over `tokio::sync::mpsc`; never `await` inside the TUI loop. The LLM layer exposes async APIs the core task drives.
- **Streaming tokens belong on the core task.** Drive `agent.prompt(..).stream()` on the core task and forward `MultiTurnStreamItem`s (mapped to `StreamItem`s) to the TUI via the existing channel; do not pull a stream from `handle_event`/`view`.
- **Clients are configs on transports.** Build with `rig_core::providers::<mod>::<X>Config::new(key)` / `with_key(&DIALECT, key)` (+ `.with_base_url(..)`) then `.client()` (shared reqwest) or `.connect(http)`. `.build()`-style fallible builders are gone.
- **OpenAI-shaped vendors are dialects of one client.** Reach them by dialect when you need a base-URL override: `OpenAIConfig::with_key(&openai::wire::GROQ, key)…` (see `references/providers-and-clients.md` for the full table).
- **Agents are built from models, not clients.** `agent.completion(id).erase()` → `AgentBuilder::new(model)…build()`; there is no `AgentClientExt`/`client.agent(..)`.
- **`agent.prompt(msg)` returns an `AgentRunner` = the run.** Set `.history(..)` (bypasses memory), `.max_turns(..)`, `.tool_concurrency(..)`, `.add_hook(h)`, then `.run()`/`.await` → `PromptResponse` (`.output` = accepted text, `.usage`, `.messages` committed) or `.stream()` → item stream. Nothing happens until driven.
- **`max_turns` is the total model-call budget** including the initial call and every retry/continuation (1 = initial call only; 0 = no calls). Chained tool calls fail with `PromptError::MaxTurnsError` (carries history).
- **Message content is `Vec<T>`; reasoning is `Sealed<Reasoning>`.** `Message::User { content: Vec<UserContent> }`, `Message::Assistant { id, content: Vec<AssistantContent> }`. `AssistantContent::Reasoning` wraps `Reasoning` in `Sealed<Reasoning>` — the value only its issuing provider's `Issuer` can `open(..)`; for display-size heuristics, round-trip through serde (the flattened content) instead.
- **`ToolCall`/`ToolResult` correlate through `CallId`.** `ToolCall { id: CallId, function: ToolFunction { name: ToolName, arguments }, .. }`; `ToolResult { call: CallId, name: ToolName, content: Vec<ToolResultContent> }` (no separate `provider` field). `CallId::wire()`/`Display` renders the provider id or the rig-issued UUID; `ToolName` implements `Display`/`as_str`.
- **Tool-context values are typed and serde-backed.** `#[derive(…, rig_core::ContextValue)]` (or `impl ContextValue for T { const KEY: &'static str = "…"; }`) is required for `ToolContext::insert`/`insert_result`/`result::<T>()` (the latter two return `Result<Option<T>, ToolContextError>`).
- **Tool results are hooked via `on_outcome`, not `on_tool_result`.** `OutcomeEvent` carries `kind`, `outcome: Result<Outcome, ErrorReport>` (`tool_result()`), `call_id: Option<&CallId>` and `context: Option<&ToolContext>`; rewrite with `OutcomeAction::rewrite_tool_result(&event, text)`. The stack composes outcomes in hook registration order (later hooks see earlier rewrites).
- **`observes(kind)` gates dispatch/outcome events in 0.43.** Return `true` for `StepEventKind::ToolDispatch` (and whatever else you steer) — the default `false`-for-everything pattern of 0.42 would silently disable tool-result hooks.
- **Streaming items are part events.** Text/reasoning arrive as `StreamEvent::Text/Reasoning` fragments; finalized content only on `End`. Tool calls are the `MultiTurnStreamItem::ToolCall` commit items (not the `End` parts, or you double-count).
- **`Usage` is `Option<u64>` per counter.** Prefer `AddAssign` on `Usage` and `unwrap_or(0)` reads; `Usage::is_reported()` tells whether the provider sent any counter.
- **`schemars` v1.0 is required** for `JsonSchema` derives (tool args, structured output). Field descriptions move from `#[schemars(description = "...")]` to `///` doc comments; use `schemars::schema_for!(T)`.
- **OpenAI Responses API requires every parameter listed under `required`.** Include a `"required"` array in hand-written tool schemas, or use the macro's `required(...)` helper, or derive from `schemars::JsonSchema` (which marks non-`Option` fields required).
- **Query embeddings must match the stored model.** Vectors from different embedding models are not comparable; use the exact same model id at ingestion and query time. The embedding model is `client.embedding(id, Some(ndims) | None)` → `Model<Embeddings>` — no more `EmbeddingModel` trait.
- **OAuth providers (`chatgpt`, `copilot`) resolve sign-in at first use.** Build the config (empty credential) and an `Authenticator::new(AuthSource::OAuth, …, DeviceCodeHandler, allow_device_flow)`; `client.authenticate(&authenticator).await` resolves/refreshes/runs the device flow (cached credentials open without network). Without a device-code handler pass `allow_device_flow: false` for a fast, actionable failure. A pasted static token goes straight into the config.

## Quickstart

```rust
use rig_core::completion::CompletionRequest;
use rig_core::providers::openai::{self, OpenAI};
use rig_agent::AgentBuilder;
use rig_agent::tool::Tool;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::new("sk-...");
    let answer = openai
        .completion(openai::GPT_5_2)          // dialect route (Responses for OpenAI)
        .call(CompletionRequest::new("Who are you?"))
        .await?;
    println!("{}", answer.choice.iter().map(|c| match c {
        rig_core::message::AssistantContent::Text(t) => t.text.clone(),
        _ => String::new(),
    }).collect::<String>());

    let agent = AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You are a helpful assistant.")
        .build();
    let response = agent.prompt("Who are you?").await?; // .run() folded
    println!("{}", response.output);
    Ok(())
}
```

`#[tokio::main]` needs Tokio's `macros` + `rt-multi-thread` features (or `full`). `Config::from_env()` reads the provider's key var (`OPENAI_API_KEY`, …; Ollama needs none).

## The mental model

```
config (dialect-aware) ─→ client ─→ model (wire + transport) ─→ driver call/stream
  OpenAIConfig::with_key(&GROQ, k)   .client()   openai.completion(id)        Model<Chat>::call/stream/erase()
                                              openai.responses(id)
                                              anthropic.completion(id) → agent runs
```

The flow is always: **config → client → model/agent**. A `Model<W, T>` erases to `DynModel<Op>` for storage and agent construction; the agent runtime adds a bus (effects), tools, hooks, memory, and the loop. Companion: every provider operation (`Completion`, `Embedding`, `ModelListing`, `Verify`, rerank/transcription/images/audio) is an `Operation` with request/response types in `rig_core::operation`.

### Layered agent API

| You want… | Use |
|-----------|-----|
| a whole agent loop folded to a final response | `agent.prompt(..).run().await` (or just `.await`) → `PromptResponse` |
| an agent loop as an item stream | `agent.prompt(..).stream()` → `StreamingResult` of `MultiTurnStreamItem` |
| a conversation carrying caller-owned history | `agent.chat(prompt, &mut history)` (appends committed messages) or `.prompt(..).history(h)` (no write-back) |
| a typed struct instead of a string | `agent.prompt_typed::<T>(..).await` or `ExtractorBuilder::<T>::new(model).build()` |
| per-run overrides (preamble, temperature, tools, …) | the `AgentRunner` setters (`.preamble`, `.temperature`, `.max_tokens`, …) |
| per-run model routing | `.using_model(label)` / `.using_model_value(model)` + `AgentBuilder::model_route` |
| a raw model call with no loop | `Model::call(CompletionRequest)` / `DynModel<Completion>::call(..)` |
| persisted/resumable runs | `AgentRun` sans-IO state machine + `agent.resume(run)` |

## Feature Decision Tree

Use this to decide which reference file to load:

**Need provider clients, configs/dialects, auth (incl. OAuth device flow), capabilities, custom base URLs, or the list of supported providers?**
→ Read `references/providers-and-clients.md`

**Need completion requests/responses, `CompletionRequest`, `Model`/`DynModel` calls, `Message`, `Usage`, or `ProviderError`?**
→ Read `references/completions.md`

**Need agents, `AgentBuilder`, `agent.prompt(...)`/`AgentRunner`, the agent loop, `max_turns`, `ToolChoice`, context, manager-worker, or model routing?**
→ Read `references/agents.md`

**Need `AgentRunner` per-run controls (`max_turns`, `tool_concurrency`, `tool_context`, `history`, `using_model`, `add_hook`), `run()` vs `stream()` vs `run_channel()`, or `AgentRun` (sans-IO state machine for durable approval flows)?**
→ Read `references/agent-runner.md`

**Need `AgentHook`, per-event actions (`CompletionCallAction`/`ModelTurnAction`/`DispatchAction`/`OutcomeAction`/`InvalidToolCallAction`), `RequestPatch`, guardrails, approvals, or invalid-tool-call recovery?**
→ Read `references/hooks.md`

**Need tools, `Tool` trait, `DynamicTool`/`new_with_context`, `ToolContext` + `ContextValue`, the `rig_tool` macro, `ToolEmbedding` (dynamic tools), tool servers, or MCP tools?**
→ Read `references/tools.md`

**Need streaming, `.stream()` runs, `MultiTurnStreamItem`, core `StreamEvent`/`Item` parts, `StreamedUserContent`, `CallId` correlation, or `stream_to_stdout`?**
→ Read `references/streaming.md`

**Need structured output, `Extractor`/`ExtractorBuilder`, `TypedRun`/`prompt_typed`, or `StructuredOutputError`?**
→ Read `references/structured-output.md`

**Need embeddings, `Embed` trait (derive or manual), `EmbeddingsBuilder`, or `Embedding`/`TextEmbedder`?**
→ Read `references/embeddings.md`

**Need vector stores, `VectorStoreIndex`, `InsertDocuments`, `InMemoryVectorStore`, `dynamic_context`, tool RAG, or `VectorSearchRequest`?**
→ Read `references/rag-and-vector-stores.md`

**Need conversation memory, `InMemoryConversationMemory`, `ConversationMemory`/`MessageFilter`/`Compactor` traits, or memory via the agent builder?**
→ Read `references/memory.md`

## API Surface (Cheat Sheet)

| Item | Path | Purpose |
|------|------|---------|
| `OpenAI` / `OpenAIConfig` / `Dialect` | `rig_core::providers::openai::{OpenAI, OpenAIConfig, wire::*}` | OpenAI-shaped client + config + dialects (`OPENAI`, `OPENROUTER`, `AZURE`, `DEEPSEEK`, `GROQ`, `TOGETHER`, `VENICE`, `XAI`(via `xai::DIALECT`), `MISTRAL`, `MOONSHOT`, `MINIMAX`, `PERPLEXITY`, `HUGGINGFACE`, `HYPERBOLIC`, `MIRA`, `DOUBLEWORD`, `XIAOMIMIMO`, `ZAI`, `LLAMACPP`) |
| `Anthropic` / `AnthropicConfig` | `rig_core::providers::anthropic::{Anthropic, AnthropicConfig}` | Messages-format client (base for moonshot/minimax/xiaomi/zai anthropic dialects) |
| `Gemini` / `GeminiConfig` | `rig_core::providers::gemini::{Gemini, GeminiConfig}` | generateContent/interactions client |
| `Ollama` / `OllamaConfig` | `rig_core::providers::ollama::{Ollama, OllamaConfig}` | local daemon client (optional api key) |
| `Copilot` / `CopilotConfig` / `auth` | `rig_core::providers::copilot::{Copilot, CopilotConfig, auth}` | Copilot client + GitHub/OAuth authenticator |
| `Cohere` / `CohereConfig` | `rig_core::providers::cohere::{Cohere, CohereConfig}` | Cohere chat/embeddings |
| `VoyageAi` / `VoyageAiConfig` | `rig_core::providers::voyageai::{VoyageAi, VoyageAiConfig}` | embeddings + rerank only |
| `Authenticator` / `AuthSource` / `DeviceCodeHandler` | `providers::chatgpt::auth::…`, `providers::copilot::auth::…` | OAuth: `authenticate(&client).await` |
| `ChatGPT dialect` | `rig_core::providers::chatgpt` | `DIALECT` + model consts; OpenAI client + `authenticate` for OAuth |
| `Model` / `DynModel` | `rig_core::driver::{Model, DynModel}` | wire + transport; `model.erase()` → `DynModel<Op>` |
| `Operation` family | `rig_core::operation::{Completion, Embedding, ModelListing, …}` | the `Op` marker parameter |
| `CompletionRequest` / `CompletionResponse` | `rig_core::completion::{CompletionRequest, CompletionResponse}` | portable request (`chat_history`, `tools`, …) and folded response (`choice: Vec<AssistantContent>`, `usage`) |
| `Message` | `rig_core::message::Message` | `System { content }` / `User { content: Vec<UserContent> }` / `Assistant { id, content: Vec<AssistantContent> }`; `Message::user/assistant/system/tool_result` |
| `AssistantContent` | `rig_core::message::AssistantContent` | `Text` / `ToolCall(ToolCall)` / `Reasoning(Sealed<Reasoning>)` / `Image` |
| `UserContent` | `rig_core::message::UserContent` | `Text` / `ToolResult(ToolResult)` / media |
| `CallId` / `ToolName` / `Issuer` / `Sealed` | `rig_core::message::{CallId, ToolName, Issuer, Sealed}` | identities: `CallId::from_wire`, `.wire()`, `ToolName::as_str`; `Sealed::open(&Issuer)` |
| `Usage` | `rig_core::completion::Usage` | `Option<u64>` counters (input/output/total/cached_input/cache_creation/tool_use_prompt/reasoning); `Add`/`AddAssign`; `is_reported()` |
| `ProviderError` / `ErrorReport` / `ErrorKind` | `rig_core::error::{ProviderError, ErrorReport, ErrorKind}` | transport/JSON/request/response/provider errors + normalized reports |
| `Agent` / `AgentBuilder` | `rig_agent::agent::{Agent, AgentBuilder}` | `AgentBuilder::new(model)`; preamble/context/tools/hooks/memory/routes; always non-generic |
| `AgentRunner` | `rig_agent::agent::AgentRunner` | per-run driver from `agent.prompt(..)`: `.history/.max_turns/.tool_concurrency/.tool_context/.add_hook/.using_model/.temperature/.max_tokens/.additional_params/.run/.stream/.run_channel` |
| `PromptResponse` | `rig_agent::agent::PromptResponse` | `output: String`, `usage`, `messages`, `completion_calls`, `memory_append` |
| `AgentRun` / `Resume` | `rig_agent::agent::AgentRun` | sans-IO state machine (durable/resumable) |
| `AgentHook` / events | `rig_agent::agent::{AgentHook, CompletionCallEvent, ModelTurnFinished, RunStart, RunSettled, DispatchEvent, OutcomeEvent, HookContext, StepEventKind}` | typed methods per boundary returning per-event actions |
| Actions | `…::{CompletionCallAction, ModelTurnAction, ModelSelectionAction, RunStartAction, DispatchAction, OutcomeAction, ObservationAction, InvalidToolCallAction}` | `Continue/Proceed` / `Patch/Rewrite/Select/Retry` / `Stop/Deny` per event |
| `RequestPatch` | `rig_agent::agent::RequestPatch` | per-turn request patch: `.history(items)`, `.extra_context(docs)`, `.temperature`, `.max_tokens`, merged in hook order |
| `MultiTurnStreamItem` | `rig_agent::agent::MultiTurnStreamItem` | `StreamAssistantItem(Item<StreamEvent>)` / `ToolCall { tool_call }` / `ToolExecutionCommitted { tool_call }` / `StreamUserItem(StreamedUserContent)` / `CompletionCall(CompletionCall)` / `ModelTurnRetried { turn }` / `FinalResponse(PromptResponse)` |
| `StreamingResult` / `StreamingError` | `rig_agent::agent::{StreamingResult, StreamingError}` | `Pin<Box<dyn Stream<Item = Result<MultiTurnStreamItem, StreamingError>> + Send>>`; errors: `Completion(ProviderError)` / `Report(ErrorReport)` / `Prompt(PromptError)` |
| `StreamedUserContent` | `rig_core::streaming::StreamedUserContent` | `ToolResult { tool_result }` (correlate via `tool_result.call`) |
| `CompletionCall` (stream) | `rig_agent::agent::CompletionCall` | per-request details: `call_index`, `usage`, ids, `finish_reason`, `raw` |
| `stream_to_stdout` | `rig_agent::agent::stream_to_stdout` | helper to print a stream |
| `Tool` / `DynamicTool` / `ToolSet` | `rig_agent::tool::{Tool, DynamicTool, ToolSet, ToolContext, …}` | `const NAME`, `Args`, `Output`, `Error`, `description`, `parameters`, `call(&self, ctx, args)`; `DynamicTool::new(name, desc, params, args-only-callback)` / `new_with_context(name, desc, params, (ctx, args)-callback)` |
| `ToolOutput` / `ToolResult` / `ToolExecutionError` / `ToolErrorKind` | `rig_core::tool::{ToolOutput, ToolResult, ToolExecutionError, ToolErrorKind}` | `ToolOutput::text/json/content(..)` with `as_text/as_json/render`; `ToolResult::success/failed/skipped/with_output/is_error`; `ToolExecutionError::invalid_args/other/…` constructors |
| `ContextValue` | `rig_core::tool::ContextValue` (trait) + `#[derive(rig_core::ContextValue)]` | typed serde-backed keys for `ToolContext::insert/get/require/insert_result/result/require_result` |
| `rig_tool` | `#[rig::rig_tool]` / `#[rig_agent::rig_tool]` | function → `Tool` impl (attribute macro; `tool_macro` renamed) |
| `ToolServer` / `ToolServerHandle` | `rig_agent::tool::server::{ToolServer, ToolServerHandle}` | shared mutable tool set via message passing; `agent.tool_server_handle()` |
| `PromptError` / `StreamingError` | `rig_agent::completion::PromptError`, `rig_agent::agent::StreamingError` | `CompletionError(ProviderError)` / `Report(ErrorReport)` / `MemoryError` / `MaxTurnsError { max_turns, chat_history, prompt }` / `PromptCancelled` / `UnknownToolCall` |
| `Extractor` / `ExtractorBuilder` | `rig_agent::extractor::{Extractor, ExtractorBuilder}` | `ExtractorBuilder::<T>::new(model).retries(n).build()`, `extract(..).await?.output` |
| `TypedRun` / `TypedPromptResponse` | `rig_agent::agent::{TypedRun, TypedPromptResponse}` | `agent.prompt_typed::<T>(..).await?.output` |
| `Embed` (derive / trait) | `#[derive(rig_core::Embed)]` / `rig_core::embeddings::Embed` | `fn embed(&self, &mut TextEmbedder) -> Result<(), EmbedError>` |
| `EmbeddingsBuilder` | `rig_core::embeddings::EmbeddingsBuilder` | `.new(model: impl Into<DynModel<op::Embedding>>)` / `.document(s)?` / `.build().await? → Vec<(T, Vec<Embedding>)>` |
| `Embedding` | `rig_core::embeddings::Embedding` | `vec: Vec<f64>` |
| `ModelInfo` / `ModelList` | `rig_core::model::{ModelInfo, ModelList}` | listing entries (`id`, `name`, `context_length`, …) / `{ data: Vec<ModelInfo> }` via `client.list_models()` |
| `VectorStoreIndex` / `InsertDocuments` | `rig_core::vector_store::{VectorStoreIndex, InsertDocuments}` | `top_n(req)` → `Vec<(score, id, doc)>`; insert built embeddings |
| `InMemoryVectorStore` | `rig_core::vector_store::in_memory_store::InMemoryVectorStore` | `.from_documents(pairs)`, `.index(model)` |
| `VectorSearchRequest` | `rig_core::vector_store::request::VectorSearchRequest` | `.builder().query(...).samples(n).build()` |
| `ConversationMemory` / `InMemoryConversationMemory` | `rig_core::memory::{ConversationMemory, InMemoryConversationMemory, MessageFilter, Compactor}` | in-process memory contracts; `.with_filter(..)`; wired via `AgentBuilder::memory(..)` |
| `test_utils` | `rig_core::test_utils` / `rig_agent::test_utils` (feature `test-utils`) | `MockCompletionModel` (`Model<MockScript, MockRuntime>`), `MockStreamEvent` (`.text/.tool_call/.final_response…`), mock memory/clients |
| `schemars` / `serde` / `serde_json` | `rig_core::{schemars, serde, serde_json}` | re-exports so macro-generated code resolves through rig; **schemars v1.0** |

## Provider List (rig-core 0.43)

Modules under `rig_core::providers::*`, each with its own client type unless marked: `openai` (`OpenAI`), `anthropic` (`Anthropic`), `gemini` (`Gemini`), `ollama` (`Ollama`), `copilot` (`Copilot`), `cohere` (`Cohere`), `voyageai` (`VoyageAi`, embeddings/rerank only), `chatgpt` (dialect + auth — the client is `openai::OpenAI`), `azure` (dialect consts — client is `openai::OpenAI`), and OpenAI/Anthropic-shaped **vendors** whose `from_env()`/`new(key)` return the shared client: `deepseek`, `doubleword`, `groq`, `huggingface`, `hyperbolic`, `llamacpp` (serves the old `llamafile` transport), `minimax`, `mira`, `mistral`, `moonshot`, `perplexity`, `together`, `venice`, `xai` (`xai::DIALECT`), `xiaomimimo`, `zai`. `providers::registry::ProviderRef` builds clients from data when the provider is chosen programmatically.

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
3. **Forgetting `max_turns` on multi-tool prompts** — it is the *total* model-call budget (1 = initial only); chained tool calls fail with `PromptError::MaxTurnsError`.
4. **Double-appending history with `chat`** — `chat(prompt, &mut history)` already appends the committed turn (including tool calls/results). `.history(..)` on a runner does NOT (it only seeds the input — the committed messages come back on `PromptResponse.messages`).
5. **Mixing embedding models** — query embeddings must come from the exact same model id used to embed the stored documents.
6. **Hand-writing tool `required` arrays wrong for OpenAI** — the Responses API requires every input parameter under `required`; use `schemars::JsonSchema` (non-`Option` fields are required) or the macro's `required(...)`.
7. **Looking for `client.agent(..)` / `CompletionModel` / `EmbeddingsClient`** — removed in 0.43. Build `AgentBuilder::new(model.erase())` and `client.embedding(id, ndims)` instead.
8. **Using `OneOrMany`** — removed in 0.42. Message content is `Vec<T>`. Construct with `Message::user`/`Message::assistant` or the `UserContent::Text(Text::new(..))` shapes.
9. **Reading reasoning via `Reasoning.display_text()` off a message** — `AssistantContent::Reasoning` carries a `Sealed<Reasoning>`; open it with its `Issuer` (or round-trip through serde for size heuristics). Streaming reasoning is plain fragments on `StreamEvent::Reasoning`.
10. **Storing bare values in `ToolContext`** — `insert_result(5u64)` does not compile; values are `ContextValue`s (typed + serde + a stable `KEY`). Wrap in a newtype with `#[derive(rig_core::ContextValue)]`, and treat both `insert_result`/`result::<T>()` as `Result`-returning.
11. **Matching `on_tool_result` / `ToolResultAction` / `internal_call_id`** — gone in 0.43. Use `on_outcome` + `OutcomeEvent.call_id`/`tool_result()` + `OutcomeAction::rewrite_tool_result`, and correlate stream items via `CallId` (`tool_call.id` ↔ `tool_result.call`).
12. **Matching streamed tool calls through `StreamEvent::End` tool-call parts** — the agent surfaces the model's calls as `MultiTurnStreamItem::ToolCall` when the turn commits; count those once (the `End` parts are for transcript/replay use).
13. **Assuming `from_env()` works without the API key env var** — each provider reads a specific var (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `COHERE_API_KEY`, …); Ollama needs none.
14. **Using `schemars` 0.8 idioms** — v1.0 is required; descriptions are `///` doc comments and the macro is `schemars::schema_for!`.
15. **Building `rig-reqwest`-backed clients without the `reqwest` feature** — `Config::…client()` is behind `rig-core/reqwest`; without it you must `.connect(http)` with an `HttpClientExt` transport of your own.
16. **Forgetting that hook `observes()` gates outcomes in 0.43** — a hook that returns `false` for `StepEventKind::ToolDispatch` silently stops seeing tool results (0.42's `observes` only hinted about deltas).

## External Resources

- **Rig website / guide**: https://rig.rs/docs
- **Rig API docs**: https://docs.rs/rig/latest/rig/
- **GitHub**: https://github.com/0xPlaygrounds/rig
- **Examples**: https://github.com/0xPlaygrounds/rig/tree/main/examples
- **awesome-rig**: https://github.com/0xPlaygrounds/awesome-rig
- **Discord**: https://discord.gg/playgrounds

## Complete File Index

| File | Description |
|------|-------------|
| `SKILL.md` | Main entry point — quickstart, mental model, decision tree, cheat sheet, repo conventions |
| `references/providers-and-clients.md` | Config + wire + transport architecture, per-provider clients & dialects, model-building methods (`completion/chat/responses/embedding`), `HttpClientExt`/transports, `from_env`, OAuth authenticators, `registry::ProviderRef`, the full provider table |
| `references/completions.md` | `CompletionRequest`/`CompletionResponse`, `Model`/`DynModel` direct calls, `Message`/`UserContent`/`AssistantContent` (incl. `Sealed<Reasoning>`), `Usage` (`Option<u64>` counters), `ProviderError`/`ErrorReport` |
| `references/agents.md` | `Agent`/`AgentBuilder` (models, preamble, context, tools, routes, memory), `agent.prompt(..)` → `AgentRunner`, the loop, `max_turns`, `ToolChoice`, manager-worker, `chat()` history write-back |
| `references/agent-runner.md` | `AgentRunner` per-run controls (`max_turns`, `history`, `preamble`, `tool_concurrency`, `tool_context`, `using_model`, `add_hook`), `run()` vs `stream()` vs `run_channel()`, `AgentRun` sans-IO state machine |
| `references/hooks.md` | `AgentHook` typed methods + per-event action enums (`CompletionCallAction`/`ModelTurnAction`/`ObservationAction`/`DispatchAction`/`OutcomeAction`/…), `RequestPatch`, invalid-tool-call recovery, `observes` gating, `HookStack` composition |
| `references/tools.md` | `Tool` trait, `DynamicTool` (`new` vs `new_with_context`), `ToolContext` + `ContextValue`/`insert_result`, `ToolOutput`/`ToolResult`/`ToolExecutionError`, `rig_tool` derive, `ToolEmbedding` + `ToolSet`, `ToolServer`, tool RAG |
| `references/streaming.md` | `.stream()` runs, `MultiTurnStreamItem` (incl. `ToolCall` commits + `ToolExecutionCommitted`), core `Item`/`StreamEvent` parts, `StreamedUserContent`, `CallId` correlation, `StreamingError`, `stream_to_stdout` |
| `references/structured-output.md` | `Extractor`/`ExtractorBuilder` (submit output-tool flow), `TypedRun`/`prompt_typed`, output schemas/modes, `StructuredOutputError`, extractor vs typed prompt |
| `references/embeddings.md` | `Embed` derive + manual impl, `TextEmbedder`, `EmbeddingsBuilder` (`DynModel<Embedding>`), `Embedding` (`vec: Vec<f64>`), `InsertDocuments`, best practices |
| `references/rag-and-vector-stores.md` | RAG phases, `VectorStoreIndex`/`InsertDocuments`, `InMemoryVectorStore`, `dynamic_context(n, index)`, `VectorSearchRequest`/`top_n`, tool RAG (`ToolEmbedding`/`ToolSet`/`dynamic_tools`), LSH store, limitations |
| `references/memory.md` | `ConversationMemory`/`MessageFilter`/`Compactor` contracts, `InMemoryConversationMemory`, memory via `AgentBuilder::memory`, bypass rules, run memory appends (`PromptResponse.memory_append`) |