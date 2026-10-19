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
//!
//! The shape of this document follows the Rhai cheat sheet
//! (<https://optozorax.github.io/rhai_cheats/>) where a lookup table is the
//! right form, and the function lists were taken from the Rhai Book
//! (<https://rhai.rs/book/>), which documents this same 1.26 series.
//!
//! Every claim has been checked against the Rhai this tool actually builds
//! (1.26.1, default features) by running it, because both sources describe the
//! language in general and this build is behind in places. A table entry the
//! engine rejects is worse than no entry, so `round(x, n)`, `substr`, string
//! `find`, `get(k, d)`, `for key in map`, `this` methods, method-call sugar on
//! script functions, `let x = try { }`, and BLOBs are all absent here because
//! they do not run -- and `tests/spike.rs` pins each one that would otherwise
//! be tempting, so an upgrade that adds one fails the build instead of leaving
//! this document quietly wrong.

/// The skill's name, as `skill` invocations and the roster see it.
pub const NAME: &str = "codemode";

/// The one line the roster shows, so the model knows when to load it.
pub const DESCRIPTION: &str = "Writing Rhai scripts for the `codemode` tool: types, methods, \
     control flow, error handling, and the patterns that keep a script's result small.";

/// The skill's body, without the frontmatter a `SKILL.md` would carry.
pub const BODY: &str = r##"# Writing codemode scripts

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
from the signature (`-> Dynamic` is honest), so read what you got -- `type_of`
answers it:

```rhai
let todos = tools::todos();
if type_of(todos) == "string" {
    `${todos.len} characters of text`
} else {
    let open = todos.filter(|todo| !todo.done);
    `${open.len} of ${todos.len} still open`
}
```

## Values and types

Rhai has `int` (64-bit), `float`, `bool`, `char`, `String`, arrays, maps, and
`()` -- the unit value, which is also what a missing map key reads as. A type is
never declared and never checked until an operation needs it.

```rhai
let n = 7;              // int
let ratio = 0.5;        // float
let name = "main";      // String
let ch = 'a';           // char -- single quotes, an Unicode scalar
let paths = ["a", "b"]; // array
let spec = #{ path: "src/main.rs", limit: 20 }; // map
`${type_of(n)} ${type_of(ratio)} ${type_of(ch)} ${type_of(spec)}`
```

`type_of` returns `"i64"`, `"f64"`, `"bool"`, `"char"`, `"string"`, `"array"`,
`"map"`, or `"()"`. `is_def_var("x")` says whether a name is bound.

A `let` with no value starts as `()` -- a common way to declare the variable a
`try`/`catch` will assign:

```rhai
let out;
try { out = tools::read_file(#{ path: "maybe.txt" }); } catch (err) { out = `no: ${err}`; }
out
```

`let` is mutable; `const` is not, and its initializer must be constant. Both are
block-scoped, and an inner `let` shadows an outer one without touching it. A
block is an expression, so it can compute something once and hand it out:

```rhai
let x = 1;
let shadowed = { let x = 2; x };   // 2; the outer x is still 1
let spec = { let path = "src/main.rs"; #{ path: path, depth: x } };
`${shadowed} ${x} ${spec.depth}`
```

## Numbers and operators

| Literal | Example |
| --- | --- |
| decimal, `_` separators | `42`, `1_000_000` |
| binary | `0b1010` |
| octal | `0o755` |
| hexadecimal | `0xFF` |
| float, with exponent | `1.0`, `1.23e-4`, `2e-3` |

```rhai
let bits = 0b1010; let hex = 0xFF; let big = 1_000_000;
`${bits} ${hex} ${big} ${1 << 4} ${2 ** 10}`
```

`+ - * / %` on numbers; `**` is power; `<< >> & | ^` work on integers, but
there is **no bitwise NOT** (`!` is boolean only). `+= -= *= /= %= **= &= |= ^=
<<= >>=` assign in place. `== != < <= > >=` compare. `&&` and `||` short-circuit.

`/` on two integers is integer division, and there is no implicit promotion:
`7 / 2` is `3`, and `7.0 / 2` is `3.5`. Use `.to_float()` to force it.

```rhai
let pages = 7 / 2;               // 3
let exact = 7.to_float() / 2.0;  // 3.5
`${pages} pages, ${exact} exactly`
```

The numeric helpers, in the form that works. Several read as properties rather
than methods, and the difference is not guessable:

| Call | Gives |
| --- | --- |
| `abs(x)`, `min(a, b)`, `max(a, b)`, `sign(x)` | `sign` is a function, not `.sign` |
| `sqrt(x)`, `exp(x)`, `ln(x)`, `log(x)` (base 10), `log(x, base)` | |
| `hypot(x, y)`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan(y, x)` | radians |
| `floor(x)`, `ceiling(x)`, `round(x)` | `round` takes **no** digits argument |
| `.int`, `.fraction` | truncate, and the part after the point |
| `.is_even`, `.is_odd`, `.is_zero`, `.is_nan`, `.is_finite`, `.is_infinite` | properties |
| `PI()`, `E()` | functions; bare `PI` is `Variable not found` |
| `to_degrees()`, `to_radians()` | method form only |
| `to_hex()`, `to_binary()`, `to_octal()` | an integer, as text |
| `parse_int(radix)`, `parse_float()` | the inverse, from a string |

```rhai
let r = 2.0;
`${r.sqrt()} ${PI().round()} ${sign(-3)} ${(3.75).fraction} ${255.to_hex()}`
```

### The null-coalescing operator

`a ?? b` is `a` unless it is `()`, in which case it is `b`. Because a missing
map key already reads as `()`, it reads as "this key, or the fallback". After
`??` you can put `break`, `continue`, `return` or `throw` instead of a value, to
leave a loop or function at the first absent one:

```rhai
let counts = #{ a: 1 };
let total = 0;
for v in [1, 2, ()] { total += v ?? break; }
`${counts.b ?? 0} for a missing key, ${counts.a ?? 0} for a real one, ${total}`
```

## Strings

Double quotes for a plain string, backticks for interpolation. `len` is the
character count and `bytes` the byte count -- they differ on non-ASCII text.

```rhai
let who = "Rhai";
`Hello, ${who}! ${"héllo".len} chars, ${"héllo".bytes} bytes`
```

`+` joins a string with either side being the non-string: `"n=" + 1` is `"n=1"`
and `1 + "n"` is `"1n"`. A string has no `*` repeat.

**The methods that change the string in place return `()`** -- chaining one is
the classic mistake, because `line.trim().split(",")` fails at the `split`:

| Mutates, returns `()` | Returns a new value |
| --- | --- |
| `trim()`, `replace(a, b)`, `truncate(n)`, `remove(s)`, `clear()` | `to_upper()`, `to_lower()`, `sub_string(start[, len])`, `sub_string(range)` |
| `pad(width, ch)`, `crop(start[, len])`, `append(s)` | `split(delim[, max])`, `split_rev(delim, max)`, `index_of(needle[, from])` |
| `make_upper()`, `make_lower()` | `contains(s)`, `starts_with(s)`, `ends_with(s)`, `to_string()`, `parse_int([radix])`, `to_chars()` |

```rhai
let parts = "a, b, c".split(",");
let padded = "abc";
padded.pad(6, "-");          // in place; the call itself is ()
`${parts.len} parts, ${parts[1].trim()} unpadded, ${padded}`
```

`sub_string(1, 3)` is start-and-length (not end), `sub_string(2)` runs to the
end, and `sub_string(1..3)` takes a range. `chars(start)` is an iterable of
characters for a `for` loop; `to_chars()` is the array. `trim()` takes both
ends -- there is no one-ended trim. A char converts with `.to_int()` and back
with `.to_string()`.

**`split` with no argument splits on whitespace**, which is the way to read a
tool's prose output into words or numbers. With an argument it splits on that
delimiter, and a second argument caps the number of pieces:

```rhai
let words = "  3 files   found ".split();
let pair = "key=value=extra".split("=", 2);
`${words.len} words, then ${pair[1]}`
```

Single characters have their own forms: `get(i)` returns a char, `set(i, ch)`
replaces one in place, and `pop()` removes and hands back the last. `min(s)` and
`max(s)` compare two strings alphabetically.

## Arrays

Zero-based, and a **negative index counts from the end**. Indexing out of bounds
is an error, unlike a missing map key. `len` and `is_empty` are properties.

```rhai
let a = [3, 1, 2];
a.push(4); a.sort();          // both in place
`${a[0]} first, ${a[-1]} last, ${a.len} total, empty is ${[].is_empty}`
```

| Mutates, returns `()` | Returns |
| --- | --- |
| `push(v)`, `insert(i, v)`, `clear()`, `sort()`, `sort(cmp)`, `reverse()` | `pop()`, `shift()`, `remove(i)` |
| `truncate(n)`, `chop(n)`, `pad(n, v)`, `dedup()` (consecutive only) | `extract(start[, len])`, `extract(range)` |

`for_each` also returns `()` -- it is for effect, not for building a new array.
`retain(|v| ...)` is the exception worth knowing: it filters **in place**,
keeping what the closure accepts.

Higher-order: `map(|v| ...)`, `filter(|v| ...)`, `for_each(|v| ...)`,
`reduce(|acc, v| ..., seed)`, `reduce_rev(|acc, v| ..., seed)`,
`some(|v| ...)`, `all(|v| ...)`, `find(|v| ...)` and `find_map(|v| ...)` (which
give `()` when nothing matches), `index_of(v)`, `contains(v)`.

**Sorting** is `sort()` and `sort_desc()` for plain values, and a **comparator**
closure -- two parameters, negative/zero/positive -- for anything else.
`sort_by(|a, b| ...)` sorts in place and `order_by(|a, b| ...)` hands back an
ordered copy, leaving the original alone. A one-parameter closure is accepted
and sorts nothing at all, which is the trap: it looks like a key extractor and
silently returns the list in the order it arrived. `order_by` is the one to
reach for on tool results:

```rhai
let rows = [#{ name: "b", n: 2 }, #{ name: "a", n: 3 }];
let by_name = rows.order_by(|x, y| if x.name < y.name { -1 } else { 1 });
let by_n = rows.order_by(|x, y| x.n - y.n);
`first by name ${by_name[0].name}, biggest ${by_n[by_n.len - 1].name}`
```

`get(i)` returns `()` rather than erroring when the index is out of range,
`set(i, v)` assigns in place, `split(at)` cuts into head and tail, and
`drain(...)` removes by position, range, or predicate and hands back what went.

```rhai
let scores = [3, 8, 5];
let best = scores.reduce(|acc, v| if v > acc { v } else { acc }, 0);
`scores [${scores}], best ${best}, any over 6: ${scores.some(|v| v > 6)}`
```

`a += [1, 2]` extends with an array and `a += 1` appends a single item. There is
no `join` and no sum helper: build a string with `reduce` and a total with a
`for` loop. `zip` exists but takes the combiner, so it pairs lazily instead of
building an array of pairs:

```rhai
let names = ["a", "b", "c"];
let sizes = [10, 20];
let labelled = names.zip(sizes, |name, size| `${name}=${size}`);
labelled
```

## Object maps

String-keyed. `m.key` and `m["key"]` both read, but only the index form
evaluates an expression. A **missing key is `()`**, not an error -- check with
`.contains("k")` or `"k" in m` when that matters. Keys come back **sorted**, not
in the order they were written. `len()` and `is_empty()` are methods here (a
bare `.len` would be a key lookup).

| Returns | Mutates, returns `()` |
| --- | --- |
| `keys()`, `values()`, `get(k)`, `remove(k)`, `filter(|k, v| ...)`, `drain(|k, v| ...)` | `set(k, v)`, `clear()`, `retain(|k, v| ...)`, `mixin(other)`, `fill_with(other)` |

```rhai
let by_kind = #{ rs: 3, md: 1 };
if by_kind.contains("toml") { "has toml" } else { `${by_kind.len()} kinds, ${by_kind.rs}` }
```

`mixin` overwrites with the other map's values; `fill_with` only adds the keys
that are missing. `drain` hands back what it removed. `m += #{ k: v }` merges.
There is no `get(k, default)` -- write `m.get(k) ?? default`. `map(|k, v| ...)`
builds a new map from the old one's entries. `to_json()` renders a map as a JSON
string, and `parse_json(text)` turns one back into a map -- useful when a tool
answers in JSON rather than prose. It insists on an object at the top level: a
bare JSON array is a type error, so wrap it (`{"rows": [...]}`) or leave the
text alone.

**A map is not iterable** and its entries do not destructure: walk `m.keys()` and
index. This is the one place Rhai differs from JavaScript most sharply.

```rhai
let counts = #{ core: 2, db: 1 };
let lines = [];
for k in counts.keys() { lines.push(`${k}=${counts[k]}`); }
lines.reduce(|acc, line| `${acc} ${line}`, "")
```

### Building a map with computed keys

A map literal's keys are **literals**: a bare name or a quoted string. They are
never evaluated, so an interpolated key is a parse error and -- the quiet case --
a bare name is the word itself, not a variable of that name.

```rhai
let dir = "docs";
// #{ `${dir}/a.md`: 1 }   parse error: "Expecting name of a property"
// #{ dir: 1 }            builds #{"dir": 1}, not #{"docs": 1}

// Assign through the index (chaining works) or call `.set` (returns `()`).
let files = #{};
files[`${dir}/a.md`] = "one";
files.set("b.md", "two");
files
```

## Control flow

`if`/`else` is an expression and its condition must be a `bool` -- there is no
truthiness, so `if () { }` and `if 1 { }` are both errors. There is no ternary;
`if` is the ternary. `if` with no `else` and a false condition is `()`.

`while`, `loop` and `do { } while` all exist, with `break` and `continue`.
`for` walks an array, a string, or a range, and a second binding takes the index:

```rhai
let total = 0;
for (value, index) in [10, 20] { total += value * index; }  // 0 + 20
let letters = 0;
for c in "abc" { letters += 1; }
`${total} ${letters}`
```

A range is `0..n` (exclusive) or `0..=n` (inclusive); it is only good for
iterating and for `in`, so there is no `.len` or `.map` on one.

`switch` matches literals (strings included), ranges, `|` unions of them, and `_`
for the rest. It is an expression. **Guards are not supported** -- `x if x > 3`
is a syntax error, so write the conditions as an `if` chain.

```rhai
fn bucket(n) {
    switch n {
        0 => "zero",
        1 | 2 | 3 => "small",
        4..=10 => "medium",
        _ => "large",
    }
}
`${bucket(0)} ${bucket(2)} ${bucket(7)} ${bucket(99)}`
```

## Functions and closures

`fn` defines a function; its last statement is the value, or use `return` to
leave early. Parameters are dynamically typed and there are no default values
(`x ?? fallback` at the top of the body covers that).

```rhai
fn slug(text) {
    text.replace(" ", "-");   // in place; the next line reads the same variable
    text.to_lower()
}
["Main Rs", "Lib Rs"].map(|name| slug(name))
```

**A function receives copies.** A map or array edited inside one is the caller's
value only if the function returns it; `fn bump(m) { m.x = 1; }` leaves the
caller's map untouched. Build state in the calling scope instead.

**A script function is called by name, never as a method.** `slug("x")` works;
`"x".slug()` is `Function not found`, and `this` is reserved, so the
method-receiver form cannot be declared either. Only the built-in types have
methods.

Closures are `|x| x * 2` and capture the variable they use. A closure held in a
variable is called with **`.call()`**: `f(2)` is `Function not found: f`. A
closure is also what `map`, `filter`, `some` and `reduce` take, where the
argument form is the only form.

```rhai
let base = 10;
let add_base = |x| x + base;
`${add_base.call(5)}`
```

`Fn("name")` makes a function pointer from a named function, `.call(...)`
invokes it, and `.curry(fixed)` pre-applies arguments. That is worth reaching
for only when a function has to be chosen at runtime.

## Handling failure

A tool that fails -- denied by a permission rule, missing, or erroring -- raises
a **runtime error** in the script, which stops it unless you catch it. `try` and
`catch` are *statements*, not expressions: `let x = try { } catch { }` will not
parse, and a statement's value is `()`, so assign to a variable declared before.

```rhai
let src = "unreadable";
try { src = tools::read_file(#{ path: "missing.txt" }); } catch (err) { src = `no: ${err}`; }
src
```

Catching is how a script keeps going after a call it expected might fail. Let an
unexpected failure through instead: the error comes back to you with the line it
happened on, which is more useful than a script that quietly returned nothing.

`throw "message"` raises your own error, and the `catch` variable binds what was
thrown. It is the right way to end a script that found something it cannot work
with. `import` is not available -- there is no module resolver, and the tools are
already reachable.

## Reporting the result

The script's value is its **last expression**, or a top-level `return`, or
`exit(value)`. `text(value)` reports a value without ending the script.
`console.log(...)`, `print(...)` and `debug(...)` all write a line to the
result's log section, so they are for progress rather than for the answer.

```rhai
let todos = tools::todos();
text(`${todos.len} todos`);
print("checked the list");
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

Everything above states the language; these are the failures that surprise, and
the ones a script most often loses a round trip to.

- **A closure cannot touch the value it is iterating.** `m.keys().map(|k| m[k])`
  fails with a data-race error, and so do `.filter` and `.reduce` on
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

- **A wrong operator is a no-op, not an error.** `"a" - "b"` is `"a"`, and the
  result looks like a successful computation. Reach for the method you meant.
- **`let _ = x;` and `catch (_)` do not parse.** `_` is a binding only in a
  `switch` arm; name the variable (`catch (err) { }`) or drop the statement.
- **There is no `?.`.** Guard with `if` or `contains` before dereferencing.
"##;
