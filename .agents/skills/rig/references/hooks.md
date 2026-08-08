# Hooks

Hooks let your code observe and steer the agent loop while `AgentRunner` drives the model and tool IO. Use them for logging, metrics, audit trails, approval flows, guardrails, request shaping, invalid-tool-call recovery, and streaming UI integration.

Official docs: https://rig.rs/docs/concepts/hooks · Source: https://github.com/0xPlaygrounds/rig/blob/main/crates/rig-core/src/agent/hook.rs

A hook implements one method: `AgentHook::on_event`. It receives a `StepEvent` and returns a `Flow`. Hooks live on the `AgentRunner` (driver) layer — the lower-level `AgentRun` state machine stays sans-IO and serializable, so it has no hooks.

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
use rig::agent::{AgentHook, Flow, StepEvent};
use rig::completion::CompletionModel;

struct ToolAudit;

impl<M: CompletionModel> AgentHook<M> for ToolAudit {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        match event {
            StepEvent::ToolCall { tool_name, args, .. } => {
                println!("calling {tool_name} with {args}");
            }
            StepEvent::ToolResult { tool_name, result, .. } => {
                println!("{tool_name} returned {result}");
            }
            _ => {}
        }
        Flow::cont()
    }
}
```

`StepEvent` borrows its payload, so hooks inspect without taking ownership.

## Hook events

| Event | When it fires | Common uses |
|-------|---------------|-------------|
| `CompletionCall { prompt, history, turn }` | Before each model request | logging, metrics, per-turn request overrides |
| `CompletionResponse { prompt, response }` | After a non-streaming model response | audit raw responses, count calls |
| `InvalidToolCall(ctx)` | Model called unknown/disallowed tool | fail, retry, repair, skip |
| `ToolCall { tool_name, args, .. }` | Before executing a valid tool | approvals, argument rewriting, deny/skip |
| `ToolResult { tool_name, result, .. }` | After a tool returns | redact, truncate, normalize, log |
| `TextDelta { delta, aggregated }` | Streaming only | live UI updates, content-policy cancellation |
| `ToolCallDelta { .. }` | Streaming only | display partial tool-call args |
| `StreamResponseFinish { prompt, response }` | Streaming text response finished | streaming-side metrics/cleanup |

`CompletionResponse` and `StreamResponseFinish` are suppressed for turns recovered by invalid-tool-call repair/skip/retry. Streaming-only events fire only on the streaming surface.

## Flow actions

`Flow::cont()` = observe only. Other actions steer the run. The runner is **fail-closed**: if a hook returns an action an event cannot honor, Rig terminates with a diagnostic instead of silently ignoring it.

| Flow | Valid events | Effect |
|------|--------------|--------|
| `cont()` | all | Continue normally |
| `terminate(reason)` | all | Stop with a cancellation error containing current history |
| `override_request(RequestOverride)` | `CompletionCall` | Patch this turn's request only |
| `rewrite_args(json)` | `ToolCall` | Execute the tool with replacement JSON args |
| `skip(reason)` | `ToolCall`, `InvalidToolCall` | Don't execute; return `reason` to model as tool result |
| `rewrite_result(result)` | `ToolResult` | Replace what the model sees as the tool output |
| `fail()` | `InvalidToolCall` | Preserve default fail-fast |
| `retry(feedback)` | `InvalidToolCall` | Append corrective feedback, ask model again |
| `repair(tool_name)` | `InvalidToolCall` | Rewrite the tool name, revalidate against allowed tools |

Returning `Flow::cont()` for `InvalidToolCall` is treated as `Flow::fail()`.

## Request overrides

`Flow::override_request` from `CompletionCall` patches one turn without mutating the agent — phased agents: force a search on turn 1, lower temperature for a critical step, shrink the advertised tool list:

```rust
use rig::agent::{AgentHook, Flow, RequestOverride, StepEvent};
use rig::completion::CompletionModel;
use rig::message::ToolChoice;

struct ForceSearchFirst;

