---
name: rig
description: >-
  You are an expert in Rig, the Rust library for building scalable, modular, and
  ergonomic LLM-powered applications. You help developers wire up provider
  clients (OpenAI, Anthropic, Gemini, Ollama, and 20+ others), build agents
  with tools and RAG context, stream completions token-by-token, extract typed
  structured output, manage conversation memory, and hook the agent loop for
  guardrails and approvals. This skill covers Rig 0.41.x (the `rig` facade
  re-exporting `rig-core` and the optional `rig-agent` classic runtime) as used
  by the `shuvarie-llm` crate. Docs: guide at https://rig.rs/docs and API docs
  at https://docs.rs/rig/latest/rig/.
license: MIT
metadata:
  author: shuvarie
  version: 0.41.0
  category: Backend Development
  tags:
    - rust
    - llm
    - ai
    - agents
    - rag
    - streaming
    - openai
    - anthropic
    - gemini
    - ollama
---

# Rig — LLM-Powered Applications in Rust

Rig is a Rust library for building LLM-powered applications and agents. It gives you unified abstractions over **model providers** (OpenAI, Anthropic, Gemini, Cohere, Ollama, and 20+ others), **vector stores**, **tools**, and **RAG pipelines**, so you can wire up an agent in a few lines and scale to a production system without changing frameworks.

This project (`shuvarie`) pins **Rig 0.41.0** in `crates/llm/Cargo.toml`. The `shuvarie-llm` crate is a thin wrapper over `rig` that exposes a `Provider` enum, model listing, and a streaming completion API; the root binary never calls `rig` directly (see `AGENTS.md`).

Official docs: **Guide** at https://rig.rs/docs and **API docs** at https://docs.rs/rig/latest/rig/.

## Workspace / facade layout

Rig separates portable provider/backend contracts from agent orchestration:

- `rig-core` — provider-neutral messages, completion models, portable tools, memory and vector-store contracts, and built-in provider mappings. Always pulled in.
- `rig-agent` — the classic builder, prompt/streaming traits, typed hooks, contextual tools, extraction, and the serializable `AgentRun` state machine. Enabled by default; the root `rig` facade re-exports both at familiar `rig::...` paths.
- Companion crates (`rig-lancedb`, `rig-qdrant`, `rig-mongodb`, `rig-neo4j`, `rig-surrealdb`, `rig-postgres`, `rig-sqlite`, `rig-milvus`, `rig-scylladb`, `rig-fastembed`, `rig-memory`, `rig-bedrock`, `rig-candle`, `rig-gemini-grpc`, `rig-helixdb`, `rig-s3vectors`, `rig-vectorize`, `rig-vertexai`) — feature-gated modules on the `rig` facade. Enable only what you use.

Depend on the root `rig` facade for the full feature-gated surface, or on `rig-core` directly when you only need the portable contracts.

```toml
[dependencies]
rig = { version = "0.41", features = ["derive"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Critical Rules

Before writing Rig code in this repo, know these constraints:

- **The root binary never calls `rig` directly.** All provider access goes through `shuvarie-llm` (see `AGENTS.md`). Put LLM/SDK glue in `crates/llm/`, not in the TUI.
- **Library crates use typed errors, not `eyre`.** `shuvarie-llm` defines `LlmError` (thiserror). Don't bubble `anyhow`/`eyre` out of `shuvarie-core` or `shuvarie-llm`; `color-eyre` is binary-only.
- **`shuvarie-llm` must stay side-effect-free w.r.t. the TUI.** Long-running LLM work runs on the spawned core task over `tokio::sync::mpsc`; never `await` inside the TUI loop. The LLM layer exposes async APIs the core task drives.
- **Streaming tokens belong on the core task.** Use `StreamingPrompt`/`StreamingChat` on the core task and forward `MultiTurnStreamItem` / `StreamedAssistantContent` events to the TUI via the existing channel; do not pull a stream from `handle_event`/`view`.
- **`schemars` v1.0 is required** for `JsonSchema` derives (tool args, extractors). Field descriptions move from `#[schemars(description = "...")]` to `///` doc comments; use `schemars::schema_for!(T)` (or `T::json_schema()`).
- **OpenAI Responses API requires every parameter listed under `required`.** Include a `"required"` array in hand-written tool schemas, or use the macro's `required(...)` helper, or derive from `schemars::JsonSchema` (which marks non-`Option` fields required).
- **`with_history` no longer appends (since 0.38).** `agent.prompt(...).with_history(hist)` does NOT append the new turn — record it yourself. `agent.chat(prompt, &mut hist)` DOES append (including tool calls/results) — don't double-push.
- **Query embeddings must match the stored model.** Vectors from different embedding models are not comparable; use the exact same model id at ingestion and query time.
- **`max_turns` defaults to 0** = initial request + one follow-up after tool execution. Multi-step tool chains need `.max_turns(n)` or the run fails with `PromptError::MaxTurnsError` (which carries the history).
- **Tool errors don't abort the prompt.** A `Tool::call` returning `Err` is sent back to the model as the tool result and the loop continues. Make error messages instructive (the model reads them to recover).
- **Hooks are awaited inline.** Keep them lightweight; offload network/disk to a background task. Hooks live on the `AgentRunner` (driver) layer, not the sans-IO `AgentRun`.

