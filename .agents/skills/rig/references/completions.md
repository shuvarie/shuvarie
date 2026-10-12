# Completions

Completions are the layer beneath agents: the call to send a single request to a language model and handle what comes back. There are no completion *traits* — a model is a concrete `Model<Wire, Transport>` value (or a `DynModel<Op>` handle) and the response types are portable. One-line prompting lives on the **agent** (`agent.prompt(..)`); full request control is building a `CompletionRequest` and calling the model.

Official docs: https://rig.rs/docs/concepts/completion · API: https://docs.rs/rig-core/latest/rig_core/completion/index.html (0.44: `rig-core-0.44.0/src/completion/`)

> Not in 0.44: the `Prompt`/`Chat`/`TypedPrompt` traits, the `CompletionModel` trait and `CompletionRequestBuilder`; `Sealed<Reasoning>` (an assistant reasoning block is plain `Reasoning { text, redacted, native }`); `ProviderError::MalformedToolInput` and `ProviderError::DuplicateCallId` (malformed arguments are kept on `ToolFunction::invalid_arguments`, a duplicate call id gets one rig issues); `ToolChoice::Specific { names }` (the field is now `function_names: Vec<ToolName>`).

## Choosing an interface

| You want… | Use |
|-----------|-----|
| a text answer through the agent loop | `agent.prompt("...").await` → `PromptResponse` (`rig_agent`; `.await` folds the run) |
| a conversation that carries caller-owned history | `agent.chat(prompt, &mut history).await` (appends committed turns) or `.prompt(..).history(h)` + `PromptResponse.messages` |
| a typed struct instead of a string | `agent.prompt_typed::<T>(..).await?.output` or `ExtractorBuilder` (see `structured-output.md`) |
| tokens as they arrive | `agent.prompt(..).stream()` (see `streaming.md`) |
| to configure and send one request yourself | `CompletionRequest::new(..)` + setters → `model.call(req)` / `model.stream(req)` |

## The model layer

```rust
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::driver::{DynModel, Model};
use rig_core::operation::Completion;
use rig_core::providers::openai::{self, OpenAI, wire::OpenAiWire};
use rig_core::streaming::CompletionStream;

let openai = OpenAI::from_env()?;                     // or openai::OpenAIConfig::from_env()?.connect(http)
let model: Model<OpenAiWire> = openai.completion(openai::GPT_5_2); // the dialect's default route

// call: the stream, finished — one decoder, one fold, one response constructor.
let response: CompletionResponse = model.call(CompletionRequest::new("What is Rust?")).await?;
println!("{}", response.text());

// stream: the same decoder and fold, frame by frame (encoding errors return here).
let stream: CompletionStream = model.stream(CompletionRequest::new("Hello"))?;
// `stream` yields `Item<StreamEvent>` items, then the error, if any; `stream.finish().await?` folds the reply.

// A runtime that stores any completion model erases the wire and transport:
let erased: DynModel<Completion> = model.clone().erase();
```

| | `Model<W, T = DynHttpClient>` | `DynModel<Op>` |
|---|---|---|
| built by | `client.completion(model)`, `Model::new(wire, transport)` | `Model::erase()`, `From<Model<W, T>>` (`DynModel<W::Op>`) |
| call | `call(req)` — `impl Into<Request<W>>` → `Future<Output = Result<Response<W>, ProviderError>>`; `call_observed(req, AdapterContext)` | same shape on `Op::Request`/`Op::Response` |
| stream | `stream(req) -> Result<Streamed<W::Op>, ProviderError>`; `stream_observed` | same |
| inspection | `name()` (provider), `id()` (model), `capabilities() -> wire::Capabilities`; public `wire`, `transport` | `name()`, `id()`, `capabilities()` |

- `Request<W>`/`Response<W>`/`Event<W>` are aliases of `W::Op`'s associated types; for a completion wire `Request<W> = CompletionRequest`, `Response<W> = CompletionResponse`, `Event<W> = StreamEvent`.
- Every completion call runs `Completion::prepare` **before** a wire encodes: the message content is validated, the request's model is resolved once (the request's `model`, else the wire's), each option the routed target refuses is reported (`options::check`), documents join the history, and the history is `adapt`ed for that model. `Model::call`/`stream` and their `_observed` twins all run it, so `DynModel`, every model the bus serves and the agent runtimes above them are covered. A wire with no replay target is `ProviderError::Request`.
- Streaming `finish()` is what `call` is: `Streamed<Op>::finish() -> Result<Op::Response, ProviderError>`; `partial() -> CompletionResponse` is what arrived so far (valid after an error or after the caller stopped polling) and `delivered(items) -> Vec<AssistantContent>` folds a partial item list.

## The request

`CompletionRequest` is `#[non_exhaustive]` and `Debug + Clone + Serialize + Deserialize`. Every field is public; each builder method changes the public fields as it is called, so call order matters.

| Field | Type | Notes |
|---|---|---|
| `model` | `Option<String>` | per-request override; `prepare` resolves it for encoders, the fold and replay |
| `chat_history` | `Vec<Message>` | the last message is the prompt; at least one message, non-empty user/assistant content |
| `documents` | `Vec<Document>` | request context documents (`id` + `text` + flattened `additional_props`), **not** `message::Document` |
| `tools` | `Vec<ToolDefinition>` | `{ name: ToolName, description: String, parameters: serde_json::Value }`; `ToolDefinition::new(..)` |
| `temperature` | `Option<f64>` | |
| `max_tokens` | `Option<u64>` | output-token cap; provider defaults/limits apply |
| `tool_choice` | `Option<ToolChoice>` | `Auto`/`None`/`Required`/`Specific` |
| `additional_params` | `Option<serde_json::Value>` | raw provider passthrough and provider-hosted tools; highest precedence |
| `output_schema` | `Option<schemars::Schema>` | native structured output where supported; the content is not deserialized for you |
| `record_telemetry_content` | `bool` | `#[serde(skip)]`, opt-in; exposes prompts, context, tool results and output in span attributes |
| `accept_unknown_finish_reasons` | `bool` | whether `FinishReason::Other` ends the turn as a normal stop; `ContentFilter` still fails |
| `options` | `GenerationOptions` | portable generation knobs (below) |
| `provider_options` | `ProviderOptions` | typed per-provider options (below) |

