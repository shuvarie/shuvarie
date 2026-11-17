# Codemode

An LLM writes code better than it emits tool calls: it has seen millions of real
programs and only a contrived corpus of tool-call formats. So instead of
presenting the roster as N schemas and N round trips, codemode presents **one**
tool that runs a script, and the script calls the other tools. Intermediate
results never enter the context window, N dependent calls become one round trip,
and the model reads back only what it asked for.

Prior art, and what we take from each:

| | Mechanism | Taken |
| --- | --- | --- |
| Cloudflare | Typed TS API built from tool schemas; V8 isolate per script; `search` + `execute` for APIs too large to inline | One tool; schema→typed API; keep huge catalogs out of context |
| OpenCode | Own interpreter for a JS subset; `tools.orders.lookup({...})`; `catalog()` + `search()`; `{ok, value, logs, warnings, truncated, toolCalls}` | Namespaced call tree; structured result with a per-call trace |
| Pi | QuickJS sandbox; `tools.<name>()`, `text()`, `console`, `store`/`load`, `ALL_TOOLS`, `searchTools`, `describeTool`; ~3000-token inline budget; `declared`/`only` modes | The globals, the budget, and the modes (`declare-builtin` is ours: Pi's `declared` is all-or-nothing) |

We differ in one place: the interpreter is **Python**, embedded through `pyo3`
(`crates/codemode`, `shuvarie-codemode`). No JS engine, no subprocess, no
isolate — and the tool roster is the boundary that permissions actually govern
(below). Python was chosen over a purpose-built script language for one reason:
the model already knows it, so the dialect card only has to describe *this*
surface instead of a language.

The interpreter is not vendored. `pyo3` with `abi3-py310` links the version-less
stable-ABI `libpython`, so the binary is not pinned to the minor version of the
machine that built it, and the floor is 3.10. When no interpreter starts at all,
codemode is not offered rather than offered and broken (see *When there is no
interpreter*).

## Shape

- `crates/codemode/` (`shuvarie-codemode`): everything language-side.
  `python.rs` (the interpreter probe: whether there is one, which, and whether it
  is new enough), `spec.rs` (the catalog type and the name mangling),
  `render.rs` (JSON Schema → Python signature, and the budgeted description),
  `runtime.py` (the script's globals, its tools, and the runner — embedded into
  the binary), `script.rs` (the interpreter, the limits, the thread/channel
  bridge), `doc.rs` (the dialect card), `text.rs` (JSON as the text a result is
  rendered from). It knows nothing about shuvarie: the host side is a
  `ToolInvoker` trait, so the crate is unit-testable with stub tools.
- `crates/core/src/tools/codemode.rs`: the `codemode` tool. Its arguments are
  JSON (`{"script": "…", "options": {…}}`) rather than raw source as in Pi — one
  less escaping rule to get wrong on the way in.
- `crates/config`: a top-level `codemode { }` section, parsed like `decisions`.
- `src/tui/`: a `/codemode` overlay and transcript rendering.

## The call surface

A tool is a **function in a namespace object**: the catalog builds one
`SimpleNamespace` per namespace, so an MCP server becomes a real level instead of
a `mcp__server__tool` string:

```python
src = tools.read_file(path="src/main.rs")
hits = mcp.dev_radius.search(query="orders")
```

Arguments are keyword arguments named after the schema's properties. A property
whose name is not an identifier (`repo-name`) cannot be a keyword, so a callable
also takes a single dict: `tools.search(**{"repo-name": "x"})`, which is the only
spelling Python offers for that.

Python identifiers cannot contain `-`, so tool names are mangled for callability
(`resolve-library-id` → `resolve_library_id`), and a name that turns into a keyword
or a dunder is prefixed or suffixed so it stays reachable (`class` → `_class`,
`__init__` → `__init___`). The reserved list is checked against the interpreter's
own `keyword.kwlist`, so it cannot fall behind Python. The catalog always prints
the callable name, so the model never has to guess the mapping; a mangling
collision is reported in the catalog rather than silently resolved.

Globals: `text(value)`, `console.log/info/warn/error` and `print` (captured, not
streamed), the last expression as the script's value, `exit(value)`,
`store(key, value)` / `load(key)` for small JSON state across calls,
`search_tools(query, limit=, namespace=)`, `describe_tool(name)`,
`describe_namespace(name)`, `ALL_TOOLS`, `ToolError` (what a failing call raises),
and `models` (below).

## The catalog and its budget

`all_tools` already builds the turn's roster and `mcp_manager` already snapshots
the MCP roster per turn, so the catalog is rendered from that same snapshot:
name, description, and a Python signature derived from the tool's JSON Schema
(`read_file(path: str, offset: int | None = None) -> Any`), grouped by
namespace. Declarations are inlined up to `inline-budget` estimated tokens
(chars/4, Pi's heuristic, default 3000); whatever does not fit is still
callable and reachable through `search_tools` / `describe_tool`. The rendered
description is pinned by a golden-file test so its cost cannot drift unnoticed.

## The sandbox and the bridge

Python is embedded, but a script still cannot `await` a tool. The interpreter runs
on a **dedicated thread**; every tool is a native function that sends a request
over a channel and **blocks** on the reply while releasing the GIL
(`Python::detach`). On the other side a tokio task performs the real `Token::call`
and replies. The GIL going back while a call is in flight is not an optimisation:
the runtime serving that call, the timer that ends the run and the watchdog that
can interrupt it all need to run, and a script holding the GIL through a blocked
call would wedge the process.

The interrupt itself is Python's (`PyThreadState_SetAsyncExc`, raised by a
watchdog thread onto the script thread), because that is the one thing only Python
can do to itself without `unsafe` — and it raises a `BaseException`, so a script's
own `except Exception` cannot swallow it. The deadline lives on the Rust side as a
timer that flips the same stop signal the caller flips; `max-operations` is a
`sys.settrace` hook counting the script's own lines, installed only when it is
configured, because a trace hook costs a few times the speed of the code it
watches.

Each run loads `runtime.py` as a module of a **name of its own** and drops it from
`sys.modules` afterwards. The name is not cosmetic: `PyImport_ExecCodeModule`
returns whatever `sys.modules` already holds under a name, so a shared name would
let two runs share one module — and one host object, which is all of the per-run
state. `sys.stdout` is a process-wide proxy for the same reason: runs are not
sequential (two tabs can run a script at once), so it dispatches on the calling
thread instead of being swapped per run.

Consequences that make this worth the thread:

- Nested calls are **real tool calls**: they pass through `Access`
  (`authorize_tool` / `authorize_path` / `authorize_shell` / `confirm_scene`), so
  a mid-script permission prompt is the ordinary prompt, the scene's `ask` and
  `disable-all` still apply, and a denial is a catchable `ToolError`.
- Nested calls are **silent**. They emit no stream item, so a script's calls are
  not transcript blocks, sidebar rows or session tool records of their own — a
  turn has one block per script, not one per call. The script's own result
  carries the trace (`Tool calls (N)`: each call's name, arguments, duration and
  outcome), which the model, the live transcript and a replayed session all read
  from the same text. Suppressing at the source rather than in the TUI is what
  keeps the live turn and the reloaded one identical.
- The block **leads with the script as code**: highlighted (Python is in syntect's
  bundled set, which is one thing embedding Python bought) behind a line-number
  gutter, then the value, console output and trace below it. Collapsed keeps three
  script lines so the block still says what ran; expanding shows the whole script.
  Moving the script out of the block's title is what makes the highlighting
  possible at all — a title is plain text.

The trade this makes: a file a script edited shows in the trace by path, but its
**diff is not rendered** — diffs are drawn from a tool block's `FileChange`, and
a nested call has no block. The alternative (forwarding nested calls as blocks)
is what the trace exists to replace; rendering an aggregated diff inside the
`codemode` block is the follow-up if it is missed in practice.

Embedding a real language means the language is **not** the sandbox, and this is
the honest version of that: a script *can* `import os` and reach the filesystem,
the network and the clock, because the interpreter is a full one. What the roster
adds is the path permissions actually govern: a nested call is a real tool call,
so it is gated exactly as a direct one, and a script that calls `run_shell`
inherits `run_shell`'s trust. Contained execution needs an OS container — or the
subprocess-with-a-restricted-build mode listed under the known limitations.
“Capability-safe by construction” was true of Rhai and is not true here.

## Limits and the result

Limits: `max-operations` (the trace hook), `timeout-ms` (a timer that sets the stop
signal, plus the watchdog that turns that signal into an interrupt), `max-tool-calls`
(counted before the call, so the call that hit the limit is still in the trace), and
`max-output-chars`. Python's own runaway guards are deliberately unused:
`sys.setrecursionlimit` is process-wide, and a real memory cap is the OS's job. A
cancellation must always answer the reply channel, or the script thread blocks
forever — the bridge guarantees a reply by construction.

**A blocked script has to be stoppable, and that is a separate mechanism.**
`PyThreadState_SetAsyncExc` interrupts *bytecode*, so a thread parked in
`time.sleep` ignores both the budget and an interrupt — and a script that waits is
not exotic: a poll interval or a retry backoff is the first thing a model writes.
Such a script does not fail, it *hangs*: the tool call stays pending, so the turn
stays running and the next message is steered into a queue behind it. So the
runtime owns `time.sleep`, sleeping in `_POLL` slices and checking the stop signal
between them (a thread that is not running a script keeps the real one), and it owns
`input()`, which raises instead of reading — a script has nobody to answer it, and
`sys.stdin` is the terminal whose keys the interface is decoding.

The same reasoning is why `timeout-ms` has a default. The shipped default was *no*
budget, on the theory that a script is the user's own work; a script is arbitrary
code, and one that never returns does not end the turn at all. Two minutes is
generous for orchestration and short enough that a hang is a mistake rather than a
restart; `timeout-ms 0` states "no budget" explicitly.

The result is one block, in the shape the references converge on: a completed or
failed header with wall time, the script's value (`text()` items and the return
value in order), a `<console_output>` block, a per-call trace (name, arguments
preview, duration, status) for every nested call, and a truncation marker. Parse
and runtime errors carry the innermost line of the script itself: the runtime
walks the traceback for the last frame whose file is the script, so a message
points at the author's line rather than at the runner.

