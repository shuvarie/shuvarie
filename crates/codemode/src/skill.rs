//! The `codemode` skill: the fuller Rhai reference, loaded on demand.
//!
//! The dialect card in [`crate::doc`] travels in the tool's description on every
//! turn, so it has to stay short. This is the rest of it -- the parts a script
//! only needs when it is already writing one -- and it ships with shuvarie
//! rather than living in a skills directory, because it is a description of this
//! tool rather than a user's material.
//!
//! Like the card, it is code: `tests/doc.rs` executes every fenced snippet in
//! it, so a reference that drifts from the engine fails the build.

/// The skill's name, as `skill` invocations and the roster see it.
pub const NAME: &str = "codemode";

/// The one line the roster shows, so the model knows when to load it.
pub const DESCRIPTION: &str = "Writing Rhai scripts for the `codemode` tool: types, methods, \
     control flow, error handling, and the patterns that keep a script's result small.";

/// The skill's body, without the frontmatter a `SKILL.md` would carry.
pub const BODY: &str = r#"# Writing codemode scripts

A `codemode` script is Rhai. It calls the same tools you can call directly, and
its value -- not its transcript -- comes back to you. Use it when the steps
between the calls matter and you would rather not read them: filtering,
joining, paging, and checking several things before deciding.

## Calling a tool

Every tool takes **one map** of arguments and returns one value. The arguments
are the tool's own JSON Schema properties, so the map keys are what the schema
calls them. A parameter the schema does not require may be left out.

```rhai
let src = tools::read_file(#{ path: "src/main.rs" });
let head = tools::read_file(#{ path: "src/main.rs", limit: 20 });
if head.len < src.len { "the file is longer than 20 lines" } else { "it fits" }
```

A tool whose arguments are all optional can be called with none:

```rhai
let paths = tools::glob(#{ pattern: "src/**/*.rs" });
let all = tools::todos();
`${paths.len} characters of paths, ${all.len} todos`
```

The name a tool is called by is the name in the catalog: `tools::<name>` for a
roster tool, `mcp::<server>::<name>` for one of an MCP server's. Rhai identifiers
cannot contain `-`, so a name that had one is written with `_` instead
(`resolve-library-id` becomes `resolve_library_id`); the catalog always prints
the callable name, and `search_tools` finds it either way.

**A tool returns text or structure.** A tool that answers in prose returns a
string. One that returns data returns a map or an array. There is no way to know
from the signature (`-> Dynamic` is honest), so read what you got:

```rhai
let todos = tools::todos();
let open = todos.filter(|todo| !todo.done);
`${open.len} of ${todos.len} still open`
```

## Values and types

Rhai has `int` (64-bit), `float`, `bool`, `String`, arrays, maps, and `()` --
the unit value, which is also what a missing map key reads as.

```rhai
let n = 7;              // int
let ratio = 0.5;        // float
let name = "main";      // String
let paths = ["a", "b"]; // array
let spec = #{ path: "src/main.rs", limit: 20 }; // map
```

`/` on two integers is integer division, and there is no implicit promotion:
`7 / 2` is `3`, and `7.0 / 2` is `3.5`. Use `.to_float()` to force it.

```rhai
let pages = 7 / 2;          // 3
let exact = 7.to_float() / 2.0;  // 3.5
`${pages} pages, ${exact} exactly`
```

Strings interpolate with backticks and `${...}`. For everything else, `+` only
concatenates a string with a string: `"n=" + 1` is an error, so use
`.to_string()` or interpolation.

```rhai
let count = 2;
`count is ${count}; n=${count.to_string()}`
```

Useful methods, in the two groups that matter. **In place, returning `()`:**
`.trim()`, `.replace(a, b)`, `.truncate(n)`, `.remove(s)`, `.clear()` on a
string, and `.push(v)`, `.sort()`, `.reverse()` on an array. **Returning a new
value:** `.split(",")`, `.sub_string(0, 4)` (or `.sub_string(2)` to the end),
`.to_upper()`, `.to_lower()`, `.index_of("s")`, `.parse_int(10)`,
`.starts_with("s")`, `.ends_with("s")`, `.contains("s")` on a string, and
`.map(|v| ...)`, `.filter(|v| ...)`, `.reduce(|acc, v| ..., seed)`,
`.extract(0, 2)` on an array.

