# Hooks

Hooks let your code observe and steer the agent loop while `AgentRunner` drives the model and tool IO. Use them for logging, metrics, audit trails, approval flows, guardrails, request shaping, invalid-tool-call recovery, and streaming UI integration.

Official docs: https://rig.rs/docs/concepts/hooks (the surface below is read from the crate sources: `rig-agent-0.44.0/src/agent/hook.rs`, `src/run/policy.rs`, `src/run/patch.rs` — the docs site may lag).

A hook implements the `AgentHook` trait — a set of **typed methods, one per boundary**, each returning an event-specific action. Model calls and tool dispatches are *effects* on the run's bus, so the tool-call/tool-result boundaries are `on_dispatch` (before) / `on_outcome` (after); there is no `on_tool_call`/`on_tool_result`, and no `ToolResultAction`. Hooks live on the `AgentRunner` (driver) layer — the lower-level `AgentRun` state machine stays sans-IO. `AgentHook` is not generic over a model.

## Add hooks to a run or an agent

```rust
// One run (AgentRunner):
let response = agent
    .prompt("Check the balance, then summarize it.")
    .max_turns(3)
    .add_hook(ToolAudit)
    .await?;

// Every run from an agent (default hooks — appended on top of runner hooks):
let agent = rig_agent::AgentBuilder::new(my_model)
    .add_hook(ToolAudit)
    .build();
```

`AgentBuilder::add_hook` and `AgentRunner::add_hook` (which takes and returns `self`) both append to the same `HookStack` on the agent config. Agent-level hooks run first; per-run hooks are appended after the defaults.

## A minimal hook

```rust
use rig_agent::agent::{AgentHook, HookContext, OutcomeAction, OutcomeEvent};

struct ToolAudit;

impl AgentHook for ToolAudit {
    async fn on_outcome(&self, _ctx: &HookContext, event: OutcomeEvent<'_>) -> OutcomeAction {
        if let (Some(name), Some(result)) = (event.tool_name(), event.tool_result()) {
            println!("{} returned {}", name, result.output().render());
        }
        OutcomeAction::proceed()
    }
}
```

Event payloads borrow their data, so hooks inspect without taking ownership. A skipped call is visible here too: its outcome is `Ok(Outcome::ToolResult { result: ToolResult::skipped(reason) })`.

## Hook methods

`AgentHook` is declared as (every method below has a default; the trait spells its futures `-> impl Future<Output = T> + WasmCompatSend`, and impls may write `async fn on_*` or return an `async` block):

```rust
pub trait AgentHook: WasmCompatSend + WasmCompatSync {
    fn name(&self) -> Option<String>;
    fn on_run_start(&self, ctx: &HookContext, event: RunStart<'_>)
        -> impl Future<Output = RunStartAction> + WasmCompatSend;
    fn on_run_settled(&self, ctx: &HookContext, event: RunSettled<'_>)
        -> impl Future<Output = ()> + WasmCompatSend;
    fn on_model_select(&self, ctx: &HookContext, event: ModelSelection<'_>) -> ModelSelectionAction; // sync
    fn on_completion_call(&self, ctx: &HookContext, event: CompletionCallEvent<'_>)
        -> impl Future<Output = CompletionCallAction> + WasmCompatSend;
    fn on_model_turn_finished(&self, ctx: &HookContext, event: ModelTurnFinished<'_>)
        -> impl Future<Output = ModelTurnAction> + WasmCompatSend;
    fn on_invalid_tool_call(&self, ctx: &HookContext, event: &InvalidToolCallContext)
        -> impl Future<Output = Option<InvalidToolCallAction>> + WasmCompatSend;
    fn on_text_delta(&self, ctx: &HookContext, event: TextDelta<'_>)
        -> impl Future<Output = ObservationAction> + WasmCompatSend;
    fn on_reasoning_delta(&self, ctx: &HookContext, event: ReasoningDelta<'_>)
        -> impl Future<Output = ObservationAction> + WasmCompatSend;
    fn on_tool_call_delta(&self, ctx: &HookContext, event: ToolCallDelta<'_>)
        -> impl Future<Output = ObservationAction> + WasmCompatSend;
    fn on_dispatch(&self, ctx: &HookContext, event: DispatchEvent<'_>)
        -> impl Future<Output = DispatchAction> + WasmCompatSend;
    fn on_outcome(&self, ctx: &HookContext, event: OutcomeEvent<'_>)
        -> impl Future<Output = OutcomeAction> + WasmCompatSend;
    fn observes(&self, kind: StepEventKind) -> bool;
}
```

