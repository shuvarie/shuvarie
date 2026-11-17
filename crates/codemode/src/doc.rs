//! Teaching the model *this* Python, not Python.
//!
//! The language needs no introduction the way Rhai did -- but the surface does:
//! which names are global, how a tool is called, and where a tool stops being an
//! ordinary function. So the card that always travels in the tool's description
//! says exactly that, and the fuller reference (the `codemode` skill) says the
//! rest. Both are code: every fenced snippet in them is executed against a stub
//! roster by `tests/doc.rs`, so a snippet that stops running fails the build.
//! Snippets that exist to show a *mistake* are inline code spans instead, which
//! the test skips -- a rule worth keeping, because teaching a footgun means
//! writing something that must not run.

/// What codemode is for, in the tool's own words.
pub const INTRO: &str = "\
Run a Python script that calls the tools below. Intermediate results stay in the \
script, so several dependent calls cost one round trip and one summary — filter, \
reduce, and page through data before returning it. The script's value is its \
last expression; `text(value)` emits another, so a script can report several \
things, and `exit(value)` ends it early with one.";

/// The dialect card: the calling convention, the globals, and the traps.
pub const CARD: &str = r#"## Calling tools

A tool is a plain function: keyword arguments named after its schema's
properties, one JSON-shaped value back (its text, or a dict when the tool
returned structure).

```python
src = tools.read_file(path="src/main.rs")
hits = tools.grep(pattern="fn main", path="src")
lines = [line for line in hits.split("\n") if line.startswith("src/")]
text(f"{len(src)} characters of main.rs; {len(lines)} hits")
```

A property whose name is not an identifier takes the dict form:
`tools.mcp_call(**{"repo-name": "x"})`, or `tools.mcp_call({"repo-name": "x"})`.

## Globals

- `tools.<name>(...)` -- the roster. An MCP server's tools are
  `mcp.<server>.<name>(...)`.
- `text(value)` -- emit a value into the result, beside the script's own.
- `console.log/info/warn/error(...)` and `print` -- captured, shown in the result.
- `store(key, value)` / `load(key)` -- JSON state surviving between scripts.
- `ALL_TOOLS` -- every callable name, as a list of strings.
- `search_tools(query, limit=10, namespace=None)` -- ranks the roster and answers
  `{"matched": int, "tools": [{"name": ..., "description": ...}]}`; `matched`
  counts every hit, so a list that had to stop says so.
- `describe_tool(name)` -- one tool with its parameter descriptions;
  `describe_namespace("mcp.server")` -- a whole namespace. An unknown name raises
  `LookupError` naming the near-misses.

```python
found = search_tools("file", limit=5)
if not found["matched"]:
    f"nothing matched; {len(ALL_TOOLS)} tools are available"
else:
    describe_tool(found["tools"][0]["name"])
```

## What is different from ordinary Python

- **A tool call is a boundary.** Each one is a real call with its own permissions,
  its own limits, and its own record in the result's trace -- and it is the only
  way out of the script.
- **A failing tool raises `ToolError`** (a `RuntimeError`) carrying the tool's own
  message. Catch it, or the script ends there.

```python
try:
    text(tools.read_file(path="missing.txt"))
except ToolError as failure:
    text(f"could not read it: {failure}")
```

- **No filesystem, network, or `input()`.** `open`, `os`, `subprocess` and
  `requests` are not how a script reaches anything: the tools are. `input()`
  blocks forever, because the script is not interactive.
- **`print` is captured**, not shown live: it lands in the result's
  `<console_output>`, the same place `console.log` goes.
- **A capped `glob` looks exact.** It stops at 100 paths (`grep` at 200) and
  appends a note -- but the note is a line a path filter drops, so a truncated
  tally reads as exactly 100. Filter paths, and check `"truncated" in output`
  before trusting a count. One pattern over many directories hits the cap sooner
  than per-directory queries.

For the fuller reference -- the tool roster's shape, values and types, error
handling, and worked patterns -- load the `codemode` skill."#;