## When there is no interpreter

The tool is offered only when `python.rs` can start one. The probe runs once per
process (initialization cannot be undone) and answers with the version and prefix,
or with the sentence to show instead; the core asks before building the turn's
wiring, so no turn is spent discovering that every call fails the same way.
`/codemode` reports it as a read-only first row, and it wins over `disabled`: the
file cannot talk its way past a missing interpreter.

`python "<path>"` in the config points the embedded interpreter at an installation
(its prefix, or the `python3` inside it) through `PYTHONHOME`, which is the only
hook an embedded interpreter has and which must be set before it starts. `main`
does that on its own thread before the core is spawned — the one place that is
still single-threaded — which is why a change takes a restart rather than applying
to the next turn.

## Permissions

`codemode` is permission-gated like any other tool, and:

- it never orchestrates itself (no recursion);
- it never orchestrates the `question` or worker tools in v1;
- `codemode { tools { tool "run_shell" { disabled #true } } }` removes a tool
  from scripts without removing it from the model;
- it is absent from worker rosters (`read_tools` / `command_tools` /
  `edit_tools`) in v1.

## Configuration

```
codemode {
  disabled #true   // omitted = offered. `#true` turns the tool off, `#false`
                     //   states it on, and `"auto"` narrows it to the turns that
                     //   have a configured tool (a `tool` entry or an MCP server)
                     //   for it to orchestrate.
  mode "declare-builtin" // default. "declare-builtin": the builtins stay, and
                     //   stdio/MCP tools are reachable only from a script.
                     //   "declared": every tool stays in the roster, described
                     //   as scriptable. "only": tools are hidden from the model
                     //   and reachable only through a script.
  python "/opt/python/bin/python3" // omitted = the interpreter this build linked.
                     //   Given as a prefix or the binary inside it: a rescue for
                     //   an embedded interpreter that cannot find its stdlib.
                     //   Applied before the first script, so it needs a restart.
  inline-budget 3000 // estimated tokens of inlined declarations
  timeout-ms 120000  // wall clock for one script, its tool calls included.
                     //   Omitted = the default two minutes; `0` = no budget.
  max-operations 0
  max-tool-calls 0
  store #true

  // Optional: tools scripts may not orchestrate, though the model may still
  // call them directly. Reuses `SceneToolsConfig` verbatim, so `enable-all`,
  // `ask-all` and `ask` work here too.
  tools { tool "run_shell" { disabled #true } }
}
```

Scenes need nothing new: `tools { disable-all; tool "codemode" { disabled #false } }`,
and per-tool `disabled` / `ask` overrides, all work through `ToolScene`/`Access`
as they do today.

### The three modes

`mode` decides how much of the roster the model sees directly, and every mode
puts the same catalog in front of the script — the difference is only what the
model can call *without* writing one.

| mode | the model sees | a script reaches |
| --- | --- | --- |
| `declared` | everything, described as scriptable | everything |
| `declare-builtin` (default) | the builtins only | everything |
| `only` | `codemode` alone | everything |

`declare-builtin` is the one that pays for itself, and it is the default. The
builtins are few and stable — `read_file`, `edit_file`, `grep`, `run_shell` — and
a schema each is cheap; a connected MCP server or a pile of configured `tool`
subprocess entries is where the roster bloats into dozens of schemas the model
mostly does not call, and where a typed call in a script reads better than one
schema per tool. The split is drawn by *provenance*, not by size: configured
sources (MCP servers and `tool` entries) go script-only, tools written in Rust
stay declared.

That split is the default, and the tool is **offered by default** with it: an
absent `disabled` node means on, so codemode is there without the user asking,
and the configured sources it would otherwise flood the roster with are already
behind it. `disabled "auto"` remains the narrower opt-in — offered only on the
turns that have a configured source to hide — and reads the config rather than
the scene-pruned roster, so a scene cannot silently flip a setting the user
wrote down.

One rule holds across all three modes: **a tool a script may not run never
leaves the model's roster.** `codemode { tools { tool "x" { disabled #true } } }`
means no script can reach `x`, so hiding it as well would make it reachable by
neither route — gone rather than hidden. Whatever a mode hides, it hides only
what is left in the script's roster.

The catalog states the split rather than leaving it to be inferred: a tool the
model's own roster does not offer is printed with a `script only` mark (and,
when it fell past the inline budget, listed under `Script only (N):`). Inference
  would be poor here — the two name spaces differ (`mcp__dev__x` in the roster,
  `mcp.dev.x` in the catalog), and `only` mode empties the roster entirely, so
there is nothing to compare against.

## Telling the model about this surface

The language needs no teaching; the surface does. It is described in three
channels:

- **System-preamble note** — appended only on the turns that offer the tool
  (`enabled "auto"` narrowing included): a script is for complex, multi-step
  work and for the tools the catalog marks `script only`; a straightforward
  tool call goes direct. Under `mode "only"` the note is replaced by its
  stronger form — a script is the only route to the roster, so the work is one
  script per piece rather than a call per step. Without the split stated, the
  model writes scripts for everything; with `only` stated as if it were a
  split, the model hunts for tools it no longer has.
- **Dialect card** — always in the `codemode` description: the calling convention
  (keyword arguments, the dict form, which namespace holds what), the globals
  table, the ways this Python differs from the Python a model would otherwise
  assume (`ToolError`, `print` captured, no `input()`), and two worked examples.
- **Embedded skill `codemode`** — the full reference (the roster's shape, the
  value, discovery, error handling, and recipes: paginate, fan out, filter before
  returning) shipped in the binary and loaded on demand through the existing
  `skill` tool. This needs `Skills::with_builtins(...)`, since skills are
  directory-only.

The footguns belong in the card, because they are what actually costs a retry: a
tool call is a boundary with its own permissions, a failure raises a `ToolError`
rather than handing back `None`, and the capped `glob`/`grep` note is a line the
natural filter drops.

Every code block in both channels is **executed by a test** against a stub tool
set, so the docs cannot rot into snippets that no longer parse.

## `models`

`models.classify(...)` exposes the non-LLM decision models
(`shuvarie-decision`) inside scripts, as Pi does for its classifiers: pick a
configured decision definition, ask typed questions about JSON state, get typed
answers — useful for sorting or labelling data a script just fetched, without a
round trip. `DecisionClient::evaluate` is already `async`, so it rides the same
bridge as tool calls, and it stays inert unless the `decisions` feature is
enabled.

## Build order

0. **Spike.** Pin the Python semantics this design depends on: a module loaded by
   `PyImport_ExecCodeModule` being handed back by name, a native `#[pyclass]`
   method callable while the GIL is released inside it, `PyThreadState_SetAsyncExc`
   interrupting the right thread, `sys.settrace` line events firing only for the
   script's own file, and a last expression as the script's value. These live as
   tests in `crates/codemode/tests/script.rs`; the Rhai spike they replaced is
   gone.
1. **`crates/codemode`.** `python` / `spec` / `render` / `runtime.py` / `script` /
   `doc` / `text`, with the `ToolInvoker` trait. Unit tests: schema→signature
   rendering (optionals, enums, arrays, nesting), name mangling against the
   interpreter's keywords, limits, error paths.
2. **Core wiring.** The tool, the roster→catalog snapshot, the dispatcher
   (scene-gated `DynamicTool`, permissions, truncation), and the per-turn store.
   Integration tests with a stub roster.
3. **Config, modes, discovery.** `disabled` / `mode`, the budget, and the
   `search_tools` / `describe_tool` / `ALL_TOOLS` fallbacks.
4. **Docs as code.** The dialect card, the embedded skill, and the tests that
   execute every snippet.
5. **TUI.** `/codemode` overlay, a running-script indicator, and transcript
   rendering of the value, logs, and call trace.

Later: parallel calls (a `Task` handle type plus `wait_all`, so a script can fan
out), captured rather than streamed `run_shell`, and attachment/image output.

## Known limitations

- **Sequential.** A script cannot overlap two tool calls in v1, so it saves round
  trips and context but not latency; `Task` + `wait_all` is the follow-up and is
  where the larger savings are.
- **The signatures are not checked.** They are documentation for the model, not a
  checker: a wrong argument type fails at call time, as the tool's own schema
  rejecting the call rather than as a script that will not compile.
- **No OS sandbox.** The interpreter is a real one in this process, so a script
  can `import os` and reach the filesystem and the network on its own; permissions
  govern only the tool route. Containment needs a container, or a restricted
  `python` build plus an OS sandbox around the process. A script that calls
  `run_shell` inherits `run_shell`'s trust.
- **Prompt-cache churn.** The catalog is part of the description, so an MCP
  server connecting, or a tool toggling, invalidates the cache once.
- **`read_file`'s per-turn dedupe cache** exists to avoid re-sending content the
  model already has; a script's intermediate values never reach the model, so
  nested reads must opt out of it.
- **`store` is in-memory per session.** Durable keys would ride the session
  record (Pi appends a store entry per successful script).
- **Line-oriented tool output is text, not data.** `glob` caps at 100 paths and
  `grep` at 200, each trailing a sentence saying so. The note is itself a line,
  so the filter that makes the output usable (`.ends_with(".rs")`) discards
  exactly the warning: a truncated tally comes back as a clean `100` that reads
  like a real count. A live run hit this three times and reported an undercount
  for the crates whose paths fell past the cap. The walk is sorted now, so the
  truncated subset is a stable prefix rather than an arbitrary one — the answer
  is still incomplete, but it is the same incomplete answer twice — and the card
  teaches checking `contains("truncated")` before trusting a count, keeping one
  query to one directory. Merging directories into one pattern reaches the cap
  sooner: `crates/*/src/**` matches 140 files, so it reports 100 for a tree that
  ten per-crate queries count exactly. Until the tools report structured counts,
  aggregates over `.split("\n")` remain a checked guess.
- **Nothing is undone.** Calls made before a failure are real, as in the
  references.
