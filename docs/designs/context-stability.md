# Context stability across interruption, resume, and retry

A turn is only as good as the conversation the model is shown when it is
re-issued. Today that conversation is **reconstructed** from storage after every
cut: `Session::from_stored` rebuilds the active path, `replay_tool_activity`
splices tool calls back in, and `resume_request` decides what the resumed
request's last message is. The reconstruction is good, but it is a *heuristic*
over state that the streaming task was mutating when it died, and Rig's own run
state — the authoritative conversation while a run is live — is thrown away on
every interruption. This document audits where the two diverge and proposes an
adapter layer that makes the conversation an explicit, checkpointed object
instead.

## What "stable" means

Four invariants, in priority order:

1. **No lost work.** What the model produced before a cut is persisted before
   the process can exit or the task can be aborted.
2. **No duplicated work.** A resumed attempt continues the conversation the
   attempt actually left, not a replay of the turn's prompt; the model is not
   asked to redo calls it already answered.
3. **A well-formed transcript.** Every assistant tool call is answered by a
   result (including "the call was cut off"), every row is either a reply or an
   explicit interruption, and the request the model sees is the same
   conversation the store holds.
4. **A correct anchor.** Usage, compaction, and scene state restored after a
   resume describe the conversation that is actually being sent.

## Today's pipeline

```
core turn ──► ProviderClient::stream ──► AgentBuilder + hooks ──► .stream()
   ▲                                                                    │
   │                                                                    ▼
   └──── StreamItem ◄──── map_agent_stream ◄──── rig MultiTurnStreamItem
                                                                    │
                        stream_stream_to_events ────────────────────┘
                        (TurnState: text runs, reasoning, calls, pending)
                                   │
                                   ▼
                        persist at Done / Error / Overflow / cut
```

- `crates/llm/src/provider.rs` builds the request (`stream`, `stream_via`,
  `agent_with_tools`), installs `ContextHook` + `FileChangeHook`, and maps Rig's
  items (`map_agent_stream`).
- `crates/core/src/core_task.rs` consumes `StreamItem`s in
  `stream_stream_to_events`, keeping a `TurnState` as the de-facto checkpoint and
  persisting at four points: `Done` (commit), `Error`
  (`persist_stream_error`), `Overflow` (`persist_stream_error` + compaction),
  and a cut (`persist_interrupted_turn`).
- `crates/core/src/session.rs` reconstructs the request from the stored tree:
  `last_turn_resume`, `resume_continuation`, `replay_tool_activity`.
- `crates/core/src/core_task.rs::resume_last_turn` reloads from the store and
  re-sends.

The reconstruction is the stability mechanism. The gaps below are where the
state the task was holding is not fully represented in the store, or where the
store is mutated in a non-atomic sequence.

## Where it breaks

Severity: **S1** = can corrupt or lose the conversation; **S2** = transcript is
inconsistent but recoverable; **S3** = cosmetic or deliberate trade-off.