| Method | Signature (impl form) | Fires | Notes |
|--------|----------------------|-------|-------|
| `name` | `fn name(&self) -> Option<String>` | — | replay identity for the `HookStack`; `None` records the type name (module path stripped, generics kept) |
| `on_run_start` | `async fn on_run_start(&self, ctx, event: RunStart<'_>) -> RunStartAction` | once, before the first model call | not invoked for a run resumed past its first call; a rewrite that arrives after the run started cancels it |
| `on_run_settled` | `async fn on_run_settled(&self, ctx, event: RunSettled<'_>) -> ()` | run ended, success or error | terminal audit; returns nothing, so no steering |
| `on_model_select` | `fn on_model_select(&self, ctx, event: ModelSelection<'_>) -> ModelSelectionAction` | after every completion-call hook proceeds, before request preparation | sync, local, non-blocking: only `ModelRef` values and the `Scratchpad` |
| `on_completion_call` | `async fn on_completion_call(&self, ctx, event: CompletionCallEvent<'_>) -> CompletionCallAction` | before each model request | logging, metrics, per-turn request patches, overflow stops |
| `on_model_turn_finished` | `async fn on_model_turn_finished(&self, ctx, event: ModelTurnFinished<'_>) -> ModelTurnAction` | end of every accepted model turn (content parked, pre-commit) | accept, retry a tool-free turn, or stop |
| `on_invalid_tool_call` | `async fn on_invalid_tool_call(&self, ctx, event: &InvalidToolCallContext) -> Option<InvalidToolCallAction>` | a model-emitted call cannot be dispatched as written | `None` defers to the next hook, then to the run policy |
| `on_text_delta` / `on_reasoning_delta` / `on_tool_call_delta` | `async fn on_*(&self, ctx, event: TextDelta<'_> \| ReasoningDelta<'_> \| ToolCallDelta<'_>) -> ObservationAction` | streaming only, per fragment | live UI, content-policy stops; deltas stay provisional until the turn is accepted |
| `on_dispatch` | `async fn on_dispatch(&self, ctx, event: DispatchEvent<'_>) -> DispatchAction` | an effect is about to be dispatched (completion, tool, memory, retrieve, …) | approvals, argument rewriting, deny/skip |
| `on_outcome` | `async fn on_outcome(&self, ctx, event: OutcomeEvent<'_>) -> OutcomeAction` | an effect resolved (completion, tool result, …) | redact, truncate, normalize, replace |
| `observes` | `fn observes(&self, kind: StepEventKind) -> bool` | the event gate (see below) | true by default except memory/retrieve/embed/rerank/custom dispatches |

Per `CallModel` step the order is: completion-call hooks (patches merged in registration order) → model selection (which receives the merged patch) → request preparation against the selected model's capabilities → the attempt is issued. A completion-call stop therefore suppresses model selection and does not advance `ModelSelection::previous_model`. Only issued attempts advance `previous_model`, including failed ones.

## Action enums

