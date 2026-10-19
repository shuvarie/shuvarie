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
`.contains("k")`, `.keys()`, `.values()`, `.map(|v| ...)`, `.filter(|v| ...)`,
`.join(",")`, `.split(",")`, `.trim()`, `.contains("s")`, `.to_string()`,
`.to_int()`, `.starts_with("s")`.

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

The roster above is only the part that fit: the rest is callable and reachable
by name. `ALL_TOOLS` is the flat list of callable names, `search_tools(query)`
ranks the roster against a query and answers
`#{ matched: int, tools: [ #{ name, description } ] }` — `matched` counts every
match, so it says when the list was cut short — `describe_tool(name)` spells one
tool out with its parameter descriptions, and `describe_namespace(name)` lists a
whole namespace (`mcp::server`). A name nobody knows is an error naming the
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

- **Some methods return nothing.** `.trim()` and `.replace()` change the string
  in place and hand back `()`, so `line.trim() != ""` is always true. Bind
  first, or use a method that returns a value.
- **Tool output can end with a note.** `glob` stops at 100 paths, `grep` at 200,
  and each appends a sentence saying so — filter for what a match looks like
  (`line.ends_with(".rs")`), not just `line != ""`.
- A missing map key reads as `()`, silently: `hit.tool` on a map without that
  key is not an error. Check with `hit.contains("tool")` when it matters.
- Property access on a map is a key lookup, so `m.len` is `()`; write
  `m.len()`. On an array or a string `.len` is the property you want.
- A map is not iterable, and its entries do not destructure: iterate
  `for k in m.keys() { }` and read `m[k]`.
- `/` on integers is integer division: `7 / 2 == 3`.
- There is no ternary and no truthiness: `if () { }` is a syntax error. Use
  `if c { a } else { b }` and compare explicitly (`if x == () { }`).
- `try`/`catch` is a statement whose value is `()`, so assign the fallback to a
  variable declared before it:

```rhai
let out = "unset";
try { out = tools::read_file(#{ path: "maybe.txt" }); } catch (err) { out = `missing: ${err}`; }
out
```

- A wrong operator is a no-op, not an error: `"a" - "b"` is `"a"`.
- `_` is not a binding: `let _ = x;` and `catch (_)` do not parse. Name it, or
  drop the statement.

For the fuller reference — values and types, the methods that exist, error
handling, and worked patterns — load the `codemode` skill."#;
