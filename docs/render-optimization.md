# Render optimization

The TUI event loop (`src/tui.rs`) is a textbook Elm loop with no batching, no
frame-rate cap, and no event coalescing:

```
draw → select!(terminal | core) → ONE update → break → draw → ...
```

So **1 core event = 1 `update` = 1 full `rat.draw`**. During streaming the core
emits one `TokenReceived` (and `ReasoningReceived` chunk) per token, so a
200 token/s stream attempts ~200 redraws/s. Each redraw of the streaming
session, when `scroll_dirty` is set (it is, on every token), runs
`rebuild_scroll_view`, which clones all messages, re-parses markdown for the
**entire** conversation history via `shuvarie_highlight::render`, and renders
the whole paragraph into the scroll-view buffer. That makes streaming roughly
O(n²) in output size.

There are two independent bottlenecks:

- **(A) too many draws** — one draw per core event, unbounded.
- **(B) each draw does too much work** — the whole history is re-parsed every
  dirty frame.

## Phase 1 — Frame-rate limiting + event coalescing (done)

Change only `render_tui` in `src/tui.rs`. Bound redraws to a configurable frame
rate (default ~60 fps / 16 ms) and coalesce bursts of core events into a single
frame.

- After applying a **terminal** event → draw immediately (keeps input snappy).
- After applying a **core** event → drain all already-queued core events
  non-blockingly (`event_rx.try_recv()` loop), applying each, then:
  - if `last_draw.elapsed() >= budget` → draw now;
  - else arm a one-shot `tokio::time::Sleep` deadline and keep listening
    (coalescing more core events) until the deadline fires → then draw once.
- A third, conditional `select!` branch polls the armed timer.

This collapses a burst of `TokenReceived` into a single frame and bounds draws
to ≤ frame-rate/s regardless of token rate. `TokenReceived` /
`ReasoningReceived` updates are pure appends, so coalescing them is semantically
safe; `ToolFinished` / `StreamDone` inside a burst just apply in order.
Terminal input stays immediate.

The frame rate is configurable via `[ui]` in `config.toml`:

```toml
[ui]
frame_rate = 60   # frames per second; 0 disables the cap (draw every event)
```

Plumbed by passing the already-loaded `Config` from `main` into `run_tui`.

## Phase 2 — Cache the committed-prefix render (done)

Split `rebuild_scroll_view` so the **committed messages** (everything in
`self.messages` + their finalized tools / reasoning / context) are rendered
once and cached, and only the **streaming tail** (the in-flight `pending`
assistant message + its tools / context / `pending_reasoning`) is re-rendered
each dirty frame.

- Added `committed_lines: RefCell<Vec<Line<'static>>>` + `committed_dirty:
  Cell<bool>` to `SessionScreen`.
- The separation is clean in the data model: in-flight tools / context carry
  `message_index == self.messages.len()`, and pending reasoning is
  `pending_reasoning`.
- `committed_dirty` is set only on changes that affect history: `Submit` /
  `SendMessage`, `StreamDone`, `StreamError` / `StreamCancelled` (when they
  push), `Loaded` / `apply_session`, `Reset`, `TurnReverted` / `TurnRestored`.
  Tail-only events (`TokenReceived`, `ReasoningReceived`, `ContextLoaded`,
  `ToolStarted` / `ToolFinished`, `WorkerStarted` / `WorkerFinished`) leave the
  committed cache alone.
- Also eliminates the `self.messages.clone()` per frame.
- The per-message rendering was extracted into `push_message_lines`, shared by
  the committed pass and the tail pass.

Result: each streaming frame re-parses only the current growing message, not
the whole conversation.

## Phase 3 — Optional polish (not started)

- Skip `sidebar.view` work when its inputs are unchanged (dirty flag).
- Cache the scroll-view buffer for the committed portion too (avoid the
  O(total) buffer fill).
- Incremental markdown parsing for the streaming tail (unnecessary once draw
  rate is bounded).