- **`RunStartAction`** (`Debug, Clone, PartialEq`) — `Continue`, `Rewrite(Message)`, `Stop(String)`; constructors `continue_run()/rewrite(impl Into<Message>) / stop(impl Into<String>)`.
- **`ModelSelectionAction`** — `Continue`, `Select(ModelRef)`, `Stop(String)`; `continue_run()/select(impl Into<ModelRef>)/stop(..)`.
- **`CompletionCallAction`** — `Continue`, `Patch(RequestPatch)` (shape this turn), `Stop(String)`; `continue_run()/patch(..)/stop(..)`.
- **`ModelTurnAction`** — `Continue`, `Retry(RetryRequest)`, `Stop(String)`; `continue_run()/repeat()` = `Retry(RetryRequest::Repeat)`, `retry_with_feedback(impl Into<String>)` = `Retry(RetryRequest::Feedback(..))`, `stop(..)`. Retrying a turn containing tool calls is rejected (unanswered calls never enter history); retries consume the run's `max_turns` budget, and rig imposes no separate response-retry limit.
- **`ObservationAction`** — `Continue`, `Stop(String)`; `continue_run()/stop(..)`.
- **`DispatchAction`** — `Proceed`, `Patch(EffectKind)`, `Deny(ErrorReport)`; `proceed()/patch(kind)/deny(report)`, `skip(impl Into<String>)` = `Deny(ErrorReport::new(ErrorKind::Other, reason))` (the model sees a skipped result), `stop(impl Into<String>)` = `Deny(ErrorReport::new(ErrorKind::Cancelled, reason))` (cancels the run), `rewrite_tool_args(&EffectKind, impl Into<serde_json::Value>)` (a tool-call patch that keeps name and context; `Proceed` when `kind` is not a tool call), `try_rewrite_tool_args<T: Serialize>(&EffectKind, &T) -> Result<Self, serde_json::Error>`. A patch must keep the effect family; for a tool call only the arguments may change.
- **`OutcomeAction`** — `Proceed`, `Replace(Result<Outcome, ErrorReport>)`; `proceed()/replace(..)`, `stop(impl Into<String>)` (replaces with a `Cancelled` error to end the run after observing — how a hook ends a run on a result it just saw), `rewrite_tool_output(&OutcomeEvent, ToolOutput)` / `rewrite_tool_result(&OutcomeEvent, impl Into<String>)` (replace the model-visible output, keeping status and dispatch context; `Proceed` when the event did not resolve to a tool result).
- **`InvalidToolCallAction`** — `Fail`, `Retry { feedback }`, `Repair { tool_name }`, `Skip { reason }`, `Stop { reason }`; constructors `fail()/retry(fb)/repair(name)/skip(reason)/stop(reason)` (detail below).

A returned action an event cannot honor terminates the run with a diagnostic — the runner is fail-closed, it never silently ignores an action.

## Event and context types