| # | Failure | Evidence | Severity |
| --- | --- | --- | --- |
| 1 | **Quit mid-turn loses the partial.** `cmd_rx` closing breaks the run loop without cutting the stream; the TUI drops `cmd_tx` and exits. The spawned task is dropped, no `persist_interrupted_turn` runs. | `core_task.rs:520-525`, `src/main.rs:124-126` | S1 |
| 2 | **Cancel can abort the commit.** `Done` sets `done`, the loop keeps draining stragglers, then the commit (`store.update_message` / `append_assistant_message`) runs inside the abortable task. `CancelStream` in that window calls `abort()` and the finished reply is discarded. | `core_task.rs:4409-4540`, `cut_running_stream` `3588-3619` | S1 |
| 3 | **Cancel can abort compaction mid-splice.** The overflow branch appends the summary, reparents the first tail message, then points the leaf back — three awaits inside the abortable task. An abort between them leaves the tail detached from the active path (recent messages vanish from every later request). | `core_task.rs:4351-4374`, `compact_active_path` `2573-2587` | S1 |
| 4 | **Retryable failure used to drop in-flight tool calls.** The persisted half is fixed (`persist_stream_error` now writes a killed record, like `persist_interrupted_turn`), so the reloaded transcript and the retried model keep the call. The live TUI still drops the block on `AttemptCut` while a reload shows it — a view divergence, not a context one. | `persist_stream_error` vs `persist_interrupted_turn` `4712-4867`; TUI intent in `chat.rs:5438-5482` | S3 |
| 5 | **Overflow prefill is persisted as a second row.** `resume_request` sets `continuation` only for `TurnRetry`; `Overflow` still re-sends the partial reply as an assistant prefill (`Continue` → `prior.pop()`), but `resume_last_turn` passes no continuation, so the attempt appends a fresh assistant row. The session ends with two consecutive assistant messages and a sentence split across them. | `resume_request` `2662-2704`, `resume_last_turn` `3436-3474`, commit `4432-4441` | S2 |
| 6 | **Abort-based cancel races persistence.** `cut_running_stream` calls `abort()` and immediately reads `TurnState`; the aborted task may still be between awaits mutating the store and the state. The steer self-cut has a `FINALIZING` guard; the user-cancel path has none. | `steer.rs:19-56`, `cut_running_stream` `3607-3616` | S2 |
| 7 | **Interrupted worker calls are not recorded.** `WorkerStart` never enters `TurnState.pending_tools`, so a cancel mid-worker writes no killed record; the live TUI shows it killed, a reload does not. | `core_task.rs:4140-4168`, `killed_records` `4666` | S2 |
| 8 | **Hard crash loses the un-checkpointed tail.** Text and reasoning are written only at `Done`/`Error`/cut; a tool-start creates an empty row. SIGKILL or power loss loses every delta since the last of those. | `ensure_assistant_row` `4599-4640`, delta arms `3886-3948` | S2 |
| 9 | **Resume replaces the in-memory session wholesale.** `resume_last_turn` does `*s.lock() = loaded`, discarding live `last_usage`, `announced_scene` changes, and any state written after the load. Mostly benign because the DB should be ahead, but it is nondeterministic when it is not. | `resume_last_turn` `3458` | S3 |
| 10 | **A "progressed" failure resets the retry chain.** An attempt that streams one token then dies starts a fresh budget every time; a provider that dies just after first byte retries forever. | `RetryableFailure` `1784-1824`, `shows_progress` `198-212` | S3 |

Items 1–3 are pure safety and can be fixed without changing semantics. Items 4–8
are where the *adapter* earns its keep.

**Status:** 1, 2 and 3 are fixed in the current tree (shutdown cuts and persists;
`TurnState::finishing` makes the commit and the compaction splice
non-abortable), and the persisted half of 4 is fixed (`persist_stream_error` now
writes a started-but-unanswered call as a killed record, matching
`persist_interrupted_turn` and the retry fixture). The live TUI still drops the
cut block on `AttemptCut` while a reload shows it; that divergence and items 5–8
remain. Phase 1's control surface has landed (`crates/llm/src/flow.rs`:
`FlowControl`, `LedgerHook`, `ControlledStream`, `StreamItem::Cancelled`, and a
cooperative cancel with a hard-cut grace); the core still persists from
`TurnState`, and `ResumePlan` is Phase 2.

## The adapter: an explicit conversation ledger

The root problem is that the conversation exists in three places — Rig's run,
`TurnState`, and the stored tree — and only the last survives a cut. The adapter
makes the second the authoritative, incrementally-written copy of the first, so
the store is always a projection of a ledger, not a replay heuristic.

### Shape

New module `crates/llm/src/flow.rs` (`shuvarie-llm`), with the core consuming it
through a narrow handle instead of poking at `AgentBuilder`:

```rust
// Landed: ProviderClient::stream_controlled(..) -> ControlledStream.
pub struct ControlledStream {
    pub stream: StreamStream,
    pub control: FlowControl,
}

pub struct FlowControl { /* Arc<FlowInner> */ }
impl FlowControl {
    pub fn cancel(&self);                       // cooperative
    pub fn phase(&self) -> FlowPhase;           // Running | Finishing | Settled
    pub fn mark_finishing(&self);
    pub fn snapshot(&self) -> Ledger;           // requests, calls, settled
}

pub struct Ledger {
    pub requests: Vec<RequestSnapshot>, // prompt + history per completion call
    pub calls: Vec<RecordedCall>,       // opened / answered / error
    pub settled: Option<Vec<Arc<Message>>>, // committed messages, on error too
}

// Phase 2 target: group the request parameters and own resume explicitly.
pub struct FlowSpec {
    pub model: String,
    pub preamble: Option<String>,
    pub prompt: ChatMsg,
    pub history: Vec<ChatMsg>,
    pub blobs: Blobs,
    pub tools: Vec<DynamicTool>,
    pub workers: Vec<WorkerAgent>,
    pub max_turns: usize,
    pub tool_concurrency: usize,
    pub context_budget: Option<ContextBudget>,
    pub seed_usage: Option<TokenUsage>,
}

```

`ProviderClient::stream_controlled(..) -> ControlledStream` builds the agent,
installs `LedgerHook` (beside the existing `ContextHook` / `FileChangeHook`), and
returns the mapped stream plus the control handle. `run_flow(spec)` is the Phase
2 form that replaces the long parameter list.

### The hook

`LedgerHook` is a `rig_agent::agent::AgentHook` with four jobs:

1. **Snapshot every request.** `on_completion_call` receives `prompt` and
   `history`; record them with the one-based turn index. This is the exact
   conversation the provider saw, which is the only ground truth for a transport
   failure that lost Rig's typed error. Caveat: `HookStack::on_completion_call`
   shows every hook the **unpatched** event and merges their `RequestPatch`es
   afterwards, so a ledger hook registered beside `ContextHook` would snapshot a
   history the provider never saw (the untrimmed one). Either fold the mechanical
   trim into the ledger hook, or record the patch with the snapshot and apply it
   when deriving the sent request.
2. **Record the committed conversation.** `on_run_settled` carries
   `messages: Option<&[Arc<Message>]>` — the messages the run committed, on
   failure as well as on success — so the ledger gets the canonical transcript
   without reconstructing it. `on_model_turn_finished` (canonical `content` +
   `usage` per accepted turn) is the Phase 2 refinement for the usage anchor.
3. **Pair tool calls.** `on_dispatch` (ToolDispatch) opens a call; `on_outcome`
   closes it with the result. `repair` semantics are then available without
   reparsing the core's `tool_records`.
4. **Honor cancellation at boundaries.** Check `FlowControl::cancel` in
   `on_completion_call`, `on_dispatch`, and the delta hooks, and return
   `CompletionCallAction::Stop`, `DispatchAction::Deny(ErrorKind::Cancelled)`, or
   `ObservationAction::Stop`. Rig then ends the run with
   `PromptError::Cancelled { chat_history, reason }` where `chat_history` is
   **already repaired** (`rig_core::conversation::pairing::repair`), i.e. every
   open tool call closed with a made-up result. That is precisely the transcript
   invariant 3 asks for, produced by the runtime that owns it.

Two consequences worth stating:

- `map_agent_stream` must **not** label `PromptError::Cancelled` as a retryable
  turn error. Add a `StreamItem::Cancelled { history }` (or a
  `StreamItem::Checkpoint` carrying the ledger) and route it to
  `persist_interrupted_turn`-equivalent handling instead of `RetryableFailure`.
- A cooperative cancel replaces `AbortHandle::abort()` for the user-cancel path.
  The task no longer has to be killed between awaits; it unwinds through Rig and
  reports a terminal snapshot. `abort()` remains only as a last-resort killer for
  a wedged tool.

### `FlowPhase` closes the commit/compaction windows

Map the current `SteerSignal::FINALIZING` idea onto the whole turn:

```
Running ──(Done seen, or compaction started)──► Finishing ──► Settled
```

The landed realization is `TurnState::finishing`, which `cut_running_stream`
consults before `abort()`: `Finishing` means "the task is inside a DB sequence;
do not abort it", exactly as it already treats `steer.is_finalizing()`. It is
set before `persist_stream_error`, before `compact_active_path`, and before the
commit's first database await; `FlowControl::phase()` mirrors the same idea for
callers. This turns failure modes 2 and 3 into "the cancel is ignored for a few
milliseconds and the turn commits", which is the correct outcome.

