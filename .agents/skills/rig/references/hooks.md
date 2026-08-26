# Hooks

Hooks let your code observe and steer the agent loop while `AgentRunner` drives the model and tool IO. Use them for logging, metrics, audit trails, approval flows, guardrails, request shaping, invalid-tool-call recovery, and streaming UI integration.

Official docs: https://rig.rs/docs/concepts/hooks

A hook implements the `AgentHook` trait — a set of **typed methods, one per event**, each returning an event-specific action. (0.42 replaced the single `on_event(StepEvent) -> Flow` method of 0.41 with dedicated methods like `on_completion_call` → `CompletionCallAction` and `on_tool_call` → `ToolCallAction`.) Hooks live on the `AgentRunner` (driver) layer — the lower-level `AgentRun` state machine stays sans-IO and serializable, so it has no hooks. `AgentHook` is not generic over a model.

## Add hooks to a request, runner, or agent

```rust
// One request:
let answer = agent
    .prompt("Check the balance, then summarize it.")
    .max_turns(3)
    .add_hook(ToolAudit)
    .await?;

// One explicit runner:
let response = agent
    .runner("Check the balance, then summarize it.")
    .max_turns(3)
    .add_hook(ToolAudit)
    .run()
    .await?;

// Every request from an agent (default hooks):
let agent = openai::Client::from_env()?
    .agent("gpt-5.5")
    .add_hook(ToolAudit)
    .build();
```

Agent-level hooks run first; per-request/per-run hooks are appended after the defaults.

## A minimal hook

```rust
use rig::agent::{AgentHook, ToolCallAction, ToolCall, ToolResultAction, ToolResultEvent};
use rig::completion::CompletionModel;

struct ToolAudit;

impl AgentHook for ToolAudit {
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        println!("calling {} with {}", event.tool_name, event.args);
        ToolCallAction::run()
    }

    async fn on_tool_result(&self, _ctx: &HookContext, event: ToolResultEvent<'_>) -> ToolResultAction {
        println!("{} returned {}", event.tool_name, event.presentation.render());
        ToolResultAction::keep()
    }
}
```

Event payloads borrow their data, so hooks inspect without taking ownership.

## Hook events and actions

Each `AgentHook` method is named `on_<event>` and returns an event-specific action. Override only the methods you care about; the defaults observe-and-continue.

| Method | Returns | Fires | Common uses |
|--------|---------|-------|-------------|
| `on_model_select` | `ModelSelectionAction` | Before each model call, after selection | route to another model per turn |
| `on_completion_call` | `CompletionCallAction` | Before each model request | logging, metrics, per-turn request patches |
| `on_completion_response` | `ObservationAction` | After a non-streaming model response | audit raw responses, count calls |
| `on_model_turn_finished` | `ModelTurnAction` | At the end of a model turn | accept or reject/retry the turn |
| `on_invalid_tool_call` | `Option<InvalidToolCallAction>` | Model called unknown/disallowed tool | fail, retry, repair, skip (see below) |
| `on_tool_call` | `ToolCallAction` | Before executing a valid tool | approvals, argument rewriting, deny/skip |
| `on_tool_result` | `ToolResultAction` | After a tool returns | redact, truncate, normalize, log |
| `on_text_delta` | `ObservationAction` | Streaming only | live UI updates, content-policy cancellation |
| `on_reasoning_delta` | `ObservationAction` | Streaming only | show thinking progress |
| `on_tool_call_delta` | `ObservationAction` | Streaming only | display partial tool-call args |
| `on_stream_response_finish` | `ObservationAction` | Streaming text response finished | streaming-side metrics/cleanup |

`on_completion_response`/`on_stream_response_finish` are suppressed for turns recovered by invalid-tool-call repair/skip/retry. Streaming-only events fire only on the streaming surface.

## Action enums

- **`CompletionCallAction`** — `Continue`, `Patch(RequestPatch)` (shape this turn), `Stop(reason)`.
- **`ToolCallAction`** — `Run`, `Rewrite(args)`, `Skip(reason)`, `Stop(reason)`.
- **`ToolResultAction`** — `Keep`, `Rewrite(ToolOutput)` / `rewrite(result)`, `Stop(reason)`.
- **`ObservationAction`** — `Continue`, `Stop(reason)` (observed completion/turn-finish/text-delta/…).
- **`ModelSelectionAction`** — `Continue`, `Select(model)`, `Stop(reason)`.
- **`ModelTurnAction`** — `Continue`, `Retry(feedback)`, `Stop(reason)`.
- **`InvalidToolCallAction`** — see "Invalid tool calls" below.

Each action enum has ergonomic constructors: `CompletionCallAction::continue_run()/patch(...)/stop(...)`, `ToolCallAction::run()/rewrite(...)/skip(...)/stop(...)`, `ToolResultAction::keep()/rewrite(...)/stop(...)`. A returned action an event cannot honor terminates the run with a diagnostic — the runner is fail-closed, it never silently ignores an action.