```rhai
let paths = tools::glob(#{ pattern: "src/**/*.rs" });
let files = paths.split("\n").filter(|line| line.ends_with(".rs"));
let joined = files.reduce(|acc, file| `${acc} ${file}`, "");
`${files.len} files, upper is ${joined.to_upper()}`
```

A map is read with `.get("k")`, `.keys()` and `.values()` (both new arrays),
`.contains("k")` (a bool), `.remove("k")` (which does hand back what it
removed) and `.len()`. It is not iterable, and its entries do not destructure:
iterate `for k in m.keys() { }` and index.

```rhai
let todo = tools::todos()[0];
for key in todo.keys() {
    console.log(`${key}`);
}
if todo.contains("done") && !todo.done { `${todo.text} is open` } else { "all done" }
```

`.len` is a **property** on arrays and strings but a **method** on maps
(`m.len()`), because on a map a bare `.len` is a key lookup. `.is_empty` exists
on arrays and strings.

```rhai
let empty = [];
`${[1, 2].len} ${empty.is_empty} ${#{ a: 1 }.len()}`
```

## Control flow

`let` and `const`, `if`/`else` as an expression, `for`, `while`, `loop`, and
`switch`. `break` and `continue` work in the loops. A `let` binding is mutable
(there is no `mut`, and no immutability to opt out of).

```rhai
let paths = tools::glob(#{ pattern: "src/**/*.rs" })
    .split("\n")
    .filter(|line| line.starts_with("src/"));
let kinds = #{ rust: 0, other: 0 };
for path in paths {
    if path.ends_with(".rs") {
        kinds.rust += 1;
    } else {
        kinds.other += 1;
    }
}
kinds
```

`fn` defines a function. Closures are `|x| ...` and capture by value. `for`
yields the value, or the value and its index with `for (v, i) in array { }`.

```rhai
fn slug(text) {
    text.replace(" ", "-");
    text
}
let hits = ["Main Rs", "Lib Rs"].map(|name| slug(name));
hits[0]
```

## Handling failure

A tool that fails -- denied by a permission rule, missing, or erroring -- raises
a **runtime error** in the script, which stops it unless you catch it. `try` and
`catch` are statements, and a statement's value is `()`, so assign to a variable
declared before it:

```rhai
let src = "unreadable";
try { src = tools::read_file(#{ path: "missing.txt" }); } catch (err) { src = `no: ${err}`; }
src
```

Catching is how a script keeps going after a call it expected might fail. Let an
unexpected failure through instead: the error comes back to you with the line it
happened on, which is more useful than a script that quietly returned nothing.

`throw` raises your own error, and is the right way to end a script that found
something it cannot work with.

## Reporting the result

The script's value is its **last expression**, or a top-level `return`, or
`exit(value)`. `text(value)` reports a value without ending the script, so a
script can report several things; `console.log(...)` writes a line to the
result's console section. Values reported by `text` come back in order, and the
last expression comes back as the value.

```rhai
let todos = tools::todos();
text(`${todos.len} todos`);
console.log("checked the list");
todos.filter(|todo| !todo.done).len
```

Keep the result small. Everything you report is read back into the context, so a
script that returns a whole file has spent more context than calling the tool
would have. Return the answer, not the material.

## Finding a tool

Only part of the roster fits in the tool's description; the rest is callable and
listed by name. `ALL_TOOLS` is every callable name. `search_tools(query)` ranks
the roster and answers `#{ matched: int, tools: [ #{ name, description } ] }`,
where `matched` counts every match so a cut-off list says so.
`describe_tool(name)` spells one tool out with its parameter descriptions, and
`describe_namespace(name)` lists one namespace (`tools`, or `mcp::server`).

```rhai
let found = search_tools("file", #{ limit: 5 });
if found.matched == 0 {
    `nothing matched; ${ALL_TOOLS.len} tools are available`
} else {
    `${found.tools[0].name} — ${found.tools[0].description}`
}
```

`search_tools` takes `#{ limit, namespace }`: `namespace: "mcp"` sees only the
MCP servers, `namespace: "mcp::github"` only that one. An unknown name given to
`describe_tool` is an error naming the near-misses rather than a silent `()`.

## Keeping state between scripts

`store(key, value)` and `load(key)` keep small JSON values for the rest of the
session, so a script can remember something for the next one. A key that was
never stored reads as `()`.

```rhai
store("last_checked", tools::glob(#{ pattern: "src/**/*.rs" }).len);
let previous = load("last_checked");
`stored ${previous}; ALL_TOOLS has ${ALL_TOOLS.len} names`
```

## Traps

- **Some methods return nothing.** `trim`, `replace`, `truncate`, `remove`,
  `clear`, and an array's `push`, `sort` and `reverse` all change the value in
  place and hand back `()`, while `to_upper`, `split`, `sub_string`, `index_of`,
  `map`, `filter` and `extract` return a new value. A chained mutating call is
  the classic mistake: `line.trim().split(",")` fails, because `trim()` is `()`
  by the time `split` sees it.
- **There is no `join`.** Build a string with `reduce`, or interpolate an array.
- **A string has no `to_int`.** Use `.parse_int(10)`, which throws on input that
  is not a number.
- **A missing map key is `()`, silently.** `todo.done` on a map without that key
  is not an error; it is `()`, which is falsy-looking but not `false`. Check with
  `todo.contains("done")` when it matters.
- **No truthiness.** `if () { }` is a syntax error, and numbers are not
  booleans. Compare explicitly: `if count > 0 { }`, `if value == () { }`.
- **No ternary.** Use `if c { a } else { b }`, which is an expression.
- **No `?.`.** Guard with `if` or `contains` before dereferencing.
- **Integer division**, as above.
- **A wrong operator is a no-op, not an error.** `"a" - "b"` is `"a"`. Reach for
  the method you meant.
- **A closure cannot touch the variable it is iterating.** `m.keys().map(|k|
  m[k])` fails with a data-race error, and so do `.filter` and `.reduce` on
  `m.keys()`/`m.values()` when the closure reads `m`: the method's result holds
  the map while the closure runs. Two escapes, both idiomatic:

```rhai
let counts = #{ core: 55, db: 15 };
// Bind the receiver first, then the closure may read the map.
let keys = counts.keys();
let widest = keys.reduce(|a, k| if counts[k] > counts[a] { k } else { a }, "core");
// Or walk it with a loop, which never creates a closure at all.
let biggest = "";
for k in counts.keys() {
    if biggest == "" || counts[k] > counts[biggest] { biggest = k; }
}
`${widest} / ${biggest}`
```

  The same rule catches a map being *built*: `m[k] = v` is fine inside a `for`
  loop, but not inside a closure that also holds `m`.
- **`let _ = x;` and `catch (_)` do not parse.** `_` is not a binding; name the
  variable (`catch (err) { }`) or drop the statement.
- **A capped result looks exact.** `glob` returns at most 100 paths and `grep`
  at most 200, and each ends its string with a sentence saying so. That note is a
  line like any other, so the filter that makes the rest usable
  (`.ends_with(".rs")`) drops exactly the line that says the answer is partial —
  a truncated tally reads as exactly `100` and looks like a real count. Check the
  text before you count, and keep one query to one directory: a single pattern
  spanning ten directories reaches the cap far sooner than ten queries do.

```rhai
let found = tools::glob(#{ pattern: "src/**/*.rs" });
let files = found.split("\n").filter(|line| line.ends_with(".rs"));
if found.contains("truncated") {
    `partial: ${files.len} — narrow the pattern`
} else {
    `${files.len} files`
}
```
- **There is no one-ended trim.** `trim_end_matches`, `strip_suffix` and
  `trim_end` do not exist; `trim()` takes both ends. Drop a single character
  with `s.sub_string(0, s.len - 1)`.
- **A map is not iterable** and its entries do not destructure, which is why the
  examples above walk `.keys()`.
- **A stored closure is called with `.call()`.** `let f = |x| x + 1;` then
  `f.call(2)` — `f(2)` is `Function not found: f`, because Rhai does not treat a
  variable holding a closure as a callable name.
- **A function gets values, not references.** `fn bump(m, k) { m[k] = 1; }`
  leaves the caller's map untouched: the parameter is a copy. Build state in the
  calling scope (a `for` loop) or return the new value.
- **Do not mutate a captured variable inside a closure.** Rhai borrows it for the
  closure, so changing it there fails with `Data race detected` rather than
  corrupting it — the fix is a plain `for` loop, which is also the readable
  idiom for building a map.
"#;