| Type | Public shape |
|------|--------------|
| `RunStart<'a>` | `prompt: &Message`, `history: &[Message]` |
| `RunSettled<'a>` | `outcome: SettledOutcome<'a>`, `messages: Option<&[Message]>` (messages this run committed; `None` when it failed before its state existed) |
| `SettledOutcome<'a>` | `Response(&PromptResponse)` \| `Error(&str)` (the error's `Display` form) |
| `ModelSelection<'a>` | `prompt: &Message`, `history: &[Message]`, `request_patch: Option<&RequestPatch>`, `previous_model: Option<&ModelRef>`, `default_model: &ModelRef`, `selected_model: &ModelRef`; `ModelSelection::new(..)` |
| `CompletionCallEvent<'a>` | `prompt: &Message`, `history: &[Message]`, `turn: usize` |
| `ModelTurnFinished<'a>` | `turn: usize`, `content: &Vec<AssistantContent>`, `usage: Usage`, `identity: &ResponseIdentity`, `finish_reason: Option<&FinishReason>`, `max_tokens: Option<u64>`, `raw: &serde_json::Value` |
| `TextDelta<'a>` | `delta: &str`, `aggregated: &str` (accumulated for the turn) |
| `ReasoningDelta<'a>` | `part: Part`, `delta: &str`, `aggregated: &str` (scoped to that reasoning part) |
| `ToolCallDelta<'a>` | `part: Part`, `tool_name: &str`, `delta: &str`, `aggregated: &str` (**Changed in 0.44:** the fragment stream and its accumulator; the old `call_id` field is gone — the id arrives with the call's end) |
| `DispatchEvent<'a>` | `id: EffectId`, `kind: &EffectKind` (after earlier patches), `turn: usize`, `call_id: Option<&CallId>`, `context: Option<&ToolContext>`; helpers `tool_name()/tool_args()/tool_context()/completion_request()` |
| `OutcomeEvent<'a>` | `id: EffectId`, `kind: &EffectKind`, `outcome: &Result<Outcome, ErrorReport>` (after earlier replacements), `turn: usize`, `call_id: Option<&CallId>`, `context: Option<&ToolContext>`; helpers `tool_result()/tool_name()/tool_args()/tool_context()/completion()` |

`call_id`/`context` are `Some(..)` for a model-emitted tool call and `None` for completion, memory and retrieval effects. `Part` is `rig_core::streaming::Part`; read a streaming call's partial argument JSON with `rig_core::streaming::parse_partial_arguments(event.aggregated)`.

## Request patches

`CompletionCallAction::Patch(RequestPatch)` shapes one turn without mutating the agent — phased agents: force a search on turn 1, lower temperature for a critical step, shrink the advertised tool list, swap the history for a trimmed one:

```rust
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch
};
use rig_core::message::{ToolChoice, ToolName};

struct ForceSearchFirst;

impl AgentHook for ForceSearchFirst {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if event.turn == 1 {
            let search = ToolName::new("search_web").expect("advertised names are non-empty");
            CompletionCallAction::patch(
                RequestPatch::new()
                    .active_tools(["search_web"])
                    .tool_choice(ToolChoice::Specific {
                        function_names: vec![search],
                    })
                    .temperature(0.0),
            )
        } else {
            CompletionCallAction::continue_run()
        }
    }
}
```

`RequestPatch` (defined in `run/patch.rs`, re-exported from `agent::hook`) fields: `preamble: Option<String>`, `temperature: Option<f64>`, `max_tokens: Option<u64>`, `tool_choice: Option<ToolChoice>` (`Specific { function_names: Vec<ToolName> }`), `active_tools: Option<Vec<String>>`, `additional_params: Option<serde_json::Value>`, `extra_context: Vec<Document>`, `history: Option<Vec<Message>>`. Builders: `new()`, `preamble(impl Into<String>)`, `temperature(f64)`, `max_tokens(u64)`, `tool_choice(ToolChoice)`, `active_tools<I: IntoIterator<Item = S>, S: Into<String>>(values)`, `additional_params(serde_json::Value)`, `extra_context<I: IntoIterator<Item = Document>>(values)`, `context(Document)`, `history<I: IntoIterator<Item = Message>>(values)`, plus `is_empty()` and `merge(later)` (which takes `self` by value).

Patches are per-turn and non-sticky, and never mutate the agent's baseline. `HookStack::on_completion_call` merges them with `earlier.merge(later)` in registration order:

- `extra_context` — appended in order.
- `additional_params` — JSON objects shallow-merge, later top-level keys winning; a later non-object replaces an earlier value.
- `active_tools` — allow-lists are **intersected**.
- scalars (`preamble`, `temperature`, `max_tokens`, `tool_choice`) and `history` — last writer wins, with a `tracing::warn!` when two hooks set the same field.

If you narrow `active_tools`, make sure any `tool_choice` still names an advertised tool. The stack reports `CompletionCallAction::Continue` when the merged patch is empty, so `ModelSelection::request_patch` is `None`.

## Guardrails and approvals (dispatch boundaries)

```rust
use rig_agent::agent::{AgentHook, DispatchAction, DispatchEvent, HookContext};

struct TransferPolicy { max_auto_transfer: u64 }

impl AgentHook for TransferPolicy {
    async fn on_dispatch(&self, _ctx: &HookContext, event: DispatchEvent<'_>) -> DispatchAction {
        let Some("transfer_funds") = event.tool_name() else {
            return DispatchAction::proceed();
        };
        let amount = event
            .tool_args()
            .and_then(|args| serde_json::from_str::<serde_json::Value>(args).ok())
            .and_then(|v| v.get("amount").and_then(|a| a.as_u64()));
        match amount {
            Some(n) if n <= self.max_auto_transfer => DispatchAction::proceed(),
            Some(n) => DispatchAction::skip(format!(
                "denied by policy: ${n} exceeds ${} automatic limit",
                self.max_auto_transfer
            )),
            None => DispatchAction::skip("denied by policy: missing amount"),
        }
    }
}
```

A skip makes the model see a synthetic skipped result (and surfaces as an outcome with `ToolResult::skipped`); `DispatchAction::stop(reason)` cancels the run. A deny's salvage rule: a patch an earlier hook made before the denial is preserved as the reported effect, so the skipped result quotes the rewritten arguments.

This is a guardrail, not a security boundary. Enforce real authorization inside the tool or downstream service as well.

## Invalid tool calls

`InvalidToolCallReason` — why a call cannot be dispatched as written (**`#[non_exhaustive]`**):

- `UnknownTool` — the name is not an executable tool for this turn.
- `DisallowedByToolChoice` — the tool exists, but the active tool choice does not allow it (**new in 0.44:** 0.43 folded this into `UnknownTool`).
- `MalformedArguments { error }` — the arguments are not a JSON object; `error` is the parser's description and `args` keeps the raw text. `InvalidToolCallReason::malformed_arguments(raw)` builds one from the raw text.

`InvalidToolCallContext` (**`#[non_exhaustive]`**, all fields `pub`): `tool_name: String`, `tool_call_id: Option<CallId>`, `args: Option<String>`, `available_tools: Vec<String>`, `allowed_tools: Vec<String>`, `tool_choice: Option<ToolChoice>`, `chat_history: Vec<Message>`, `is_streaming: bool`, `reason: InvalidToolCallReason`.

`InvalidToolCallAction` is `Fail | Retry { feedback } | Repair { tool_name } | Skip { reason } | Stop { reason }` and is **`#[non_exhaustive]`** — match it with a `_ =>` arm. `None` from every hook defers to the run policy. Two paths:

**Unknown / disallowed name.** `Fail` fails the run with `PromptError::UnknownToolCall { tool_name, available_tools, allowed_tools, chat_history }`; `Retry { feedback }` appends the rejected turn plus corrective feedback and re-asks while `max_invalid_tool_call_retries` remains (default `0`; each retry also consumes the total model-call budget, and an exhausted budget fails as `UnknownToolCall`); `Repair { tool_name }` renames the call and revalidates the name against the turn's allowed tools (a disallowed name fails as `UnknownToolCall`); `Skip { reason }` answers with a synthetic skipped result without executing and is **rejected under `ToolChoice::None`**; `Stop { reason }` cancels the run. In a `HookStack` the **first `Some(..)` wins** and later hooks are not called. If every hook returns `None`, the run's `UnhandledInvalidToolCall` policy applies — `Fail` (default) or `Ignore`, which drops the call and continues the turn without suppressing sibling calls or response hooks. Set it with `AgentRunner::unhandled_invalid_tool_call(rig_agent::run::UnhandledInvalidToolCall::{Fail, Ignore})` (`run` only; `agent` does not re-export it) or `RunSpec.unhandled_invalid_tool_call`; a resumed run keeps the policy it persisted.

**Malformed arguments** (**new in 0.44:** the call is offered to this hook and always answered with a tool result). The call is offered **in the tool step, after its turn committed**: `None` answers with `rig_core::transcript::invalid_arguments_feedback(name, raw)`, `Retry { feedback }` with its feedback, `Skip { reason }` with `ToolResult::skipped(reason)`, `Stop { reason }` cancels the run, and `Fail` **or** `Repair { .. }` fails the run naming the tool and the parse error (renaming cannot fix arguments). Consecutive malformed calls are bounded by `AgentRunner::max_consecutive_malformed_tool_calls(impl Into<Option<usize>>)` (`RunSpec.max_consecutive_malformed_tool_calls`; `None`, the default, sets no limit): the counter increments per tool step that answers a malformed call and resets when a step's calls all parse — past the limit the run fails. `UnhandledInvalidToolCall` never applies to malformed arguments.

```rust
use rig_agent::agent::{
    AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext, InvalidToolCallReason,
};

struct RepairDefaultApi;

impl AgentHook for RepairDefaultApi {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        match (&event.reason, event.tool_name.as_str()) {
            // A name can be repaired; argument text cannot.
            (InvalidToolCallReason::UnknownTool, "default_api")
                if event.allowed_tools.iter().any(|name| name == "search_web") =>
            {
                Some(InvalidToolCallAction::repair("search_web"))
            }
            (InvalidToolCallReason::MalformedArguments { .. }, _) => Some(
                InvalidToolCallAction::retry("arguments must be a JSON object"),
            ),
            // `InvalidToolCallReason` is `#[non_exhaustive]`: keep a catch-all.
            _ => Some(InvalidToolCallAction::retry(format!(
                "use one of: {:?}",
                event.available_tools
            ))),
        }
    }
}
```

When any call in a turn is skipped — by this hook or by a tool-dispatch skip — **none** of that turn's calls execute: peers receive synthetic "not executed" results.

## Hook composition

Hooks are stored in a `HookStack` and run in registration order. How results compose is event-dependent:

- `on_run_start` — rewrites chain: each hook sees the latest rewrite, the last `Rewrite` is reported, and the first `Stop` is terminal.
- `on_run_settled` — every hook runs, in order; the return value is ignored.
- `on_model_select` — chain: each hook sees the earlier candidate, the **last** `Select` wins, a `Stop` is terminal.
- `on_completion_call` — patches accumulate and merge (rules above), a `Stop` is terminal.
- `on_dispatch` — chain: each hook sees the previous hook's patched kind. An illegal patch (family changed, or a tool call's target name changed) is turned into a denial of that dispatch with an internal report. The **first `Deny` wins** and prevents the remaining hooks.
- `on_outcome` — chain: each hook sees the previous hook's replacement, the **last** `Replace` wins. A replacement whose report is `Cancelled` short-circuits immediately and ends the run; later hooks cannot undo it.
- `on_model_turn_finished`, the three delta hooks and `on_invalid_tool_call` — first non-`Continue` (or first `Some`) wins; later hooks are not invoked.

Nested stacks keep the same semantics (a `HookStack` is itself an `AgentHook`, and `push` flattens a nested stack's names), so a salvage/rewrite survives at any nesting depth.

```rust
let response = agent
    .prompt("Process this request.")
    .add_hook(AuditLog)
    .add_hook(TransferPolicy { max_auto_transfer: 500 })
    .run()
    .await?;
