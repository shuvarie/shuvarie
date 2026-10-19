//! Teaching the model Rhai.
//!
//! The model is fluent in JavaScript and not in Rhai, so the language has to be
//! taught twice: a card that always travels in the tool's description (here),
//! and a fuller reference it can load on demand (the `codemode` skill). Both
//! are code: every fenced snippet in them is executed against a stub roster by
//! `tests/`, so a snippet that stops parsing fails the build. Snippets that
//! exist to show a *mistake* are inline code spans instead, which the test
//! skips -- a rule worth keeping, because teaching a footgun means writing
//! something that must not run.

/// What codemode is for, in the tool's own words.
pub const INTRO: &str = "\
Run a Rhai script that calls the tools below. Intermediate results stay in the \
script, so several dependent calls cost one round trip and one summary — filter, \
reduce, and page through data before returning it. The script's value is its \
last expression (or a top-level `return`); use `text(value)` to emit more than \
one thing.";

/// The dialect card: the essentials, the globals, and the traps.
pub const CARD: &str = r#"## Calling tools

One map argument, one output. A tool returns its output text, or a map/array
when the tool returned structured content.

```rhai
let src = tools::read_file(#{ path: "src/main.rs" });
let hits = tools::grep(#{ pattern: "fn main", path: "src" });
let lines = hits.split("\n").filter(|line| line.starts_with("src/"));
`main.rs is ${src.len} characters; ${lines.len} lines matched`
```

The last expression is the script's value. `text(v)` emits a value as well, so
a script can report several things; `return v` ends early. `exit(v)` ends
early with a value too.

## Rhai essentials

`let x = 1;` (mutable), `const X = 1;`, `if c { a } else { b }` as an
expression, `for item in array { }`, `while c { }`, `break`, `continue`,
`fn name(a, b) { }`, closures `|x| x * 2`, maps `#{ key: value, "odd key": 1 }`,
arrays `[1, 2, 3]`, `switch x { 1 => "one", _ => "other" }`,
`try { } catch (err) { }`, and strings with backtick interpolation.
Methods: `.len()` (maps), `.len` (arrays), `.push(v)`, `.get("k")`,
`.keys()`, `.values()`, `.map(|v| ...)`, `.filter(|v| ...)`,
`.split(",")`, `.trim()`, `.contains("s")`, `.to_string()`,
`.parse_int(10)`, `.starts_with("s")`.

```rhai
let paths = tools::glob(#{ pattern: "src/**/*.rs" });
let files = paths.split("\n").filter(|line| line.ends_with(".rs"));
`${files.len} files, first is ${files[0]}`
```

## Globals

- `text(value)` — emit a value into the result.
- `console.log/info/warn/error(...)` — captured, shown in the result.
- `store(key, value)` / `load(key)` — small JSON state that survives between
  scripts in this session.
- `exit(value)` — end the script now, with a value.

## Finding a tool

The roster above is only what fit: the rest is callable by name. `ALL_TOOLS`
lists every callable name; `search_tools(query)` ranks the roster and answers
`#{ matched: int, tools: [ #{ name, description } ] }` (`matched` counts all
matches, so it says when the list was cut short); `describe_tool(name)` spells
one out with its parameter descriptions; `describe_namespace(name)` lists a
whole namespace (`mcp::server`). An unknown name is an error naming the
near-misses, not a silent `()`.

```rhai
let found = search_tools("file", #{ limit: 5 });
if found.matched == 0 {
    `nothing matched; ${ALL_TOOLS.len} tools are available`
} else {
    let hit = found.tools[0];
    `${hit.name} — ${hit.description}`
}
```

## Traps

- **Some methods return nothing.** `.trim()`/`.replace()` change the string in
  place and hand back `()`, so `line.trim() != ""` is always true. Bind first.
- **A capped `glob` looks exact.** It stops at 100 paths (`grep` at 200) and
  appends a note — but the note is a line a path filter drops, so a truncated
  tally reads as exactly 100. Filter paths, and check `out.contains("truncated")`
  before trusting a count. One pattern over many directories hits the cap
  sooner than the same work split per directory.
- On a map, a missing key is `()` silently and `.len` is a key lookup — write
  `m.len()`, and check `hit.contains("tool")` when it matters. Arrays and
  strings take the property.
- A map is not iterable, and its entries do not destructure: iterate
  `for k in m.keys() { }` and read `m[k]`.
- **A closure cannot read the variable it is iterating.** `m.keys().map(|k|
  m[k])` is a data-race error; bind first (`let ks = m.keys();`) or use `for`.
- `/` on integers is integer division: `7 / 2 == 3`.
- No ternary, no truthiness: `if () { }` is a syntax error. Use `if c { a } else
  { b }`, and compare explicitly (`if x == () { }`).
- `try`/`catch` is a statement whose value is `()`, so assign the fallback to a
  variable declared before it:

```rhai
let out = "unset";
try { out = tools::read_file(#{ path: "maybe.txt" }); } catch (err) { out = `missing: ${err}`; }
out
```

- A wrong operator is a no-op, not an error: `"a" - "b"` is `"a"`.
- `_` is not a binding: `let _ = x;` and `catch (_)` do not parse.

For the fuller reference — values and types, the methods that exist, error
handling, and worked patterns — load the `codemode` skill."#;