| Builder | Effect |
|---|---|
| `CompletionRequest::new(prompt)` / `From<&str>`/`String`/`Message` | one-message conversation; `From<Vec<Message>>` is the history as given |
| `preamble(text)` | inserts a `Message::System` **first** (a second call adds a second system message) |
| `message(..)`, `messages(..)` | insert before the prompt (the last message) |
| `model(..)`, `temperature(..)`, `max_tokens(..)`, `tool_choice(..)`, `output_schema(..)` | one field each (`None` clears where the field is optional) |
| `document(d)`, `documents(ds)` | request documents |
| `tool(t)`, `tools(vec)` | tools |
| `provider_tool(t)`, `provider_tools(vec)` | append to `additional_params.tools` with `"type": kind` forced from the typed field; a later `additional_params` with a `tools` key (or `None`) replaces them — set `additional_params` first |
| `additional_params(params)` | merges key by key into what is already set; `None` clears (provider tools included); logs each key that shadows a typed field set on the same request |
| `options(o)` | replaces every option; a shortcut called before it is lost, one after it sets its field on top |
| `reasoning(..)`, `cache(..)`, `service_tier(..)`, `verbosity(..)`, `parallel_tool_calls(..)`, `top_p(..)`, `seed(..)`, `stop(..)`, `on_unsupported(..)` | write that one field of `options`, keeping the rest |
| `provider_options(o)` / `provider_option(o)` | replace all entries / set one provider's entry |
| `record_content_telemetry(bool)` | content telemetry |
| `accept_unknown_finish_reasons(bool)` | see above |

| Read-only helper | Returns |
|---|---|
| `system_instructions()` | the leading `Message::System`'s content, where `preamble` put it |
| `validate_message_content()` | `Err(ProviderError::Request)` for an empty history or an empty user/assistant message, naming role and index; the driver runs it before encoding |
| `output_schema_name()` | the schema's `"title"`, else `"response_schema"` |
| `normalized_documents()` | the documents as one `Message::User` of `document_text(.., DocumentMediaType::TXT)` blocks, `None` when empty; `prepare` inserts them after any leading system messages unless the wire sends documents itself |
| `messages_for_telemetry()` | the history with those documents inserted |

**Precedence, lowest first: the mapped typed options → `provider_options` → `additional_params`.** `GenerationOptions` are mapped to the wire's body by the wire; provider options and `additional_params` merge *above* them at the body's top level, and a shadowed typed field is logged. `additional_params` must be a JSON object (or `null`, meaning nothing); its keys deep-merge recursively with what the typed layers wrote — every other value, arrays and `null` **included**, replaces, so a raw `null` is sent as `null` rather than erasing the key. The one sanctioned reader outside the merge is `options::param(target, request, key)`.

## Generation options (0.44)

`GenerationOptions` is the reusable value: build it once and hand it to an agent (`.options(..)`), overlay a run's on an agent's, or store it serialized with a request. It holds only what `CompletionRequest` has no field for — `temperature`, `max_tokens`, `tool_choice` and `output_schema` stay on the request.

| Item | Surface |
|---|---|
| `GenerationOptions` (`#[non_exhaustive]`, `Clone + Debug + Default + PartialEq`) | fields `reasoning: Option<Reasoning>`, `cache: Option<CacheRetention>`, `service_tier: Option<ServiceTier>`, `verbosity: Option<Verbosity>`, `parallel_tool_calls: Option<bool>`, `top_p: Option<f64>`, `seed: Option<u64>`, `stop: Vec<String>` (empty = none), `on_unsupported: Option<OnUnsupported>` |
| | `new()`, one setter per field, `is_default()`, `unsupported_policy() -> OnUnsupported` (the set one, else `Error`), `fields() -> OptionFields<'_>`, `overlay(&GenerationOptions) -> GenerationOptions` (every field `over` sets goes on top, a non-empty `stop` list and a set policy included) |
| `Reasoning` | `Off` \| `Effort(Effort)` \| `Budget { tokens: u32 }`; `From<Effort>` |
| `Effort` | `Minimal` \| `Low` \| `Medium` \| `High` \| `XHigh` \| `Max`, `as_str()` → `"minimal"…"max"` |
| `CacheRetention` | `None` \| `Short` \| `Long` (how long the provider keeps the prompt prefix cached) |
| `ServiceTier` | `Auto` \| `Default` \| `Flex` \| `Priority` |
| `Verbosity` | `Low` \| `Medium` \| `High`, `as_str()` |
| `OnUnsupported` | `Error` (default: the request fails with `UnsupportedOption`) \| `Ignore` (the option is skipped with a warning) |
| `UnsupportedOption` (`#[non_exhaustive]`, `Clone + Error`) | `option: Cow<'static, str>`, `provider: String`, `model: String`, `reason: String`; `new(..)`; carried by `ProviderError::UnsupportedOption` |

`is_default()` matters: a request whose options are default is **not** checked against its model's catalog entry, and setting any field — the policy included — turns the checks on. Under `OnUnsupported::Ignore` each refused option is reported once and cleared on the request; under `Error` the encode fails before anything is sent, so a refusal always names the option, the provider and the resolved model.

What a wire does with an option (`options::{mapping, merge}`), briefly:

- `OptionFields<'_>` — the request's options borrowed (not `#[non_exhaustive]`, so companion wires destructure it whole): `reasoning: Option<&Reasoning>`, `cache`, `service_tier`, `verbosity`, `parallel_tool_calls`, `top_p`, `seed`, `stop: &[String]`.
- `OptionMap { reasoning, cache, service_tier, verbosity, parallel_tool_calls, top_p, seed, stop }` — one `Mapping` per field, written by `ReplayTarget::map_options`. No `..` and no `Default`, so a new option fails to compile in every wire.
- `Mapping::{Nothing, Send(serde_json::Value), Omit(&'static str), Place, Unsupported(String)}` with `Mapping::of(option, f)`, `Mapping::of_stop(stop, f)` and `Mapping::unsupported(reason)`. `Nothing` for a set option is an encode error; `Send` must be a JSON object and is deep-merged above the wire's own encoding; `Place` is valid for `cache` only.
- `merge::{check, request_params, param, BaseInput, FinalBody, RawAt, Rewrite}` — `request_params(target, request, base, raw_at, rewrites) -> Result<FinalBody, EncodeError>` is the one merge (wire's own body, mapped options, provider options, `additional_params`, then `rewrites` in order); `BaseInput` is what the base builder may read (`cache()`, `param(key)`, `raw_tools()`, `refuse_cache(reason)`); `RawAt` says where the raw layer merges (`Top`, `Under(pointer)`, `Split { top, rest }` — Bedrock's `/additionalModelRequestFields`, Ollama's native `options`); `Rewrite` is a closed post-merge write list (`Stream(bool)`, `NoStream`, `StreamUsage`, and rig-internal entries); `FinalBody` has `is_empty()`, `get(key)`, `pointer(json_pointer)`, `into_body() -> wire::Body`, `deserialize::<T>()`.

## Typed provider options (0.44)

| Item | Surface |
|---|---|
| `ProviderExtension` | `const PROVIDER: &'static str` (the provider key, stable from 0.44: it keys both these options and the catalog's entries), `type Options: ExtensionOptions`, `type Extras: ReplyExtras` |
| `ExtensionOptions` | `type Ext: ProviderExtension<Options = Self>` (the provider an option type is stored for), `unsupported(&self, target, request) -> Vec<(&'static str, String)>` (fields the target cannot send; default none). Bounds: `Serialize + Clone + Debug + Send + Sync + UnwindSafe + RefUnwindSafe + 'static`, serialize-only |
| `ProviderOptions` | `new()`, `insert::<P>(&P::Options) -> Result<&mut Self, OptionsError>`, `set<O: ExtensionOptions>(O) -> Self`, `with::<P>(&P::Options) -> Result<Self, OptionsError>`, `get::<P>() -> Option<&Map<String, Value>>`, `remove::<P>()`, `contains::<P>()`, `is_empty()`, `overlay(&ProviderOptions)` |
| `ReplyExtras` | `from_reply(api: &Api, raw: &Value) -> Result<Self, serde_json::Error>`, the typed view of a reply's `raw` |
| `OptionsError` (`#[non_exhaustive]`) | `Serialize { provider, source: serde_json::Error }` \| `NotSections { provider }` |
| `SHARED` | `"*"`, the section every route of a provider reads |

- An entry is an object of **sections**: `SHARED` (`"*"`) plus one per route API (`"openai.chat"`, `"openai.responses"`, `"ollama.chat"`), each an object of body keys. A wire merges `"*"`, then the section of the route it encodes, at the top level of the body — above the mapped generation options, below `additional_params`; a section for another route is skipped.
- `set` stores the options under `O::Ext::PROVIDER`; a third-party provider reusing a built-in options type must use `with::<P>`, or its wire never reads them. Options that do not serialize are stored as a failed entry, which fails the **encode** of any request carrying them (and fails serializing the value, so the error is never dropped); `insert`/`with` report the same error at once.
- Read the reply side with `response.extras::<P>() -> Option<Result<P::Extras, serde_json::Error>>` (`None` unless `P` produced the reply) or `extras_lossy::<P>() -> P::Extras` (empty view on a mismatch or a decode failure).

| | `GenerationOptions` (`request.options`) | `ProviderOptions` (`request.provider_options`) | `additional_params` |
|---|---|---|---|
| shaped as | portable fields every wire maps or refuses | provider-keyed sections of body keys | one raw JSON object |
| checked | against the model's catalog entry (`ModelSpec::validate`) and the wire's `map_options` | against the entry's own `unsupported` | never; a shadowed typed field is logged |
| on refusal | `UnsupportedOption` error, or a warning and the option cleared | error, or a warning and that field removed from the entry | — |
| escapes | no | no | yes: exactly the bytes you wrote |

Per-provider modules (`extension`), each a `ProviderExtension` marker plus its options and extras: `openai::extension` (a directory: `OpenAiExt`, `OpenAiOptions`/`OpenAiExtras`, sections `OpenAiShared`, `ChatOptions`, `OpenAiResponsesOptions`), `anthropic::extension::{AnthropicExt, AnthropicOptions, AnthropicExtras}` (top_k, metadata_user_id, inference_geo, speed, task_budget, fallbacks, container, context_management, mcp_server(s), diagnostics; `anthropic-beta` flags required for the beta fields), `ollama::extension::{OllamaExt, OllamaOptions, OllamaExtras}` (`keep_alive`, `num_ctx`, `num_keep`, `top_k`, `min_p`, `repeat_penalty`, `repeat_last_n`, `num_gpu`, `num_thread`, `logprobs`, `top_logprobs` — the `"ollama.chat"` section goes to the native route only), `openrouter::extension::{OpenRouterExt, OpenRouterOptions, OpenRouterExtras, ProviderPreferences, ModelFallbacks}`; one `.rs` each for azure, chatgpt, cohere, copilot, deepseek, gemini, groq, llamacpp, minimax, mistral, moonshot, perplexity, together, venice, xai, xiaomimimo and zai (deepseek and perplexity ship extras only, no request options).

## Responses

`CompletionResponse` is `#[non_exhaustive]`; it deserializes through a repr that reconciles the finish reason with the choice.

```rust
pub struct CompletionResponse {
    pub choice: Vec<AssistantContent>,          // one block per provider item, in provider order; may be empty
    pub usage: Usage,
    pub origin: Origin,                         // wire API, provider, requested model, reported model/id
    pub error: Option<String>,                  // provider reported the turn failed → StopReason::Error, never replayed
    pub aborted: Option<String>,                // the caller stopped reading → StopReason::Aborted, never replayed
    pub provider_request_id: Option<String>,    // transport request id, from headers or SDK metadata
    pub raw: serde_json::Value,                 // the provider's own document (for .extras::<P>())
    // private: finish_reason, accepts_unknown_finish_reasons — read through the accessors
}
```

| Accessor | Returns |
|---|---|
| `provider()`, `model()`, `response_id()` | `&str`, `Option<&str>`, `Option<&str>` (the reported model and response id) |
| `finish_reason()` | `Option<FinishReason>`, reconciled with the choice |
| `stop()` | `StopReason` — how the turn ends, for history |
| `identity()` | `ResponseIdentity { response_id, provider_request_id }` (unreported ids stay `None`) |
| `accepts_unknown_finish_reasons()`, `accept_unknown_finish_reasons(bool)` | read/set the request's choice on the response |
| `with_finish_reason(r)`, `with_optional_finish_reason(Option<r>)` | set/clear it, reconciling a present reason with the choice |
| `text()`, `reasoning()` | the text / reasoning parts of `choice`, concatenated |
| `message()` | `Option<Message>` — the assistant turn to append (`None` for an empty choice) |
| `head()` | `AssistantMessage` with `origin` and `stop` and no content |
| `continued(content)` | `AssistantMessage` for a runtime that rolled a turn back: it keeps the origin, stops to call tools, and canonicalizes blocks it did not take verbatim |
| `tool_calls()` | `impl Iterator<Item = &ToolCall>` |
| `extras::<P>()`, `extras_lossy::<P>()`, `new(choice, usage, origin, raw)` | see provider options |

**Normalized replies.** A wire is one `Decoder` plus one `Fold` per operation (`operation::Completion`), and both `call` and `stream` run them: the fold absorbs each event the consumer took, then folds them with the provider's end (`operation::Finish`: usage, reason, response id, model, error) and the driver's `Reply` (provider name, the raw document, the request id) into one `CompletionResponse`. So a buffered reply and a streamed one normalize identically. Provider data is kept *beside* the normalized fields, never inside them: `raw` is the whole provider document (`Response::raw`), and each block keeps the provider item it was decoded from as its `native` (with a fingerprint), which replay sends back to the same model. `origin` names the wire (`Api`, e.g. `"anthropic.messages"`, `"openai.responses"`), the provider (`"openai"`), the requested model and the reported one; `message()`/`head()` carry that origin and the turn's `StopReason` into history, and the fold records `Origin::context` when the wire binds items to the request's tools and system prompt.

| `FinishReason` | Meaning |
|---|---|
| `Stop` | natural end |
| `Length` | output-token limit |
| `ToolCalls` | the model stopped to call tools |
| `ContentFilter` | the provider filtered the content |
| `Other(String)` | a provider value outside the vocabulary, carried verbatim (the enum is `#[non_exhaustive]`) |

`reconcile_with_output(has_tool_call)` turns `Stop` into `ToolCalls` when the choice holds a call (applied by the finish-reason setters and by the fold); `truncated_output()` is `Length | ContentFilter` — an unclassified `Other` is never truncation; `no_answer_message()` formats the answerless-turn diagnostic with budget/filter advice.

| `stop()` | When |
|---|---|
| `StopReason::Error(reason)` | `error` is set; or `finish_reason` is `ContentFilter`; or it is `Other` and `accepts_unknown_finish_reasons()` is false |
| `StopReason::Aborted(reason)` | `aborted` is set |
| `StopReason::Length` | `Length` |
| `StopReason::ToolUse` | `ToolCalls`; or `Stop`/`Other`/no reason with at least one tool call in `choice` |
| `StopReason::Stop` | `Stop`, or `Other` when accepted, or no reason |

**Unknown finish reasons are one outcome, a caller's choice, and never silent** (0.44): every unmapped provider value lands in `FinishReason::Other(..)`, the fold warns with the provider, the reason and whether it was accepted, and the request's `accept_unknown_finish_reasons` decides whether it stops the turn normally or fails it. `Other` also holds genuine failures (a malformed call, a recitation block), so accepting it lets such a reply's tool calls run. `ContentFilter` still fails, and a reported failure still fails. `StopReason::Error`/`Aborted` are kept in history but never replayed (`StopReason::is_failure()`), which is the rule `adapt` skips a turn by.

## Token usage and cost

Every response carries a `Usage`; the agent loop aggregates across turns (`PromptResponse.usage`, per call in `PromptResponse.completion_calls`).

```rust
#[non_exhaustive] // Debug, Default, PartialEq, Clone, Copy, Serialize, Deserialize (no Eq: `cost` is f64)
pub struct Usage {
    pub input_tokens: Option<u64>,               // every prompt token: uncached, cache-read, cache-written, hosted-tool
    pub output_tokens: Option<u64>,              // every generated token, reasoning included
    pub total_tokens: Option<u64>,               // input + output; absent unless both are reported
    pub cached_input_tokens: Option<u64>,        // the part of input read from a provider-managed cache
    pub cache_creation_input_tokens: Option<u64>,// the part written to one
    pub tool_use_prompt_tokens: Option<u64>,     // the part a provider's hosted tools added
    pub reasoning_tokens: Option<u64>,           // the part of output spent reasoning
    pub cost: Option<Cost>,                      // what the turn cost in USD, when known
}
```

- Invariants every provider mapping keeps: `cached_input_tokens + cache_creation_input_tokens <= input_tokens`, `reasoning_tokens <= output_tokens`, `total_tokens == input_tokens + output_tokens` (absent unless both are reported).
- A counter the provider did not send is `None`; a reported zero is `Some(0)`. `Usage::new()` is all-`None`; `is_reported()` says whether anything (a counter or a cost) was reported. Per-field builders take `impl Into<Option<u64>>` (`None` clears): `input_tokens`, `output_tokens`, `total_tokens`, `cached_input_tokens`, `cache_creation_input_tokens`, `tool_use_prompt_tokens`, `reasoning_tokens`, and `cost(impl Into<Option<Cost>>)`.
- `Add`/`AddAssign` sum per counter, treating an unreported side as zero without "un-reporting" a reported one; a `Usage` that reports nothing is the identity, so a fold from `Usage::default()` keeps its first turn's cost. The summed `cost` is `Some` only when both sides have one — an unknown turn makes the sum unknown rather than too low.
- `cost` is never derived from the token counters and none of them is derived from it. A cost the provider reports is its figure; the fold fills `cost` from the built-in catalog when the provider reported none (`Pricing::cost`), and never for ChatGPT, Copilot or Ollama (subscription, local, or ambiguous hosting). The catalog figure is the standard-tier list price of the counted tokens: no service tier, no long-context tiers, no hosted-tool fees, so it can be lower than the bill.

```rust
#[non_exhaustive] // Clone, Copy, Debug, Default, PartialEq
pub struct Cost { pub input: Option<f64>, pub output: Option<f64>, pub cache_read: Option<f64>,
                  pub cache_write: Option<f64>, pub total: f64 }
```

`Cost::from_parts(input, output, cache_read, cache_write)` (all parts known, `total` their sum) and `Cost::from_total(total)` (every part `None`); the per-field builders `input/output/cache_read/cache_write` set or clear one part and leave `total` alone. A part is `None` when its source does not give it and is never a placeholder `0.0`; summing keeps a part only when every side has it, while `total` always sums.

`cache_cost::{CacheCost, CacheRates}` is the caller-side calculator (not a provider mapping): `CacheCost { uncached_input, cache_reads, cache_writes, storage_token_hours }`, `CacheCost::from_usage(&Usage)` (input minus reads and writes, saturating), `prompt_tokens()`, `usd(&CacheRates)`, `uncached_usd(..)`, `saving(..)`, `Add`/`AddAssign`/`Sum`; `CacheRates { input, cached_read, cache_write, storage_per_hour }` are USD per 1M tokens (storage: per 1M token-hours), the caller's prices from the provider's list. Use it to price a run at rates the catalog does not carry.

## Messages

Message content is a plain `Vec<T>` (the `OneOrMany` container was removed in 0.42):

```rust
pub enum Message {
    System { content: String },
    User { content: Vec<UserContent> },
    Assistant(AssistantMessage),   // item-shaped from 0.44: blocks, provenance, how the turn ended
}

#[non_exhaustive]   // Clone, Debug, Default, Deserialize, Serialize, PartialEq
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,   // one block per provider item, in provider order
    pub origin: Option<Origin>,           // who produced it; `None` on a hand-built turn
    pub stop: Option<StopReason>,
}
```

- `AssistantMessage::new(content)` (hand-built: no origin, no stop), `rolled_back(origin, content)` (what a runtime rolled back, stopped to call tools), `aborted(origin, content, reason)`, `with_content(..)`, `with_origin(..)`, `with_stop(..)`, `tool_calls()`.
- `Origin { api: Api, provider: String, model: String, response_model: Option<String>, response_id: Option<String>, context: Option<Fingerprint> }` with `Origin::new(api, provider, model)` and `same_model(api, provider, model)` — the one sameness rule replay uses (a `context` fingerprint mismatch replays a turn as if from another model).
- `StopReason::{Stop, Length, ToolUse, Error(String), Aborted(String)}` (`#[non_exhaustive]`) with `is_failure()` = `Error | Aborted`.
- `MessageError::ConversionError(String)` converts to `ProviderError::Request`.

Constructors and `From` impls rather than hand-built enums (text-only case):

```rust
history.push(Message::user("What is Rust?"));
history.push(Message::assistant("A systems programming language..."));   // AssistantMessage::new(..)
// also: Message::system(s), Message::tool_result(call, name, text), Message::tool_results(results), Message::rag_text()
// From<String>/&str/&String/Text/Image/Audio/Document/ToolResult → user; ToolCall → assistant;
// From<AssistantContent>, AssistantMessage, UserContent, Vec<UserContent>, Vec<AssistantContent>, &Message.
```

```rust
#[non_exhaustive]
pub enum AssistantContent {         // serde tag "type", lowercase; deserialization requires the tag
    Text(Text),                     // answer text, with citations on an assistant block
    ToolCall(ToolCall),
    Reasoning(Reasoning),           // { text, redacted, native } — plain, not `Sealed<Reasoning>`
    Image(Image),                   // an image the assistant produced
    Opaque(Opaque),                 // a provider item with no canonical meaning
}

pub enum UserContent { Text(Text), ToolResult(ToolResult), Image(Image), Audio(Audio), Video(Video), Document(Document) }
```

- `AssistantContent::text(..)`, `image_base64(data, Option<ImageMediaType>, Option<ImageDetail>)`, `tool_call(id, name, arguments)`, `reasoning(text)`.
- Per-block helpers: `canonical()` (the block without its provider item), `is_answer()` (non-blank text, a call, an image), `is_blank()` (blank text or reasoning without a current item, redacted reasoning, an opaque item that does not replay), `fingerprint()`, `with_native(item)`, `native_item()` (the provider item while the block is still what it was decoded from — an edited block rebuilds canonically).
- `Text { text, native }` with private `citations`: `Text::new(..)`, `text()`, `citations() -> &[Citation]`, `span(Range<usize>) -> Option<Span>`, `with_citations(..)`, `clear_citations()`, `cited(&Citation) -> Option<&str>`; citations are bound to the text they were resolved against (`Fingerprint`), so they read empty once it is edited. A `Citation { span: Option<Span>, sources: Vec<Source> }` (byte span into the text, `None` for the whole block) and its `Source { location: SourceLocation, title, cited_text, confidence }` come from the wire, not from you. `Reasoning::new(text)`; signatures, encrypted payloads and ids live in `native`.
- `Native { item: serde_json::Value, fingerprint: Fingerprint }` — the provider item verbatim beside the canonical block it decoded to; `Fingerprint::of(&value)` is a 64-bit FNV-1a of the value's sorted-key JSON, stored as 16 hex digits, and an item whose block was edited becomes stale and is rebuilt instead of replayed. `Opaque { item, replay }` with `kind()` (the item's `type`): a provider item with no canonical meaning — a hosted-tool step, a compaction record — that only its own API reads; `replay` false means kept in history but never sent.

## Media and documents

| Type | Surface |
|---|---|
| `DocumentSourceKind` | `Url(String)` \| `Base64(String)` \| `FileId(String)` \| `Raw(Vec<u8>)` \| `String(String)` \| `Unknown`; `url/base64/file_id/string(..)`, `try_into_inner()`, `Display` |
| `Image` | `{ data: DocumentSourceKind, media_type: Option<ImageMediaType>, detail: Option<ImageDetail>, native }` |
| `Audio` / `Video` | `{ data, media_type }` / `{ data, media_type, additional_params: Option<serde_json::Value> }` |
| `Document` (message-level) | `{ data, media_type, additional_params }` — provider-specific fields (Anthropic's `title`, `context`, `citations`); **distinct from the request-level `completion::Document`**, which is `{ id, text, additional_props }` and renders as a `<file id: ..>` block |
| media types | `ImageMediaType::{JPEG,PNG,GIF,WEBP,HEIC,HEIF,SVG}`, `DocumentMediaType::{PDF,TXT,RTF,HTML,CSS,MARKDOWN,CSV,XML,Javascript,Python}` (+ `is_code()`), `AudioMediaType::{WAV,MP3,AIFF,AAC,OGG,FLAC,M4A,PCM16,PCM24}`, `VideoMediaType::{AVI,MP4,MPEG,MOV,WEBM}`, `ContentFormat::{Base64,String,Url}`, `MediaType::{Image,Audio,Document,Video}`, `ImageDetail::{Low,High,Auto}` (default `Auto`) |
| `MimeType` | `from_mime_type(&str) -> Option<Self>` / `to_mime_type() -> &'static str`, implemented for `MediaType` and each media-type enum |

Constructor names are consistent per media kind and never fetch or decode data (0.44):

| On | Constructors |
|---|---|
| `UserContent` | `text(t)`, `image_base64/image_raw/image_url(data, Option<ImageMediaType>, Option<ImageDetail>)`, `audio_base64/audio_raw/audio_url(data, Option<AudioMediaType>)`, `video_base64/video_raw/video_url(data, Option<VideoMediaType>)`, `document_base64/document_raw/document_url(data, Option<DocumentMediaType>)`, `document_text(text, Option<DocumentMediaType>)`, `tool_result(call, name, Vec<ToolResultContent>)` |
| `AssistantContent` | `text(t)`, `image_base64(data, Option<ImageMediaType>, Option<ImageDetail>)`, `tool_call(id, name, arguments)`, `reasoning(t)` |
| `ToolResultContent` | `text(t)`, `json(value)`, `image_base64/image_raw/image_url(data, Option<ImageMediaType>, Option<ImageDetail>)` |
| `Text` | `From<String>/&String/&str`; `From<String>` for `UserContent`/`AssistantContent`/`ToolResultContent` is the text form |

## Tool calls and results

```rust
pub struct ToolCall {
    pub id: CallId,              // THE id: the provider's, or one rig minted when it sent none
    pub function: ToolFunction,  // { name: ToolName, arguments: Map<String, Value>, invalid_arguments: Option<String> }
    pub native: Option<Native>,  // the provider item it was decoded from
}

pub struct ToolResult {
    pub call: CallId,            // echoes the answered ToolCall::id
    pub name: ToolName,          // executed tool's name (may differ after hook repair)
    pub content: Vec<ToolResultContent>,
    pub is_error: bool,          // the tool failed, was refused, or never ran
}
```

- `ToolFunction` arguments are **always a JSON object**: arguments the model sent that are not one are kept as text in `invalid_arguments`, with `arguments` holding what could still be read of them (`{}` when nothing), so a malformed call never fails a reply. `ToolFunction::new(name, value)` (an object is kept, a string holding one is unwrapped once, `null` is `{}`), `ToolFunction::parse(name, text)` (repairs, keeps a cut-off object's best prefix), `arguments_value()`.
- `ToolCall::new(id, function)`, `from_wire(wire_id, function)` (rig issues an id for an empty one), `result(content)` (sets `call` and `name` for you), `error_result(content)`.
- `CallId` is `Provider(ProviderCallId) | Local(LocalCallId)`: `CallId::from_wire("..")` (an empty id yields a rig-issued `LocalCallId::new()` v4 UUID), `provider() -> Option<&ProviderCallId>`, `is_local()`, `wire() -> Cow<str>`, `Display`. `ProviderCallId` is a non-empty newtype (`new(..) -> Option<Self>`, `as_str()`); `EmptyCallId` is its error. There is no dual-wire id in 0.44 — a wire with a call-id slot names it through `ReplayTarget::call_id_slot`.
- `ToolName` is a non-empty newtype (`.as_str()`, `Deref<str>`, `Display`, `PartialEq<str>`, `ToolName::new(..)?`); `EmptyToolName` is its error. `ToolDefinition` and `ToolChoice::Specific` names are `ToolName`s.
- Correlate a result with its call via `result.call == call.id`; build the reply with `call.result(content)` and send `Message::tool_results(results)`.
- `ToolResultContent` is `Text(Text) | Image(Image) | Json { value }` with `as_text()`, `as_json()`, `deserialize_json::<T>()` (structured JSON decodes directly; literal text is parsed only because you asked).
- `ToolChoice::{Auto, None, Required, Specific { function_names: Vec<ToolName> } }` (`Auto` is the default). `None` still declares tools to the wire as history; `Required`/`Specific` may be rejected by some models, and rig then avoids forcing its own output tool for those (`ProviderCapabilities::rejects_forced_tool_choice`).
- `ProviderCapabilities { composes_native_output_with_tools, rejects_forced_tool_choice }` (`new()`, `with_native_output_tool_composition(bool)`, `with_forced_tool_choice_rejected(bool)`) is what a runtime reads off `model.capabilities()` (`wire::Capabilities::completion(..)`).
- `ProviderToolDefinition { kind: String, config: Map<String, Value> }` (serde `type` + flattened config) with `new(kind)`, `with_config(key, value)`: a provider-hosted tool (web search, code execution), added through `provider_tool(s)` so it lands in `additional_params.tools`.

## History shaping

`completion::history` is where a stored conversation meets the model it is about to be sent to; `Completion::prepare` calls it, so you rarely do.

- `adapt(history: &[Message], target: &dyn ReplayTarget) -> Vec<Message>`: a turn whose `origin` names exactly the target's API, provider and model keeps its provider items (its reasoning and blank text are dropped unless they still hold a current item); any other turn keeps only canonical fields (items cleared, reasoning as plain text, opaque items dropped, call ids through `normalize_tool_call_id` and made distinct); content the model does not read is downgraded; opaque items marked not to replay, and turns that failed or were aborted, are dropped with the results answering them; every call left unanswered gets a `NO_RESULT_PROVIDED` error result and a result with no call is dropped; adjacent user messages merge; a turn left empty is dropped.
- `ReplayTarget` (every completion wire implements it): `api()`, `provider()`, `model()`, `accepts(model) -> Accepts`, `map_options(request, OptionFields) -> OptionMap`, and defaults for `encodes(model, Media)`, `normalize_tool_call_id`, `continues_stored(request)`, `declares_tools(request)`, `identity(item)`, `call_id_slot()`, `states_finish_reason()`, `binds_context(model)`, `drops_unbound_items(request)`, `route(request)`, `takes_documents()`, `needs_next(item)`, `hosted_pair(item)`, `starts_with_user()`, `later_system(model)`, `alternates_roles()`, `hosted_needs_tools()`, `result_parts(model)`, `sends_alone(block)`.
- Support types: `Accepts { user_images, assistant_images, tool_result_images, tools }` with `Accepts::ALL`/`Accepts::TEXT`; `Place::{User, ToolResult, Assistant}` and `Media::{Image(&Image, Place), Audio(&Audio), Video(&Video), Document(&Document)}` for `encodes`; `LaterSystem::{InPlace, Leading, UserText}`; `Pairing::{Use, Result}` for hosted-tool pairs; `Replay::{Item(Cow<Value>), Identity(Map), Rebuild}` is what an encoder sends for one block.
- Placeholder text constants: `NO_RESULT_PROVIDED`, `USER_IMAGE_OMITTED`, `ASSISTANT_IMAGE_OMITTED`, `TOOL_IMAGE_OMITTED`, `IMAGE_UNSENDABLE`, `AUDIO_UNSENDABLE`, `VIDEO_UNSENDABLE`, `DOCUMENT_UNSENDABLE`, `TOOL_IMAGE_ATTACHED`, `TOOL_IMAGES_HEADING`.
- Turn verdicts read by every runtime: `message::turn_delivered_no_answer(&[AssistantContent])` (no block is an answer — reasoning, opaque items and blank text are not) and `message::turn_failure(choice, Option<&StopReason>, Option<&FinishReason>) -> Option<String>` (a failing turn's calls ran none; an answerless turn cut by the output budget or a filter fails with `no_answer_message()`; a turn the provider failed fails even with an answer).

## Streaming

```rust
pub type CompletionStream = Streamed<Completion>;                       // rig_core::streaming
pub enum StreamEvent {                                                  // serde tag "event"
    Start { part: Part, kind: PartKind, name: Option<ToolName> },       // a call opens once its tool is named
    Text { part: Part, text: String },
    Reasoning { part: Part, text: String },
    Arguments { part: Part, json: String },                             // tool-call argument fragments, as they arrive
    End { part: Part, content: AssistantContent },
}
pub enum Item<E> { Event(E), Unknown(UnknownPayload) }                  // serde tag "item"
pub enum PartKind { Text, Reasoning, ToolCall, Image, Opaque }
```

- `Streamed<Op>` is the one stream every reply arrives as (a `futures::Stream<Item = Result<Item<Op::Event>, ProviderError>>`); an error is the last item; stop polling to pause, drop the stream to cancel. `finish()` resolves it into the response, `partial()` gives what arrived so far.
- `Part` is a position in the response's `choice` that only the writer constructs (it reads back from its serialized form); `StreamEvent::part()`/`name()`; `Transcript::parse(value) -> Result<Transcript, SequenceError>` is the one place a serialized event sequence is checked (each part starts once, grows only while open with its own kind of fragment, and ends once; a dropped item leaves a gap).
- `parse_partial_arguments(text) -> Map<String, Value>` is the display-only best object of a cut-off argument stream; `delivered(&[Item<StreamEvent>]) -> Vec<AssistantContent>` folds a partial item list.
- Bus relays: `Relayed::{Origin(Origin), Item(Item<StreamEvent>), Done(Box<CompletionResponse>)}`, `StreamEvents = WasmBoxedStream<'static, Result<Relayed, ErrorReport>>`, `Streamed::<Completion>::relay(label, events)` and `into_relay()`; a relay that ends without a response was cut short (`ProviderError::Truncated`). `UnknownPayload` is a redacted-`Debug` unmodeled payload (`value()` opts in).

## Errors

```rust
pub enum ProviderError {                     // #[non_exhaustive], Clone; one variant per ErrorKind
    Http(Arc<http_client::Error>),           // transport failure, no reply (a reply, any status, is ProviderResponse)
    Json(Arc<serde_json::Error>),
    Url(url::ParseError),
    Request(SharedError),                    // request could not be built (Arc<dyn Error + Send + Sync>)
    Response(String),                        // the reply decoded but does not answer the request
    Provider(String),                        // provider reported a failure, reply not preserved
    ProviderResponse(ProviderResponseError), // preserved reply: status, body, headers, request id
    InvalidAuthentication(ProviderResponseError),  // 401/403
    CacheExpired { name: String, response: ProviderResponseError },   // 403/404 on a cache handle
    MismatchedDimensions { provider: String, requested: usize, returned: usize },
    Relayed(Box<ErrorReport>),               // a report relayed over the bus; classifies as that report
    Truncated,                               // the reply ended before the provider ended it
    UnsupportedOption(UnsupportedOption),    // a GenerationOptions/provider field the wire or model refuses
}
```

- `ProviderError::request(reason)`, `from_http_response(status, body)`, `from_provider_body(body)`, `from_transport_error(err)`, `report() -> ErrorReport`.
- Classification: `error.kind() -> ErrorKind` (`Request | UnsupportedOption` → `Request`; `Response | MismatchedDimensions | Truncated` → `Response`; `ProviderResponse | InvalidAuthentication | CacheExpired` → `ProviderResponse`; `Relayed` → the relayed report's kind).
- `error.is_retryable()`: transport errors through `transient_transport` (`StreamEnded`/`Instance` and statuses 408/425/429/5xx are retryable; protocol, header and content-type errors are not), a preserved reply's own verdict, a relayed report's `retryable`, and `Truncated` (always true). Every other failure — rejected credentials, expired caches included — is not.
- Response accessors: `provider_response()`, `provider_response_body()`, `provider_response_json()`, `provider_response_status()`, `provider_request_id()`, `provider_response_headers()` (`Retry-After`), plus `with_provider_request_id/status/code/headers/transient` to enrich a preserved reply.
- `ErrorReport` is the serde-able wire form: `kind: ErrorKind`, `retryable: bool`, `message: String`, `code: Option<String>`, `http_status: Option<u16>`, `refusal: bool`, `source_chain: Vec<String>`, `request_id: Option<String>`, `provider_response: Option<ProviderResponseError>`, with `new(kind, message)`, `with_retryable/with_code/with_http_status/with_request_id`, `refused()`, `is_retryable()` and the same provider-response accessors. `ErrorKind` (snake-case `code()`) is `Http | Json | Url | Request | Response | Provider | ProviderResponse | Tool(ToolErrorKind) | MemoryBackend | MemoryPolicy | Internal | Cancelled | Timeout | BusClosed | HandlerUnavailable | Divergence | Denied | Other`.
- `EncodeError` is what `Wire::encode` returns: a transparent `ProviderError` that converts only into `Request`/`UnsupportedOption` (both `ErrorKind::Request`), with `EncodeError::request(..)`, `EncodeError::unsupported(UnsupportedOption)` and `unsupported_option()`. That is the error that carries `additional_params` shape failures, `OptionsError`s and refused generation options — before anything is sent.
- Runtime prompt failures wrap into `rig_agent::completion::PromptError` (`#[non_exhaustive]`): `Provider(ProviderError)`, `Report(ErrorReport)`, `Memory(MemoryError)`, `MaxTurns { max_turns, chat_history, prompt }`, `Cancelled { chat_history, reason }`, `UnknownToolCall { tool_name, available_tools, allowed_tools, chat_history }`; typed-output paths add `rig_agent::completion::StructuredOutputError { Prompt(PromptError), Deserialization { output, error }, EmptyResponse }`. See https://rig.rs/docs/concepts/error_handling for transient-vs-fatal handling and retry.

## Catalog (what a completion caller needs)

The built-in catalog is model facts keyed by provider and model id; a request consults it, so a completion caller needs only this much (the full API belongs to `providers-and-clients.md`):

- `Catalog::builtin() -> &'static Catalog`; `resolve("provider/model")` (also `vendor[/format]:model`), `get(ProviderId, model)` (exact), `find(ProviderId, model)` (exact, else without a dated snapshot suffix — what encoders use), `iter()`, `from_json(&str)`, `merge(overrides)`.
- `ModelSpec::validate(&GenerationOptions) -> Result<(), UnsupportedOption>` checks `reasoning` and `cache` against what the model takes **before anything is sent**; a request whose options are default skips the check.
- `Pricing::cost(&Usage) -> Option<Cost>` prices a usage in USD per 1M tokens (needs both input and output tokens; a cache rate the catalog does not list is charged at the input rate). The fold applies it to `usage.cost` when the provider reported no cost.

## Project boundary (shuvarie)

`shuvarie-llm` exposes its own async completion/streaming API and maps `ProviderError`/`PromptError` into `LlmError::Provider`/`LlmError::Model` (thiserror). The root binary never imports `rig_core`'s completion types directly; `crates/llm/src/retry.rs` matches `PromptError::Provider(ProviderError::Http | ProviderError::Provider)` into its own `ConnectionFailure` and classifies context-length rejections (`is_context_length_error`).