```

## `observes` and the event gate

`StepEventKind` variants: `RunStart`, `RunSettled`, `CompletionCall`, `ModelTurnFinished`, `InvalidToolCall`, `TextDelta`, `ReasoningDelta`, `ToolCallDelta`, `CompletionDispatch`, `ToolDispatch`, `EmbedDispatch`, `RerankDispatch`, `MemoryDispatch`, `RetrieveDispatch`, `CustomDispatch`; `StepEventKind::for_family(EffectFamily) -> Self` maps a family to its dispatch kind.

The default `observes` is `!matches!(kind, EmbedDispatch | RerankDispatch | MemoryDispatch | RetrieveDispatch | CustomDispatch)` — **true for everything else**, so completion dispatches, tool dispatches and the lifecycle kinds are observed out of the box. What the gate actually does in 0.44:

- **Per hook, the dispatch/outcome boundaries.** Answering `false` for `CompletionDispatch` or `ToolDispatch` means that hook is *not called* for those effects and cannot patch, deny or replace them. This is the trap: overriding `observes` to keep a narrow set silently drops completions and tool calls.
- **Stack-level (aggregate) for the rest.** `HookStack::observes` is `any` over its hooks, and the engine uses it to skip work entirely: `on_run_start` and `on_run_settled` are not fired when no hook observes `RunStart`/`RunSettled`, and the streaming deltas are neither built nor dispatched when no hook observes `TextDelta`/`ReasoningDelta`/`ToolCallDelta`. Because that gate is aggregate, a hook that answers `false` for a delta kind can still be called with it while a sibling observes it.
- `CompletionCall`, `ModelTurnFinished` and `InvalidToolCall` are not gated at all.
- `impl AgentHook for ()` answers `false` for every kind (a stack of only `()` observes nothing).

```rust
use rig_agent::agent::{AgentHook, StepEventKind};