### The ledger makes resume an algebra, not a heuristic

Replace `last_turn_resume` + `resume_continuation` + `resume_request` with one
derivation over the canonical entries:

```rust
pub enum ResumePlan {
    /// The attempt produced no entry: replay the turn's user entry.
    Replay { prompt: ChatMsg, history: Vec<ChatMsg> },
    /// Continue the last assistant entry in place.
    Continue { prompt: ChatMsg, history: Vec<ChatMsg>, row: RowRef },
    /// Answer the last tool round (the attempt died after the batch settled).
    ToolRound { results: Vec<ChatToolResult>, history: Vec<ChatMsg>, row: RowRef },
    /// A cancelled run: every call is already answered by `repair`, so the
    /// next request is the repaired history plus the user's next prompt.
    Repair { history: Vec<ChatMsg> },
}
```

Every case is explicit and every case names the row to append to. `TurnRetry`
and `Overflow` differ only in whether the pane reloads, **not** in whether the
row is continued — which fixes failure mode 5. The old heuristic ("the path's
last message is the prompt") disappears; a trailing assistant prefill is a
property of `Continue`, not an accident of message ordering.

### Durability: turn checkpoints

`TurnState` is already the right shape; it is just in memory. Add a
`turn_checkpoints` row (session id, attempt, revision, serialized `TurnState`)
written at low frequency: after every tool result, after the first text run, and
on a debounce (e.g. every 500 ms or 64 deltas). On startup with `-c`, a
checkpoint newer than the last stored message means:

- the turn was cut by process death, not by the user;
- its `interrupted` marker should be set on load;
- its ledger can seed a `ResumePlan` instead of the lossy replay.

This closes failure mode 8 without per-delta database writes.

### Idempotent tool effects (the harder, later win)

A retried turn that already ran `apply_patch` or `run_shell` must not run it
again blindly. The ledger gives stable call ids; add a `tool_effects` journal
keyed by `(session, turn, call id)` holding the recorded result. A retried call
with the same id is answered from the journal (`OutcomeAction::rewrite_tool_result`)
instead of dispatched. This is the only way to satisfy "no duplicated work" for
side effects, and it depends on the ledger existing first.

## Rollout

| Phase | Change | Closes |
| --- | --- | --- |
| 0 | **Done.** Shutdown: cut + persist when `cmd_rx` closes. `TurnState::finishing` guard around the reply commit, the error/overflow persist and compaction. | 1, 2, 3 |
| 1 | **Control surface done.** `shuvarie-llm::flow`: `FlowControl`/`LedgerHook`/`ControlledStream` and `StreamItem::Cancelled`; the run loop cancels cooperatively and falls back to a hard cut after a grace. Still to do: `FlowSpec` (the request object) and persisting from the ledger's settled messages instead of `TurnState`. | 6, 4 |
| 2 | `ResumePlan` replaces the `TurnState` reconstruction path; overflow continues the row; worker calls join `pending_tools`. | 5, 7 |
| 3 | `turn_checkpoints` + startup recovery. | 8 |
| 4 | `tool_effects` journal and journaled retries. | 4 (side effects) |

Phase 0 is independent of the adapter and should land first: it is small, it
turns three S1 windows into impossible states, and it gives Phase 1 a clean
`Finishing` vocabulary to extend.

## Open questions

- **Does a retry show a killed tool block, or drop it?** The TUI currently drops
  it (`chat.rs:5438-5482`); the model needs to know a side-effecting call may
  have run. Recommendation: keep the TUI's drop for the *live* attempt, but
  persist the call as killed so a reload and the retried model both see it.
- **Should a user cancel during a retry wait resurrect the error?** Today
  `CancelStream` while `pending_retry` re-emits the original `StreamError`
  (`core_task.rs:946-952`). With a ledger, the cancelled retry is a normal
  interrupted turn and need not surface an error.
- **Does the ledger replace `Session.messages`, or project onto it?** Start by
  projecting (one-way ledger → `Session` and the store) so the TUI and the tree
  popup keep working; replace `Session` only if the projection proves lossless.