## Quickstart

```rust
use rig::client::{CompletionClient, ProviderClient};
use rig::completion::Prompt;
use rig::providers::openai;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let client = openai::Client::from_env()?;
    let agent = client
        .agent("gpt-5.5")
        .preamble("You are a helpful assistant.")
        .build();
    let answer = agent.prompt("Who are you?").await?;
    println!("{answer}");
    Ok(())
}
```

`#[tokio::main]` needs Tokio's `macros` + `rt-multi-thread` features (or `full`). `from_env()` reads `OPENAI_API_KEY` (or the matching var for each provider).

## The mental model

```
provider module ─→ Client ─→ Model ─→ Agent / Extractor / Index
  rig::providers::openai    openai::Client   client.completion_model("gpt-5.5")   client.agent("gpt-5.5").build()
```

The flow is always: **provider → client → model → agent/extractor/index**. Swapping providers is usually a one-line import + model-id change; the client methods (`agent`, `completion_model`, `embedding_model`) are the same across providers.

### Layered completion API

| You want… | Use |
|-----------|-----|
| a text answer to a one-off prompt | `Prompt` (`.prompt(...)`) |
| a conversation that carries history | `Chat` (`.chat(prompt, &mut history)`) |
| a typed struct instead of a string | `TypedPrompt` (`.prompt_typed(...)`) |
| tokens as they arrive | `StreamingPrompt` / `StreamingChat` / `StreamingCompletion` |
| to configure the request before dispatch | `Completion` (`.completion(...)` → `CompletionRequestBuilder`) |
| to bypass the agent loop entirely | `CompletionModel` directly (`.completion_request(...).send()`) |

Most apps reach for an **Agent**, which implements the high-level traits *and* runs the agent loop. Drop to a bare `CompletionModel` only when you need control over individual requests.

## Feature Decision Tree

Use this to decide which reference file to load:

**Need provider clients, capabilities, auth, custom base URLs, or the list of supported providers?**
→ Read `references/providers-and-clients.md`

**Need completions, `Prompt`/`Chat`/`TypedPrompt`, `CompletionModel`, `Message`, `Usage`, or `CompletionError`?**
→ Read `references/completions.md`

**Need agents, `AgentBuilder`, the agent loop, `max_turns`, `ToolChoice`, context, manager-worker, or `extended_details`?**
→ Read `references/agents.md`

**Need `AgentRunner` per-run controls (`max_turns`, `tool_concurrency`, `tool_extensions`, `conversation`, `add_hook`), or `AgentRun` (sans-IO state machine for durable approval flows)?**
→ Read `references/agent-runner.md`

**Need `AgentHook`, `StepEvent`, `Flow`, request overrides, guardrails, approvals, or invalid-tool-call recovery?**
→ Read `references/hooks.md`

**Need tools, `Tool` trait, `tool_macro`, `ToolEmbedding` (dynamic tools), tool servers, or MCP tools?**
→ Read `references/tools.md`

**Need streaming, `StreamingPrompt`/`StreamingChat`/`StreamingCompletion`, `MultiTurnStreamItem`, `StreamedAssistantContent`, `stream_to_stdout`, or `PauseControl`?**
→ Read `references/streaming.md`

**Need structured output, `Extractor`, `ExtractionError`, or the Extractor-vs-`TypedPrompt` choice?**
→ Read `references/structured-output.md`

**Need embeddings, `Embed` trait (derive or manual), `EmbeddingsBuilder`, or `Embedding`/`TextEmbedder`?**
→ Read `references/embeddings.md`