struct ToolOnlyHook;

impl AgentHook for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolDispatch)
    }
}
```

As written this hook also loses `on_run_start`, `on_run_settled` and the streaming deltas while it is the only hook, because no hook observes those kinds. Even with `observes`, hook methods should still return the continue/proceed action for events they ignore — a sibling hook may cause an event to be dispatched.

The delta events fire only on the streaming surface (`run()` never emits them); the dispatch/outcome boundaries fire on every surface.

## Run-scoped state: `HookContext`

`HookContext` is supplied to every hook: `run_id() -> RunId`, `turn() -> usize` (one-based model-call index; `0` before the first call), `is_streaming() -> bool`, `agent_name() -> Option<&str>`, `scratchpad() -> &Scratchpad`, `bind<F: Family>(key: &Key<F>) -> Result<RunHandle<'ctx, F>, ErrorReport>`, `index(key) -> Result<RunHandle<Retrieve>, _>`, `model(key) -> Result<RunHandle<Completion>, _>`, `tool(key) -> Result<RunHandle<Tool>, _>`, `memory(key) -> Result<RunHandle<Memory>, _>` (all `HandlerKey`), `append_entry<T: serde::Serialize>(kind, &value) -> Result<(), serde_json::Error>`, `entries(kind: &str) -> Vec<RunEntry>`, `last_entry(kind: &str) -> Option<RunEntry>`. `bind` fails when the context was built outside a run (`ErrorKind::BusClosed`) or when the key serves another family. Dispatch through a bound view does not re-enter hooks.

