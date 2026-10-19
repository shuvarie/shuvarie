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

We differ in one place: the interpreter is **Rhai**, embedded directly
(`crates/codemode`, `shuvarie-codemode`). No JS engine, no subprocess, no
isolate — and Rhai's capability model is the sandbox (below).

## Shape

- `crates/codemode/` (`shuvarie-codemode`): everything language-side.
  `spec.rs` (the catalog type), `render.rs` (JSON Schema → Rhai signature, and
  the budgeted description), `convert.rs` (JSON ↔ `Dynamic`), `script.rs` (the
  engine, the limits, the thread/channel bridge), `doc.rs` (the Rhai dialect
  card). It knows nothing about shuvarie: the host side is a `ToolInvoker`
  trait, so the crate is unit-testable with stub tools.
- `crates/core/src/tools/codemode.rs`: the `codemode` tool. Its arguments are
  JSON (`{"script": "…", "options": {…}}`) rather than raw source as in Pi — one
  less escaping rule to get wrong on the way in.
- `crates/config`: a top-level `codemode { }` section, parsed like `decisions`.
- `src/tui/`: a `/codemode` overlay and transcript rendering.

## The call surface

Rhai *static modules* (`Engine::register_static_module`) are resolved
namespace-qualified without an `import`, and support sub-modules — so MCP servers
become real namespaces instead of a `mcp__server__tool` string:

```rhai
let src = tools::read_file(#{ path: "src/main.rs" });
let hits = mcp::dev_radius::search(#{ query: "orders" });
```

Rhai identifiers cannot contain `-`, so tool names are mangled for callability
(`resolve-library-id` → `resolve_library_id`). The catalog always prints the
callable name, so the model never has to guess the mapping; a mangling collision
is reported in the catalog rather than silently resolved.

