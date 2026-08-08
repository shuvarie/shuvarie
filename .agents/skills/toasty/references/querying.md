# Querying: Filter Expressions, Sorting, Pagination

## Field paths

Every model has a `fields()` method returning typed accessors:

```rust
let name_path = User::fields().name();
let country_path = User::fields().country();
```

Field paths produce `Expr<bool>` when you call a comparison method, then pass to `Model::filter()`.

## Comparison methods

| Method | Meaning | SQL |
|--------|---------|-----|
| `.eq(value)` | Equal | `= value` |
| `.ne(value)` | Not equal | `!= value` |
| `.gt(value)` | Greater than | `> value` |
| `.ge(value)` | Greater than or equal | `>= value` |
| `.lt(value)` | Less than | `< value` |
| `.le(value)` | Less than or equal | `<= value` |
| `.in_list([...])` | Value in list | `IN (...)` |
| `.is_none()` | Null check (Option fields) | `IS NULL` |
| `.is_some()` | Not-null check (Option fields) | `IS NOT NULL` |
| `.starts_with(prefix)` | Case-sensitive prefix match | `begins_with` / `^@` / `GLOB` / `BINARY ... LIKE` |
| `.like(pattern)` | Pattern match (SQL only) | `LIKE pattern` |
| `.ilike(pattern)` | Case-insensitive (PostgreSQL only) | `ILIKE pattern` |

```rust
let users = User::filter(User::fields().name().eq("Alice")).exec(&mut db).await?;
let users = User::filter(User::fields().country().ne("US")).exec(&mut db).await?;
let users = User::filter(User::fields().country().in_list(["US", "CA", "MX"])).exec(&mut db).await?;
let users = User::filter(User::fields().bio().is_none()).exec(&mut db).await?;
let users = User::filter(User::fields().name().starts_with("Al")).exec(&mut db).await?;
```

`.is_none()`/`.is_some()` only on `Option<T>` fields — compile error on non-optional.

### `.like()` vs `.starts_with()` vs `.ilike()`

- `.starts_with()` works on all databases, case-sensitive on every backend. Prefer over `.like("prefix%")`.
- `.like()` is **SQL-only** (panics on DynamoDB). Case sensitivity varies:
  - PostgreSQL: case-sensitive
  - MySQL: set by column collation (`_ci` vs `_bin`)
  - SQLite: case-insensitive for ASCII, case-sensitive for non-ASCII
- `.ilike()` is **PostgreSQL-only**. Toasty does not emulate it elsewhere (returns `unsupported_feature`).

## Combining expressions

### AND

```rust
let events = Event::filter(
    Event::fields().kind().eq("info")
        .and(Event::fields().timestamp().gt(1000))
        .and(Event::fields().timestamp().lt(2000)),
).exec(&mut db).await?;
```

Equivalent — chaining `.filter()` adds AND conditions:
```rust
let events = Event::filter(Event::fields().kind().eq("info"))
    .filter(Event::fields().timestamp().gt(1000))
    .filter(Event::fields().timestamp().lt(2000))
    .exec(&mut db).await?;
```

### OR

```rust
let users = User::filter(
    User::fields().name().eq("Alice").or(User::fields().age().eq(35)),
).exec(&mut db).await?;
```

Expressions evaluate left to right; each method wraps everything before it. `a.or(b).and(c)` → `(a OR b) AND c`. To group differently, pass sub-expressions as arguments: `a.or(b.and(c))` → `a OR (b AND c)`.

### NOT

```rust
let users = User::filter(User::fields().name().eq("Alice").not()).exec(&mut db).await?;
let users = User::filter(!User::fields().name().eq("Alice")).exec(&mut db).await?; // `!` operator

// compound:
let users = User::filter(
    !(User::fields().name().eq("Alice").or(User::fields().name().eq("Bob"))),
).exec(&mut db).await?;
```

## Filtering on associations

A field path can traverse a relation (SQL-only):

```rust
let users = User::filter(User::fields().profile().score().gt(50)).exec(&mut db).await?;
```

### `.any()` — at least one match (HasMany)

```rust
let users = User::filter(
    User::fields().todos().any(Todo::fields().complete().eq(false)),
).exec(&mut db).await?;
```

`.any()` also works on a `has_many(via = ...)` path. For many-to-many, call `.any()` on the derived relation to filter by the opposite endpoint, or on the direct join-model relation to filter by join metadata.

### `.all()` — every related record matches (HasMany, SQL-only)

```rust
let users = User::filter(
    User::fields().todos().all(Todo::fields().complete().eq(true)),
).exec(&mut db).await?;
```

Vacuously true for a parent with no related records (mirrors `[].iter().all(...)`).

## Vec<scalar> predicates

| Method | Meaning |
|--------|---------|
| `.contains(value)` | The array contains `value` |
| `.is_superset(values)` | The array contains every element of `values` |
| `.intersects(values)` | The array shares at least one element with `values` |
| `.len()` | The array's length, as `Expr<i64>` |
| `.is_empty()` | The array is empty |