## Request patches

`CompletionCallAction::Patch(RequestPatch)` shapes one turn without mutating the agent — phased agents: force a search on turn 1, lower temperature for a critical step, shrink the advertised tool list:

```rust
use rig::agent::{AgentHook, CompletionCallAction, CompletionCall, HookContext, RequestPatch};
use rig::completion::CompletionModel;
use rig::message::ToolChoice;

struct ForceSearchFirst;

impl AgentHook for ForceSearchFirst {
    async fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCall<'_>,
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

Patches are per-turn and non-sticky. `additional_params` are shallow-merged with the agent's; other fields replace the baseline for that turn. If you narrow `active_tools`, make sure any `tool_choice` still names an advertised tool.

## Guardrails and approvals

```rust
use rig::agent::{AgentHook, HookContext, ToolCall, ToolCallAction};
use rig::completion::CompletionModel;

struct TransferPolicy { max_auto_transfer: u64 }

impl AgentHook for TransferPolicy {
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        if event.tool_name != "transfer_funds" {
            return ToolCallAction::run();
        }
        let amount = serde_json::from_str::<serde_json::Value>(event.args)
            .ok()
            .and_then(|v| v.get("amount").and_then(|a| a.as_u64()));
        match amount {
            Some(n) if n <= self.max_auto_transfer => ToolCallAction::run(),
            Some(n) => ToolCallAction::skip(format!(
                "denied by policy: ${n} exceeds ${} automatic limit",
                self.max_auto_transfer
            )),
            None => ToolCallAction::skip("denied by policy: missing amount"),
        }
    }
}
```

This is a guardrail, not a security boundary. Enforce real authorization inside the tool or downstream service as well.

## Invalid tool calls

An invalid tool call is an unknown/unadvertised/disallowed tool name. Default: fail fast. A hook opts in to recovery by returning an `InvalidToolCallAction` (returning `None` from every hook preserves fail-fast):

```rust
use rig::agent::{AgentHook, HookContext, InvalidToolCallAction, InvalidToolCallContext};
use rig::completion::CompletionModel;

struct RepairDefaultApi;

impl AgentHook for RepairDefaultApi {
    async fn on_invalid_tool_call(
        &self,
        _ctx: &HookContext,
        event: &InvalidToolCallContext,
    ) -> Option<InvalidToolCallAction> {
        match event.tool_name.as_str() {
            "default_api" => Some(InvalidToolCallAction::repair("search_web")),
            _ => Some(InvalidToolCallAction::retry(format!(
                "Use one of: {:?}",
                event.available_tools
            ))),
        }
    }
}
```

- `InvalidToolCallAction::Fail` — default fail-fast.
- `Retry(feedback)` — append corrective feedback and re-ask; bound with `max_invalid_tool_call_retries(n)`.
- `Repair(tool_name)` — rewrite the tool name and revalidate against allowed tools.
- `Skip(reason)` — synthetic tool result without executing. If any invalid call in a turn is skipped, Rig suppresses the turn's other tool calls too (returns synthetic "not executed" results). Skip is rejected under `ToolChoice::None`.

## Hook composition

Hooks are stored in a `HookStack` and run in registration order. How results compose is event-dependent: model selections and `ToolCall`/`ToolResult` rewrites **chain** (each hook sees the previous hook's output), completion-call patches accumulate and merge, while model-turn steering and observe-only/recovery events use first-non-`Continue`-wins.

```rust
let response = agent
    .runner("Process this request.")
    .add_hook(AuditLog)
    .add_hook(TransferPolicy { max_auto_transfer: 500 })
    .run()
    .await?;
```

## Streaming and `observes`

Text/reasoning/tool-call deltas can be frequent. Override `observes` so Rig can skip work when no hook cares about a high-frequency event kind:

```rust
use rig::agent::{AgentHook, HookContext, StepEventKind, ToolCall, ToolCallAction};
use rig::completion::CompletionModel;

struct ToolOnlyHook;

impl AgentHook for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolCall | StepEventKind::ToolResult)
    }
    async fn on_tool_call(&self, _ctx: &HookContext, event: ToolCall<'_>) -> ToolCallAction {
        println!("tool: {}", event.tool_name);
        ToolCallAction::run()
    }
}
```

Even with `observes`, hook methods should still return the continue action for events they ignore — a sibling hook may cause an event to be dispatched.

## Best practices

- Keep hooks lightweight — awaited inline; slow hooks delay the next model/tool step.
- Offload logging/network/audit to background tasks.
- Treat hook guardrails as UX/policy, not authorization boundaries.
- Make hook state concurrency-safe when `tool_concurrency > 1`.
- Prefer one composed policy hook when multiple rules must produce one decision.