**Need vector stores, `VectorStoreIndex`, `InsertDocuments`, `InMemoryVectorStore`, `dynamic_context`, tool RAG, or `VectorSearchRequest`?**
→ Read `references/rag-and-vector-stores.md`

**Need conversation memory, `InMemoryConversationMemory`, `ConversationMemory` trait, sliding-window / token-window / compaction policies, or long-term memory?**
→ Read `references/memory.md`

## API Surface (Cheat Sheet)

| Item | Path | Purpose |
|------|------|---------|
| `CompletionClient` | `rig::client::CompletionClient` | `client.completion_model(id)`, `client.agent(id)` |
| `EmbeddingsClient` | `rig::client::EmbeddingsClient` | `client.embedding_model(id)` |
| `ProviderClient` | `rig::client::ProviderClient` | `from_env()` / `from_val(...)` |
| `AgentClientExt` | `rig::client::AgentClientExt` | `client.agent(id)` constructor (via `prelude`) |
| `Prompt` | `rig::completion::Prompt` | `agent.prompt("...").await?` → `String` |
| `Chat` | `rig::completion::Chat` | `agent.chat(prompt, &mut history).await?` (appends) |
| `TypedPrompt` | `rig::completion::TypedPrompt` | `agent.prompt_typed("...").await?` → `T` |
| `Completion` | `rig::completion::Completion` | `agent.completion(...)` → `CompletionRequestBuilder` |
| `CompletionModel` | `rig::completion::CompletionModel` | Provider trait: `completion(req)`, `stream(req)` |
| `CompletionRequestBuilder` | `rig::completion::CompletionRequestBuilder` | `.preamble(...)`, `.temperature(...)`, `.max_tokens(...)`, `.documents(...)`, `.tools(...)`, `.send()` |
| `CompletionResponse` | `rig::completion::CompletionResponse` | `choice: OneOrMany<AssistantContent>`, `raw_response: T` |
| `Message` | `rig::message::Message` | `Message::User { content }` / `Message::Assistant { content }`; `Message::user(...)`, `Message::assistant(...)` |
| `AssistantContent` | `rig::message::AssistantContent` | `Text` / `ToolCall` / `Reasoning` |
| `UserContent` | `rig::message::UserContent` | `Text` / `ToolResult` / `Image` / `Audio` / `Document` / `Video` |
| `ToolChoice` | `rig::message::ToolChoice` | `Auto` / `None` / `Required` / `Specific { function_names }` |
| `Usage` / `GetTokenUsage` | `rig::completion::{Usage, GetTokenUsage}` | input/output/total/cached/reasoning tokens |
| `Agent` / `AgentBuilder` | `rig::agent::{Agent, AgentBuilder}` | model + preamble + context + tools + memory |
| `AgentRunner` | `rig::agent::AgentRunner` | per-run driver: `.max_turns(n)`, `.tool_concurrency(n)`, `.add_hook(h)`, `.run()` |
| `AgentRun` | `rig::agent::AgentRun` | sans-IO state machine (durable/resumable) |
| `AgentHook` / `StepEvent` / `Flow` | `rig::agent::{AgentHook, StepEvent, Flow}` | observe/steer the loop |
| `MultiTurnStreamItem` | `rig::agent::MultiTurnStreamItem` | `StreamAssistantItem(...)` / `FinalResponse(...)` |
| `stream_to_stdout` | `rig::agent::stream_to_stdout` | helper to print a stream |
| `Tool` / `ToolEmbedding` / `ToolSet` / `ToolError` | `rig::tool::{...}` | `const NAME`, `Args`, `Output`, `Error`, `definition`, `call` |
| `tool_macro` / `rig_tool` | `#[rig::tool_macro(...)]` / `#[rig::rig_tool]` | function → `Tool` impl |
| `ToolServer` / `ToolServerHandle` | `rig::tool::server::{ToolServer, ToolServerHandle}` | shared mutable tool set via message passing |
| `StreamingPrompt` / `StreamingChat` / `StreamingCompletion` | `rig::streaming::{...}` | `stream_prompt(...).await`, `stream_chat(...)`, `stream_completion(...)` |
| `StreamedAssistantContent` | `rig::streaming::StreamedAssistantContent` | `Text` / `ToolCall` delta / usage |
| `PauseControl` | `rig::streaming::PauseControl` | pause/resume a stream |
| `Extractor` / `ExtractionError` | `rig::extractor::{Extractor, ExtractionError}` | `client.extractor::<T>(id).build()`, `extract(...)` |
| `Embed` (derive) | `rig::Embed` | `#[derive(rig::Embed)]` with `#[embed]` fields |
| `Embed` (trait) | `rig::embeddings::Embed` | `fn embed(&self, &mut TextEmbedder) -> Result<(), EmbedError>` |
| `EmbeddingsBuilder` | `rig::embeddings::EmbeddingsBuilder` | `.document(s)?` / `.documents(vec)?` / `.build().await?` |
| `Embedding` | `rig::embeddings::Embedding` | document text + `Vec<f64>` |
| `VectorStoreIndex` | `rig::vector_store::VectorStoreIndex` | `top_n(req).await?` → `Vec<(score, id, doc)>` |
| `InsertDocuments` | `rig::vector_store::InsertDocuments` | `insert_documents(embeddings).await?` |
| `InMemoryVectorStore` | `rig::vector_store::in_memory_store::InMemoryVectorStore` | default dev store; `.add_documents(...)`, `.index(model)` |
| `VectorSearchRequest` | `rig::vector_store::VectorSearchRequest` | `.builder().query(...).samples(n).build()` |
| `ConversationMemory` | `rig::memory::ConversationMemory` | `load` / `append` / `clear` (async) |
| `InMemoryConversationMemory` | `rig::memory::InMemoryConversationMemory` | in-process; `.with_filter(...)` for policies |
| `prelude` | `rig::prelude` | common imports + `AgentClientExt` |
| `schemars` | `rig::schemars` | re-export; **v1.0** required |