Globals: `text(value)`, `console.log/info/warn/error/debug` (captured, not
streamed), `return` (top-level `return` is the script's result), `exit()`,
`store(key, value)` / `load(key)` for small JSON state across calls,
`search_tools(query, #{ limit, namespace })`, `describe_tool(name)`,
`describe_namespace(name)`, `ALL_TOOLS`, and `models` (below).

## The catalog and its budget

`all_tools` already builds the turn's roster and `mcp_manager` already snapshots
the MCP roster per turn, so the catalog is rendered from that same snapshot:
name, description, and a Rhai signature derived from the tool's JSON Schema
(`read_file(path: String, offset?: int, limit?: int) -> Dynamic`), grouped by
namespace. Declarations are inlined up to `inline-budget` estimated tokens
(chars/4, Pi's heuristic, default 3000); whatever does not fit is still
callable and reachable through `search_tools` / `describe_tool`. The rendered
description is pinned by a golden-file test so its cost cannot drift unnoticed.

## The sandbox and the bridge

Rhai has no `async`, so the pattern is the one Rhai's own book documents for
async host APIs. The `Engine` runs on a **dedicated thread**; every callable tool
is a registered Rust closure that sends a request over a channel and blocks on
the reply. On the other side a tokio task performs the real `Tool::call` and
replies. The script thread builds the engine, compiles, and evaluates, so the
engine never crosses a thread boundary and Rhai's `sync` feature is not needed.

Consequences that make this worth the thread:

- Nested calls are **real tool calls**: they pass through `Access`
  (`authorize_tool` / `authorize_path` / `authorize_shell` / `confirm_scene`), so
  a mid-script permission prompt is the ordinary prompt, the scene's `ask` and
  `disable-all` still apply, and a denial is a catchable Rhai error.
- Nested results must reach the transcript, the sidebar, the DB tool-call
  records, LSP notifications, and file-change summaries exactly as a direct call
  does. `FileChangeHook` is already a generic `tokio::sync::mpsc::Sender<StreamItem>`
  sink, so either it is driven from here or a shared `ToolInvoker` is extracted
  for both paths — the extraction is preferred, and it is the sharpest
  integration point in this feature.

Rhai is a **capability** sandbox, not an OS sandbox: a script cannot touch the
filesystem, network, clock, or processes, and `import` cannot resolve anything
(no module resolver is registered) — but it can still call `run_shell` if the
roster allows `run_shell`. The permission layer, not the language, is the
boundary, which is the same posture `run_shell` already has.

## Limits and the result

Engine limits are set at construction: `max_operations`, `max_call_levels`,
`max_expr_depths`, `max_string_size`, `max_array_size`, `max_map_size`, plus a
deadline and a cancel flag checked from `on_progress` (which is also how a turn
abort unwinds a script that is blocked on a tool). Above that: `timeout-ms`,
`max-tool-calls`, and `max-output-chars`. A cancellation must always answer the
reply channel, or the script thread blocks forever — the bridge guarantees a
reply by construction.

The result is one block, in the shape the references converge on: a completed or
failed header with wall time, the script's value (`text()` items and the return
value in order), a `<console_output>` block, a per-call trace (name, arguments
preview, duration, status) for every nested call, and a truncation marker. Parse
and eval errors carry Rhai's line/column.

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
  enabled #true      // default #false — inert until opted in
  mode "declared"    // "declared": tools stay in the roster, described as
                     //   scriptable. "declare-builtin": the builtins stay, and
                     //   stdio/MCP tools are reachable only from a script.
                     //   "only": tools are hidden from the model and reachable
                     //   only through a script.
  inline-budget 3000 // estimated tokens of inlined declarations
  timeout-ms 0       // 0 = unset
  max-operations 0
  max-tool-calls 0
  max-output-chars 20000
  store #true

  // Optional: tools scripts may not orchestrate, though the model may still
  // call them directly. Reuses `SceneToolsConfig` verbatim, so `enable-all`,
  // `ask-all` and `ask` work here too.
  tools { tool "run_shell" { disabled #true } }
}
```

Scenes need nothing new: `tools { disable-all; tool "codemode" { enabled #true } }`,
and per-tool `disabled` / `ask` overrides, all work through `ToolScene`/`Access`
as they do today.

### The three modes

`mode` decides how much of the roster the model sees directly, and every mode
puts the same catalog in front of the script — the difference is only what the
model can call *without* writing one.

| mode | the model sees | a script reaches |
| --- | --- | --- |
| `declared` (default) | everything, described as scriptable | everything |
| `declare-builtin` | the builtins only | everything |
| `only` | `codemode` alone | everything |

`declare-builtin` is the one that pays for itself. The builtins are few and
stable — `read_file`, `edit_file`, `grep`, `run_shell` — and a schema each is
cheap; a connected MCP server or a pile of configured `tool` subprocess entries
is where the roster bloats into dozens of schemas the model mostly does not
call, and where a typed call in a script reads better than one schema per tool.
The split is drawn by *provenance*, not by size: configured sources (MCP servers
and `tool` entries) go script-only, tools written in Rust stay declared.

One rule holds across all three modes: **a tool a script may not run never
leaves the model's roster.** `codemode { tools { tool "x" { disabled #true } } }`
means no script can reach `x`, so hiding it as well would make it reachable by
neither route — gone rather than hidden. Whatever a mode hides, it hides only
what is left in the script's roster.

## Telling the model about Rhai

The model is fluent in JavaScript and not in Rhai, so the syntax has to be
taught, in two channels:

- **Dialect card** — always in the `codemode` description: the essentials
  (`let`, `fn`, `if`/`else` as an expression, `for x in arr`, `switch`,
  `try {} catch (err) {}`, `#{ key: value }` maps, `[]` arrays, backtick
  interpolation, `.len`/`.push`), the globals table, and two worked examples.
- **Embedded skill `codemode`** — the full reference (Rhai method surface, error
  handling, and recipes: paginate, fan out, filter before returning) shipped in
  the binary and loaded on demand through the existing `skill` tool. This needs
  `Skills::with_builtins(...)`, since skills are currently directory-only.

The footguns belong in the card, because they are what actually costs a retry:
a missing map key evaluates to `()` instead of throwing, `/` on integers is
integer division, there is no ternary operator, and there is no `?.`.

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

0. **Spike.** Pin the Rhai semantics this design depends on — static-module
   namespaces without `import`, `Fn`/`FnPtr`, `on_progress` interrupt with
   `set_max_*`, `rhai::serde` JSON ↔ `Dynamic` (null → `()`, integers, `u64`
   beyond `i64::MAX`), top-level `return`, `exit()`. The spike's assertions
   become `crates/codemode/tests/spike.rs`.
1. **`crates/codemode`.** `spec` / `render` / `convert` / `script` / `doc`, with
   the `ToolInvoker` trait. Unit tests: schema→signature rendering (optionals,
   enums, arrays, nesting), conversion round-trips, limits, error paths.
2. **Core wiring.** The tool, the roster→catalog snapshot, the dispatcher
   (scene-gated `DynamicTool`, permissions, `FileChange`/`ShellStreams`
   forwarding, truncation), and the per-turn store. Integration tests with a
   stub roster.
3. **Config, modes, discovery.** `enabled` / `mode`, the budget, and the
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
- **Rhai is not TypeScript.** The signature strings are documentation for the
  model, not a checker — a wrong argument type fails at call time.
- **No OS sandbox.** Isolation is capability-based (no file, network, or process
  access of its own), and a script that calls `run_shell` inherits `run_shell`'s
  trust.
- **Prompt-cache churn.** The catalog is part of the description, so an MCP
  server connecting, or a tool toggling, invalidates the cache once.
- **`read_file`'s per-turn dedupe cache** exists to avoid re-sending content the
  model already has; a script's intermediate values never reach the model, so
  nested reads must opt out of it.
- **`store` is in-memory per session.** Durable keys would ride the session
  record (Pi appends a store entry per successful script).
- **Nothing is undone.** Calls made before a failure are real, as in the
  references.
