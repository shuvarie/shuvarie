# Hooks

Hooks let your code observe and steer the agent loop while `AgentRunner` drives the model and tool IO. Use them for logging, metrics, audit trails, approval flows, guardrails, request shaping, invalid-tool-call recovery, and streaming UI integration.

Official docs: https://rig.rs/docs/concepts/hooks (0.43 surface read from the crate sources: `rig-agent-0.43.0/src/agent/hook.rs` — the docs site may lag).

A hook implements the `AgentHook` trait — a set of **typed methods, one per boundary**, each returning an event-specific action. 0.43 reorganized the boundaries around the run's **effect bus**: model calls and tool dispatches are *effects*, and the tool-call/tool-result boundaries are `on_dispatch` (before) / `on_outcome` (after) instead of 0.42's `on_tool_call`/`on_tool_result`. Hooks live on the `AgentRunner` (driver) layer — the lower-level `AgentRun` state machine stays sans-IO. `AgentHook` is not generic over a model.

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

Agent-level hooks run first; per-run hooks are appended after the defaults.

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

Event payloads borrow their data, so hooks inspect without taking ownership. `OutcomeEvent` also carries `id` (the dispatch id), `kind` (`EffectKind`), `turn`, `call_id: Option<&CallId>` (the call the effect answered, for model-emitted tool calls) and `context: Option<&ToolContext>` (the dispatched tool context).

## Hook events and actions

Each `AgentHook` method is named `on_<event>` and returns an event-specific action. Override only the methods you care about; the defaults observe-and-continue.

| Method | Returns | Fires | Common uses |
|--------|---------|-------|-------------|
| `on_run_start` | `RunStartAction` | Once, before the first model call | rewrite/validate the prompt, refuse the run |
| `on_model_select` | `ModelSelectionAction` | Before each model call, after completion-call hooks proceed | route to another model per turn (sync, non-blocking) |
| `on_completion_call` | `CompletionCallAction` | Before each model request | logging, metrics, per-turn request patches, overflow stops |
| `on_model_turn_finished` | `ModelTurnAction` | At the end of a model turn (content parked, pre-commit) | accept or reject/retry the turn |
| `on_invalid_tool_call` | `Option<InvalidToolCallAction>` | Model called unknown/disallowed tool | fail, retry, repair, skip (see below) |
| `on_dispatch` | `DispatchAction` | An effect is about to be dispatched (completion, tool, …) | approvals, argument rewriting, deny/skip tool calls |
| `on_outcome` | `OutcomeAction` | An effect resolved (completion, tool result, …) | redact, truncate, normalize, replace results |
| `on_text_delta` / `on_reasoning_delta` / `on_tool_call_delta` | `ObservationAction` | Streaming only | live UI updates, content-policy cancellation, display partial args |
| `on_run_settled` | `()` | Run ended (success or error) | terminal audit; returns nothing |

Streaming-only delta events fire only on the streaming surface; the dispatch/outcome boundaries fire on every surface.

## Action enums

- **`RunStartAction`** — `Continue`, `Rewrite(prompt)`, `Stop(reason)`; constructors `continue_run()/rewrite(..)/stop(..)`.
- **`CompletionCallAction`** — `Continue`, `Patch(RequestPatch)` (shape this turn), `Stop(reason)`; `continue_run()/patch(..)/stop(..)`.
- **`ModelSelectionAction`** — `Continue`, `Select(ModelRef)`, `Stop(reason)`; `continue_run()/select(..)/stop(..)`.
- **`ModelTurnAction`** — `Continue`, `Retry(RetryRequest::Repeat | Feedback)` (`repeat()`/`retry_with_feedback(..)`), `Stop(reason)`; retry of tool-call turns is rejected (unanswered calls never enter history); retries consume the run's `max_turns` budget.
- **`DispatchAction`** — `Proceed`, `Patch(EffectKind)` (same family; tool-call patches must keep the target name, so they patch arguments), `Deny(ErrorReport)`; helpers `proceed()/patch(..)/deny(report)/skip(reason)` (tool call fails as "skipped" with `reason`) `/stop(reason)` (a `Cancelled` report — cancels the run), `rewrite_tool_args(kind, args)`.
- **`OutcomeAction`** — `Proceed`, `Replace(Result<Outcome, ErrorReport>)`; helpers `proceed()/replace(..)/stop(reason)` (replaces with a `Cancelled` error to end the run after observing), `rewrite_tool_output(&event, output)` / `rewrite_tool_result(&event, text)` (replace the model-visible output, keeping status and dispatch context).
- **`ObservationAction`** — `Continue`, `Stop(reason)` (delta events).
- **`InvalidToolCallAction`** — see "Invalid tool calls" below.

A returned action an event cannot honor terminates the run with a diagnostic — the runner is fail-closed, it never silently ignores an action.

## Request patches

`CompletionCallAction::Patch(RequestPatch)` shapes one turn without mutating the agent — phased agents: force a search on turn 1, lower temperature for a critical step, shrink the advertised tool list:

```rust
use rig_agent::agent::{
    AgentHook, CompletionCallAction, CompletionCallEvent, HookContext, RequestPatch
};
use rig_core::message::ToolChoice;

struct ForceSearchFirst;

impl AgentHook for ForceSearchFirst {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> CompletionCallAction {
        if event.turn == 1 {
            CompletionCallAction::patch(
                RequestPatch::new()
                    .active_tools(["search_web"])
                    .tool_choice(ToolChoice::Specific {
                        function_names: vec!["search_web".to_string()],
                    })
                    .temperature(0.0),
            )
        } else {
            CompletionCallAction::continue_run()
        }
    }
}
```

`CompletionCallEvent` is `{ prompt: &Message, history: &[Message], turn: usize }`. `RequestPatch` fields: `temperature`, `max_tokens`, `tool_choice`, `active_tools`, `additional_params`, `extra_context` (retrieved documents), `history` (replace the history for this turn only), plus `.preamble(..)`/`.context(doc)`. Patches are per-turn and non-sticky; in a stack they **accumulate and merge** in registration order (later fields/object merges win). If you narrow `active_tools`, make sure any `tool_choice` still names an advertised tool.

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

`DispatchEvent` carries `id`, `kind: &EffectKind`, `turn`, `call_id: Option<&CallId>`, `context: Option<&ToolContext>` with helpers `tool_name()/tool_args()/tool_context()/completion_request()`. A skip makes the model see a synthetic skipped result; `DispatchAction::stop(reason)` cancels the run. A deny's salvage rule: a hook-patched kind is preserved as the reported effect.

This is a guardrail, not a security boundary. Enforce real authorization inside the tool or downstream service as well.

## Invalid tool calls

An invalid tool call is an unknown/unadvertised/disallowed tool name or non-JSON arguments (`InvalidToolCallReason::{UnknownTool, MalformedArguments}`); the `InvalidToolCallContext` carries `tool_name`, `tool_call_id`, `args`, `available_tools`, `allowed_tools`, `tool_choice`, `chat_history`, `is_streaming`. Default: fail fast. A hook opts in to recovery by returning `Some(InvalidToolCallAction)`, `None` from every hook preserves fail-fast:

```rust
use rig_agent::agent::{AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext};

struct RepairDefaultApi;

impl AgentHook for RepairDefaultApi {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        match event.tool_name.as_str() {
            "default_api" => Some(InvalidToolCallAction::Repair {
                tool_name: "search_web".to_string(),
            }),
            _ => Some(InvalidToolCallAction::Retry {
                feedback: format!("Use one of: {:?}", event.available_tools),
            }),
        }
    }
}
```

- `InvalidToolCallAction::Fail` — default fail-fast.
- `Retry { feedback }` — append corrective feedback and re-ask; bound with `max_invalid_tool_call_retries(n)` on the runner.
- `Repair { tool_name }` — rewrite the tool name and revalidate against allowed tools.
- `Skip { reason }` — synthetic tool result without executing. If any invalid call in a turn is skipped, Rig suppresses the turn's other tool calls too (returns synthetic "not executed" results). Skip is rejected under `ToolChoice::None`.

## Hook composition

Hooks are stored in a `HookStack` and run in registration order. How results compose is event-dependent: model selections, `on_completion_call` patches, **and the dispatch/outcome boundaries chain** (each hook sees the previous hook's patched kind / replaced outcome), while model-turn steering, delta observations and invalid-call recovery use first-non-`Continue`-wins. A stop in any hook prevents the remaining hooks.

```rust
let response = agent
    .prompt("Process this request.")
    .add_hook(AuditLog)
    .add_hook(TransferPolicy { max_auto_transfer: 500 })
    .run()
    .await?;
```

## Streaming and `observes`

In 0.43 `observes(kind)` **gates the dispatch and outcome boundaries** (both `on_dispatch` and `on_outcome`): a hook whose `observes` answers `false` for `StepEventKind::ToolDispatch` is not called for tool dispatches/outcomes and cannot steer them. Memory, retrieval, embedding, rerank and custom dispatches default to `false`; all other kinds default to `true`. The delta methods (`on_text_delta` etc.) are separate methods, not gated.

```rust
use rig_agent::agent::{AgentHook, StepEventKind};

struct ToolOnlyHook;

impl AgentHook for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolDispatch)
    }
}
```

Even with `observes`, hook methods should still return the continue/proceed action for events they ignore — a sibling hook may cause an event to be dispatched.

## Run-scoped state: `HookContext`

`HookContext` is supplied to every hook: `run_id()`, `turn()` (one-based model-call index), `is_streaming()`, `agent_name()`, `scratchpad()` (a typed, run-scoped `Scratchpad` with `insert/get/update/remove` for concurrency-safe hook state), `bind(key)` for bus-bound typed views (`RunHandle`) and `append_entry(kind, &value)`/`entries(kind)` for entries that travel with the serializable run.

## Best practices

- Keep hooks lightweight — awaited inline; slow hooks delay the next model/tool step.
- Offload logging/network/audit to background tasks.
- Treat hook guardrails as UX/policy, not authorization boundaries.
- Make hook state concurrency-safe when `tool_concurrency > 1` (or use the `Scratchpad`).
- Prefer one composed policy hook when multiple rules must produce one decision.