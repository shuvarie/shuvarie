//! The `codemode` skill: the fuller reference, loaded on demand.
//!
//! The dialect card in [`crate::doc`] travels in the tool's description on every
//! turn, so it has to stay short. This is the rest of it -- the parts a script
//! only needs when it is already writing one -- and it ships with shuvarie rather
//! than living in a skills directory, because it is a description of this tool
//! rather than a user's material.
//!
//! It is deliberately *not* a Python tutorial. The model knows Python, and a
//! reference that re-taught it would be long enough not to be read. What it
//! teaches is the surface: the roster, the value, the boundary, and the handful
//! of ways this Python differs from the Python a script would otherwise assume.
//!
//! Like the card, it is code: `tests/doc.rs` executes every fenced snippet in it
//! against a stub roster by running it, so a reference that drifts from the
//! runtime fails the build.

/// The skill's name, as `skill` invocations and the roster see it.
pub const NAME: &str = "codemode";

/// The one line the roster shows, so the model knows when to load it.
pub const DESCRIPTION: &str = "Writing Python scripts for the `codemode` tool: the tool roster, \
     the value a script returns, discovery, error handling, and the patterns that keep a result \
     small.";

/// The skill's body, without the frontmatter a `SKILL.md` would carry.
pub const BODY: &str = r##"# Writing codemode scripts

A `codemode` script is Python. It calls the same tools you can call directly, and
its **value** -- not its transcript -- comes back to you. Reach for it when the
steps between the calls matter and you would rather not read them: filtering,
joining, paging, and checking several things before deciding.

## The roster

The tools you can call are in the tool's own description, and every one of them
is a module attribute:

- `tools.<name>` for the roster's own tools (`tools.read_file`).
- `mcp.<server>.<name>` for an MCP server's (`mcp.dev_radius.search`), one level
  per server.

A tool takes the arguments its JSON Schema names, as keyword arguments, and
returns one JSON-shaped value. Nothing is positional, nothing is optional that
the schema does not say is optional.

```python
src = tools.read_file(path="src/main.rs")
length = len(src)
f"{length} characters, {src.count(chr(10))} newlines"
```

**A name that had to change.** A Python identifier cannot contain `-`, so a tool
whose name has one is written with `_` instead: `resolve-library-id` becomes
`tools.resolve_library_id`. The description always prints the callable name, and
`search_tools` finds a tool either way. A property name that is not an identifier
(a schema may say `repo-name`) cannot be a keyword, so pass those as a dict:

```python
hits = tools.grep(pattern="fn main", path="src")
first = hits.split(chr(10))[0]
first
```

## The value

The script's value is its **last expression**, as in a REPL -- the statements
before it run for their effect. That is what a script normally returns:

```python
paths = tools.glob(pattern="src/**/*.rs").split(chr(10))
files = [path for path in paths if path.endswith(".rs")]
f"{len(files)} rust files, first is {files[0]}"
```

`text(value)` emits another: several `text()` calls and a last expression come
back as sections in the order they were produced. `exit(value)` ends the script
there with a value, and the rest of it does not run.

```python
count = len(tools.glob(pattern="src/**/*.rs").split(chr(10)))
text(f"{count} files")
text("second line")
count
```

## What a tool returns

A tool that answers in prose returns a **string**. One that returns data returns a
**dict or a list**. The signature cannot say which (`-> Any` is honest), so read
what you got rather than assuming:

```python
todos = tools.todos()
if isinstance(todos, str):
    f"{len(todos)} characters of prose"
else:
    open_todos = [todo for todo in todos if not todo["done"]]
    f"{len(open_todos)} of {len(todos)} still open"
```

## Keeping the result small

The point of a script is that its intermediates never reach the conversation, so
the value should be the answer and nothing else. Count, sum, join, and slice
before returning; a list of ten matches reads better than the file it came from.

```python
lines = tools.grep(pattern="fn main", path="src").split(chr(10))
by_file = {}
for line in lines:
    if not line:
        continue
    name = line.split(":")[0]
    by_file[name] = by_file.get(name, 0) + 1
sorted(by_file.items())
```

Paging is the same idea with a budget: ask for a slice, keep the cursor in the
value.

```python
paths = [path for path in tools.glob(pattern="src/**/*.rs").split(chr(10)) if path]
window = paths[0:2]
{"page": 1, "of": (len(paths) + 1) // 2, "paths": window}
```

## Finding a tool

The description inlines as much of the roster as its budget pays for; the rest is
still callable, and listed by name. `ALL_TOOLS` is every callable name.
`search_tools(query, limit=10, namespace=None)` ranks the roster and answers
`{"matched": int, "tools": [{"name", "description"}]}`, where `matched` counts
*all* hits so a cut list is visible. `describe_tool(name)` spells one out with its
parameter descriptions, and `describe_namespace("mcp.dev_radius")` lists a whole
namespace. Both accept a qualified or a bare name.

```python
found = search_tools("search")
names = [hit["name"] for hit in found["tools"]]
describe_tool(names[0]) if names else f"{len(ALL_TOOLS)} tools, none matching"
```

## Handling failure

A tool that fails -- denied by a permission rule, missing, or erroring -- raises
`ToolError`, a `RuntimeError` whose message is the tool's own. An unknown tool
name is an `AttributeError`; an unknown name given to `describe_tool` is a
`LookupError` naming the near-misses.

```python
try:
    text(tools.read_file(path="missing.txt"))
except ToolError as failure:
    text(f"skipped: {failure}")
"kept going"
```

A script that catches everything it calls is a script that returned nothing for a
reason nobody can see. Catch what you expect, and let the rest fail: the result
comes back with the line it happened on, which is more useful than silence.

`raise` your own failure the same way -- `raise ValueError("...")` -- to end a
script that found something it cannot work with.

## State between scripts

`store(key, value)` and `load(key)` keep JSON state across calls in the session.
`load` of a key that was never stored is `None`, or the default you give it.

```python
seen = load("count", 0) + 1
store("count", seen)
load("count")
```

## Limits

Three limits are configurable, and the defaults are none: a wall-clock budget, a
line budget (`max-operations`), and a cap on nested tool calls. A script that
exceeds one is stopped and the result says which -- a `try`/`except` of your own
does not get to continue past it. Loops over tool output are the usual way to
find them, so bound the loop when the data could be unbounded.

```python
paths = [path for path in tools.glob(pattern="src/**/*.rs").split(chr(10)) if path]
total = 0
for path in paths[:3]:
    total += len(tools.read_file(path=path))
f"{total} characters across {min(3, len(paths))} files"
```

Two smaller things worth knowing before they cost a round trip: `import` works,
but nothing is installed for you -- the tools are the way to reach the
filesystem, the network, and the rest of shuvarie. And a script is not
interactive: `input()` blocks until the run is stopped.
"##;