```rust
let tagged = Article::filter(Article::fields().tags().contains("rust")).exec(&mut db).await?;
let both = Article::filter(Article::fields().tags().is_superset(["rust", "orm"])).exec(&mut db).await?;
let many = Article::filter(Article::fields().tags().len().gt(3)).exec(&mut db).await?;
```

`.len()` produces `Expr<i64>` (not boolean) — pair with a comparison. `is_superset`/`intersects` on DynamoDB require a literal right-hand side.

## Embedded enum filtering

```rust
// Unit enum — is_*() or .eq():
let tasks = Task::filter(Task::fields().status().is_active()).exec(&mut db).await?;
let tasks = Task::filter(Task::fields().status().eq(Status::Active)).exec(&mut db).await?;

// Data-carrying enum — .matches():
let users = User::filter(
    User::fields().contact().email().matches(|e| e.address().eq("alice@example.com")),
).exec(&mut db).await?;
```

## Sorting with `.order_by()`

```rust
let items = Item::all()
    .order_by(Item::fields().order().asc())
    .exec(&mut db).await?;

let items = Item::all()
    .order_by(Item::fields().order().desc())
    .exec(&mut db).await?;
```

### Multiple fields (tie-breakers)

```rust
let users = User::all()
    .order_by((User::fields().age().asc(), User::fields().name().desc()))
    .exec(&mut db).await?;
```

Equivalent — chained `.order_by()` appends:
```rust
q.order_by(User::fields().age().asc()).order_by(User::fields().name().desc());
```

Works with filters:
```rust
let items = Item::filter(Item::fields().category().eq("books"))
    .order_by(Item::fields().order().desc())
    .exec(&mut db).await?;
```

## Limiting results

```rust
let items = Item::all().limit(5).exec(&mut db).await?;

// Top 7 by order (highest first):
let items = Item::all()
    .order_by(Item::fields().order().desc())
    .limit(7)
    .exec(&mut db).await?;
```

`.limit(n)` is an upper bound — Toasty may filter returned rows further. Use cursor pagination to walk every matching record.

## Offset

```rust
let items = Item::all()
    .order_by(Item::fields().order().asc())
    .limit(7)
    .offset(5) // requires .limit() first
    .exec(&mut db).await?;
```

Offset-based pagination gets slower as offset grows (database still reads/discards skipped rows) and can produce inconsistent results when rows are inserted/deleted between page fetches. Prefer cursor-based pagination.

## Cursor-based pagination

`.paginate(per_page)` requires `.order_by()` and returns a `Page`:

```rust
use toasty::stmt::Page;

let page: Page<_> = Item::all()
    .order_by(Item::fields().order().desc())
    .paginate(10)
    .exec(&mut db)
    .await?;

for item in page.iter() {
    println!("order: {}", item.order);
}
println!("items: {}", page.len());
```

`Page` dereferences to a slice (index, iterate, `.len()`, `.iter()`). `per_page` is an upper bound — a page can contain fewer than `per_page` items even when more exist. Check `.has_next()` rather than relying on page size.

### Navigating

```rust
if let Some(second_page) = first_page.next(&mut db).await? {
    println!("page 2 has {} items", second_page.len());
    if let Some(back) = second_page.prev(&mut db).await? {
        println!("back to page 1: {} items", back.len());
    }
}

if page.has_next() {
    let next = page.next(&mut db).await?.unwrap();
}
```

### Walking all pages

```rust
let mut page: Page<_> = Item::all()
    .order_by(Item::fields().order().asc())
    .paginate(10)
    .exec(&mut db)
    .await?;

loop {
    for item in page.iter() {
        println!("order: {}", item.order);
    }
    match page.next(&mut db).await? {
        Some(next) => page = next,
        None => break,
    }
}
```

### Starting from a cursor position

```rust
let page: Page<_> = Item::all()
    .order_by(Item::fields().order().desc())
    .paginate(10)
    .after(90) // start after order=90; first item will be order=89
    .exec(&mut db)
    .await?;
```

The value corresponds to the field used in `.order_by()`.

## Method summary

Query builders:

| Method | Description |
|--------|-------------|
| `.order_by(field.asc())` | Sort ascending |
| `.order_by(field.desc())` | Sort descending |
| `.order_by((a.asc(), b.desc()))` | Sort by multiple fields |
| `.limit(n)` | At most `n` records |
| `.offset(n)` | Skip first `n` (requires `.limit()`) |
| `.paginate(per_page)` | Cursor pagination (requires `.order_by()`) |
| `.latest_by(field)` | Shorthand for `order_by(field.desc())` |
| `.select(path)` | Project columns |
| `.include(path)` | Preload a relation/deferred field |
| `.filter(expr)` | Add AND condition |

`Page`:

| Method | Returns | Description |
|--------|---------|-------------|
| `.next(&mut db)` | `Result<Option<Page>>` | Fetch next page |
| `.prev(&mut db)` | `Result<Option<Page>>` | Fetch previous page |
| `.has_next()` | `bool` | Whether a next page exists |
| `.has_prev()` | `bool` | Whether a previous page exists |
| `.items` | `Vec<M>` | The records in this page |
| `.len()` | `usize` | Number of items (via `Deref` to slice) |
| `.iter()` | iterator | Iterate items |