- **`Scratchpad`** — typed run-scoped storage, `Clone + Default` sharing in-process state: `insert<T>(value) -> Option<T>`, `get<T>() -> Option<T>`, `contains<T>() -> bool`, `remove<T>() -> Option<T>`, `update<T, R>(impl FnOnce(&mut T) -> R) -> R` — where `T: Clone + Send + Sync + 'static`, plus `Default` for `update`. Entries are never serialized and are not rewound when a run is forked, so use `append_entry` for state that must travel with the run.
- **`RunEntry`** — `{ kind: String, turn: usize, value: serde_json::Value }`, stored verbatim, excluded from provider requests, flushed into the run at step boundaries (not a disk write). Entries appended during `on_run_settled` stay local. The persisted envelope is `RUN_FORMAT = 2`; an unknown key or another format is refused on load.
- **`RunHandle<'ctx, F: Family>`** — `dispatch(F::Request)`, `key() -> &HandlerKey`, `descriptor() -> HandlerDescriptor`, `is_closed() -> bool`; on `family::Retrieve` also `top_n<T: DeserializeOwned>(VectorSearchRequest<Filter<Value>>) -> bus::Retrieval<T>`; on `family::Completion` also `call(CompletionRequest) -> bus::Completion` and `label() -> ModelRef`. It borrows the context (`PhantomData<&'ctx HookContext>`): not `'static`, not storable, not spawnable. A request future it returns may outlive the view but only runs while the run's driver serves; dropping it cancels that dispatch.
- **`RunId`** — process-local counter id (`rig_core::id`), serde-transparent, `RunId::new()` / `RunId::from_raw(u64) -> Option<Self>`; one per run.
- **`HookStack`** — `new()`, `with(hook)`, `push(hook)`, `names() -> Vec<String>`, `len()`, `is_empty()`; `Clone + Default`, and it implements `AgentHook` itself.

## Non-exhaustive enums

0.44 marks enums expected to grow `#[non_exhaustive]`. In the hook surface that is exactly the policy trio: `InvalidToolCallReason`, `InvalidToolCallAction` and `InvalidToolCallContext` — so a `match` over a reason or an action needs a `_ =>` arm, and the context can only be read, never constructed. Everything else the hooks touch is still exhaustive in 0.44: `RunStartAction`, `ModelSelectionAction`, `CompletionCallAction`, `ModelTurnAction`, `ObservationAction`, `DispatchAction`, `OutcomeAction`, `RetryRequest`, `StepEventKind`, `EffectKind`, `Outcome`, `ErrorKind`, `ToolChoice`. `FinishReason` has `Other(String)`, so a `match` on it still needs a catch-all.

## Best practices

- Keep hooks lightweight — awaited inline; slow hooks delay the next model/tool step.
- Offload logging/network/audit to background tasks.
- Treat hook guardrails as UX/policy, not authorization boundaries.
- Make hook state concurrency-safe when `tool_concurrency > 1` (or use the `Scratchpad`).
- Prefer one composed policy hook when multiple rules must produce one decision.
- Never narrow `observes` to the dispatch kinds alone unless the hook truly wants nothing else — you also lose `on_run_start`/`on_run_settled` and the streaming deltas while no sibling observes them.

## Project boundary (shuvarie)

- Hooks live in `shuvarie-llm`: `crates/llm/src/tool.rs` (`FileChangeHook`) implements `on_outcome` — imported as `rig_agent::agent::hook::{HookContext, OutcomeEvent}` plus `rig_agent::agent::{AgentHook, OutcomeAction}` — reading `event.tool_result()/call_id/tool_name/tool_context()`, using `ToolResult::is_error()` and returning `OutcomeAction::Proceed`.
- `crates/llm/src/context_hook.rs` (`ContextHook` / `ContextBudget`) uses `on_completion_call` with `CompletionCallAction::Patch(RequestPatch::new().history(..))`, `on_outcome` with `OutcomeAction::rewrite_tool_result(..)`, and narrows `observes` to `StepEventKind::ToolDispatch` (its completion-call hook is not dispatch-gated, so nothing else is lost).
- Both import paths — `rig_agent::agent::hook::{..}` and the `rig_agent::agent::{..}` re-exports (`AgentHook`, `HookContext`, `StepEventKind`, `RequestPatch`, `RunEntry`, …) — exist unchanged in 0.44. `UnhandledInvalidToolCall` is only at `rig_agent::run`.