impl<M: CompletionModel> AgentHook<M> for ForceSearchFirst {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        match event {
            StepEvent::CompletionCall { turn: 1, .. } => Flow::override_request(
                RequestOverride::new()
                    .active_tools(["search_web"])
                    .tool_choice(ToolChoice::Specific {
                        function_names: vec!["search_web".to_string()],
                    })
                    .temperature(0.0),
            ),
            _ => Flow::cont(),
        }
    }
}
```

Overrides are per-turn and non-sticky. `additional_params` are shallow-merged with the agent's; other fields replace the baseline for that turn. If you narrow `active_tools`, make sure any `tool_choice` still names an advertised tool.

## Guardrails and approvals

```rust
use rig::agent::{AgentHook, Flow, StepEvent};
use rig::completion::CompletionModel;

struct TransferPolicy { max_auto_transfer: u64 }

impl<M: CompletionModel> AgentHook<M> for TransferPolicy {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        let StepEvent::ToolCall { tool_name, args, .. } = event else {
            return Flow::cont();
        };
        if tool_name != "transfer_funds" {
            return Flow::cont();
        }
        let amount = serde_json::from_str::<serde_json::Value>(args)
            .ok()
            .and_then(|v| v.get("amount").and_then(|a| a.as_u64()));
        match amount {
            Some(n) if n <= self.max_auto_transfer => Flow::cont(),
            Some(n) => Flow::skip(format!(
                "denied by policy: ${n} exceeds ${} automatic limit",
                self.max_auto_transfer
            )),
            None => Flow::skip("denied by policy: missing amount"),
        }
    }
}
```

This is a guardrail, not a security boundary. Enforce real authorization inside the tool or downstream service as well.

## Invalid tool calls

An invalid tool call is an unknown/unadvertised/disallowed tool name. Default: fail fast. A hook can recover:

```rust
use rig::agent::{AgentHook, Flow, StepEvent};
use rig::completion::CompletionModel;

struct RepairDefaultApi;

impl<M: CompletionModel> AgentHook<M> for RepairDefaultApi {
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        match event {
            StepEvent::InvalidToolCall(ctx) if ctx.tool_name == "default_api" => {
                Flow::repair("search_web")
            }
            StepEvent::InvalidToolCall(ctx) => {
                Flow::retry(format!("Use one of: {:?}", ctx.available_tools))
            }
            _ => Flow::cont(),
        }
    }
}
```

- `fail()` — default fail-fast.
- `retry(feedback)` — append corrective feedback and re-ask; bound with `max_invalid_tool_call_retries(n)`.
- `repair(tool_name)` — rewrite the tool name and revalidate.
- `skip(reason)` — synthetic tool result without executing. If any invalid call in a turn is skipped, Rig suppresses the turn's other tool calls too (returns synthetic "not executed" results). Skip is rejected under `ToolChoice::None`.

## Hook composition

Hooks are stored in a `HookStack` and run in registration order. The first hook returning anything other than `Flow::cont()` wins for that event; later hooks aren't called for that event. If multiple policies must combine into one action, compose them inside a single hook.

```rust
let response = agent
    .runner("Process this request.")
    .add_hook(AuditLog)
    .add_hook(TransferPolicy { max_auto_transfer: 500 })
    .run()
    .await?;
```

## Streaming and `observes`

Text/tool-call deltas can be frequent. Override `observes` so Rig can skip work when no hook cares about a high-frequency event kind:

```rust
use rig::agent::{AgentHook, Flow, StepEvent, StepEventKind};
use rig::completion::CompletionModel;

struct ToolOnlyHook;

impl<M: CompletionModel> AgentHook<M> for ToolOnlyHook {
    fn observes(&self, kind: StepEventKind) -> bool {
        matches!(kind, StepEventKind::ToolCall | StepEventKind::ToolResult)
    }
    async fn on_event(&self, event: StepEvent<'_, M>) -> Flow {
        if let StepEvent::ToolCall { tool_name, .. } = event {
            println!("tool: {tool_name}");
        }
        Flow::cont()
    }
}
```

Even with `observes`, `on_event` should still return `Flow::cont()` for events it ignores — a sibling hook may cause an event to be dispatched.

## Best practices

- Keep hooks lightweight — awaited inline; slow hooks delay the next model/tool step.
- Offload logging/network/audit to background tasks.
- Treat hook guardrails as UX/policy, not authorization boundaries.
- Make hook state concurrency-safe when `tool_concurrency > 1`.
- Prefer one composed policy hook when multiple rules must produce one decision.