## Provider List (rig-core 0.41)

`rig::providers::*` — each defines a `Client` and model constants: `anthropic`, `azure`, `chatgpt` (OAuth), `cohere`, `copilot`, `deepseek`, `doubleword`, `gemini`, `groq`, `huggingface`, `hyperbolic`, `llamafile`, `minimax`, `mira`, `mistral`, `moonshot`, `ollama`, `openai`, `openrouter`, `perplexity`, `together`, `voyageai`, `xai`, `xiaomimimo`, `zai`.

OpenAI-compatible vendors (Groq, Together, Hyperbolic, OpenRouter, DeepSeek, …) can also be reached through `rig::providers::openai::Client::builder().base_url(...)` when a dedicated module isn't needed.

## Conventions for This Repo

- **Import shape**: `use rig::client::{CompletionClient, ProviderClient};` + `use rig::providers::{openai, anthropic, gemini, ollama};` behind a `shuvarie-llm` `Provider` enum. The root binary never imports `rig`.
- **Provider abstraction**: `shuvarie-llm` wraps rig's per-provider `Client` types behind its own `Provider` enum so the TUI never names a provider concretely. Follow that boundary for any new provider.
- **Typed errors**: `shuvarie-llm` uses `thiserror`; map rig's `CompletionError`/`PromptError`/`ExtractionError` into `LlmError::Provider`/`LlmError::Model` rather than bubbling them.
- **No comments** unless explicitly requested (project-wide convention; `cargo fmt` defaults; empty `rustfmt.toml`).
- **Lint gate**: `cargo fmt --check` and `cargo clippy --all-targets` must pass before a change is considered done.

## Common Pitfalls

1. **Calling `rig` from the root binary** — go through `shuvarie-llm` so the TUI stays provider-agnostic and the core task owns the async work.
2. **Awaiting a stream on the TUI thread** — run `stream_prompt` on the core task and forward stream items over `mpsc` to the TUI.
3. **Forgetting `max_turns` on multi-tool prompts** — default is 0 (one follow-up); chained tool calls fail with `MaxTurnsError`.
4. **Double-appending history with `chat`** — `chat(prompt, &mut history)` already appends the turn (including tool calls/results). Don't push the user message / assistant reply again.
5. **Mixing embedding models** — query embeddings must come from the exact same model id used to embed the stored documents.
6. **Hand-writing tool `required` arrays wrong for OpenAI** — the Responses API requires every input parameter under `required`; use `schemars::JsonSchema` (non-`Option` fields are required) or the macro's `required(...)`.
7. **Returning `Flow::cont()` from `InvalidToolCall`** — that's treated as `Flow::fail()` (fail-fast default). Opt into `retry`/`repair`/`skip` explicitly.
8. **`override_request` with a narrow `active_tools` but a stale `ToolChoice`** — if you narrow tools, make sure any `tool_choice` still names an advertised tool, or Rig fails-closed.
9. **Assuming `from_env()` works without the API key env var** — each provider reads a specific var (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `COHERE_API_KEY`, …); Ollama needs none.
10. **Using `schemars` 0.8 idioms** — v1.0 is required; descriptions are `///` doc comments and the macro is `schemars::schema_for!`.

## External Resources

- **Rig website / guide**: https://rig.rs/docs
- **Rig API docs**: https://docs.rs/rig/latest/rig/
- **GitHub**: https://github.com/0xPlaygrounds/rig
- **Examples**: https://github.com/0xPlaygrounds/rig/tree/main/examples
- **awesome-rig**: https://github.com/0xPlaygrounds/awesome-rig
- **Discord**: https://discord.gg/playgrounds
- **Provider tests (cassette/replay)**: https://github.com/0xPlaygrounds/rig/tree/main/tests/providers

## Complete File Index

| File | Description |
|------|-------------|
| `SKILL.md` | Main entry point — quickstart, mental model, decision tree, cheat sheet, repo conventions |
| `references/providers-and-clients.md` | `Client`/`Provider`/`ProviderBuilder`/`Capabilities` system, `ProviderClient`/`CompletionClient`/`EmbeddingsClient`/`ModelListingClient`/`VerifyClient`, auth types, custom base URLs, supported providers, writing a custom provider |
| `references/completions.md` | `Prompt`/`Chat`/`TypedPrompt`/`Completion`, `CompletionModel`, `CompletionRequestBuilder`, `CompletionResponse`/`AssistantContent`, `Message`/`UserContent`, `Usage`/`GetTokenUsage`, `CompletionError` |
| `references/agents.md` | `Agent`/`AgentBuilder`, the agent loop, `max_turns`, `ToolChoice`, static + dynamic context, manager-worker, conversations & memory overview, `extended_details`, `additional_params` |
| `references/agent-runner.md` | `AgentRunner` per-run controls (`max_turns`, `max_invalid_tool_call_retries`, `history`, `conversation`, `without_memory`, `tool_concurrency`, `tool_extensions`, `add_hook`), `run()` vs `stream()`, `AgentRun` sans-IO state machine |
| `references/hooks.md` | `AgentHook::on_event`, `StepEvent` table, `Flow` action table, `RequestOverride`, guardrails/approvals, invalid-tool-call recovery (`fail`/`retry`/`repair`/`skip`), `observes`, composition & short-circuiting |
| `references/tools.md` | `Tool` trait, `schemars` schema derivation, `tool_macro`/`rig_tool`, tool failures, designing good tools, static vs dynamic tools, `ToolEmbedding` + `ToolSet` (tool RAG), `ToolServer`, MCP tools via `rmcp` |
| `references/streaming.md` | `StreamingPrompt`/`StreamingChat`/`StreamingCompletion`, `MultiTurnStreamItem`, `StreamedAssistantContent`, `StreamingCompletionResponse`, `stream_to_stdout`, `PauseControl`, per-chunk errors & backpressure |
| `references/structured-output.md` | `Extractor`, target type derives, `extract(...)`, `ExtractionError` (`NoData`/`DeserializationError`/`PromptError`), preamble/context, batch, `Extractor` vs `TypedPrompt` |
| `references/embeddings.md` | `Embed` derive + manual impl, `TextEmbedder`, `EmbeddingsBuilder` (`.document`/`.documents`), `Embedding`, `InsertDocuments`, best practices |
| `references/rag-and-vector-stores.md` | RAG phases, `VectorStoreIndex`/`InsertDocuments`, `InMemoryVectorStore`, `dynamic_context(n, index)`, `VectorSearchRequest`/`top_n`, tool RAG (`ToolEmbedding`/`ToolSet`/`dynamic_tools`), re-ranking, hybrid search, RAG-as-memory, limitations |
| `references/memory.md` | `InMemoryConversationMemory`, `ConversationMemory` trait (`load`/`append`/`clear`), bypass rules, `rig-memory` policies (`SlidingWindowMemory`/`TokenWindowMemory`/`CompactingMemory`/`DemotingPolicyMemory`), manual history & compaction, long-term memory |