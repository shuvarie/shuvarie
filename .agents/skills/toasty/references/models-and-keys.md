# Models and Keys

A model is a Rust struct annotated with `#[derive(toasty::Model)]`. Each struct maps to a database table and each field maps to a column. An embedded type uses `#[derive(toasty::Embed)]` and is flattened into the parent table (no table of its own).

## Defining a model

```rust
use toasty::Model;

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    email: String,
}
```

In SQLite this produces:

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    email TEXT NOT NULL
);
```

Toasty auto-pluralizes the struct name for the table name. Override with `#[table = "people"]`.

## Supported field types

| Rust type | Database type |
|-----------|---------------|
| `bool` | Boolean |
| `String` | Text |
| `i8`/`i16`/`i32`/`i64`, `u8`/`u16`/`u32`/`u64` | Integer |
| `f32`/`f64` | Floating point |
| `uuid::Uuid` | UUID |
| `Vec<u8>` | Binary / Blob |
| `Vec<T>` (scalar `T`, not `u8`) | PostgreSQL native array; JSON on MySQL/SQLite; DynamoDB List `L` |
| `Option<T>` | Nullable version of `T` |
| `#[derive(toasty::Embed)]` types | Flattened into parent table columns |
| `#[document]` fields | One structured column (JSON/JSONB/Map) |

With feature flags:

| Feature | Rust type | Database type |
|---------|-----------|---------------|
| `rust_decimal` | `rust_decimal::Decimal` | Decimal |
| `bigdecimal` | `bigdecimal::BigDecimal` | Decimal |
| `jiff` | `jiff::Timestamp` | Timestamp |
| `jiff` | `jiff::civil::Date` / `Time` / `DateTime` | Date / Time / DateTime |
| `serde` | `toasty::Json<T>`, `serde_json::Value` | `text`/`json`/`jsonb` column |

## Optional fields

Wrap a field in `Option<T>` to make it nullable. Required fields (`String`, `u64`, etc.) map to `NOT NULL` columns.

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    bio: Option<String>, // nullable column; defaults to NULL on create
}
```

## Table names

Default: auto-pluralized (`User` → `users`). Override with `#[table = "people"]`.

## What `#[derive(Model)]` generates

For a model with basic fields (no relationships or indexes):

**Static methods:**
- `User::create()` — create builder (also via `toasty::create!(User { ... })`)
- `User::create_many()` — bulk insert builder
- `User::all()` — query all
- `User::filter(expr)` — query with filter
- `User::fields()` — field accessors for filter expressions

**Instance methods:**
- `user.update()` — update builder (takes `&mut self`); reloads with new values after `.exec()`
- `user.delete()` — delete builder (consumes `self`)

### Setter flexibility (IntoExpr)

Builder setters accept more than the exact field type via `IntoExpr`. For a `String` field, pass `&str`, `String`, or `&String`. For numeric fields, pass the value directly or by reference. Field shorthand works in `create!`/`update!` like struct literals.

## Keys and auto-generation

Every model needs a primary key. `#[key]` marks the key field(s); `#[auto]` optionally auto-generates the value.

### Single-field keys

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
}
```

Generates `User::get_by_id()`, `User::upsert_by_id()`, `User::filter_by_id()`, `User::delete_by_id()`.

### Keys without `#[auto]`

You must supply the key value on create and ensure uniqueness:

```rust
#[derive(Debug, toasty::Model)]
struct Country {
    #[key]
    code: String, // no #[auto]
    name: String,
}

let country = toasty::create!(Country { code: "US", name: "United States" })
    .exec(&mut db).await?;
```

### Other key types

UUID is common. With `#[auto]` on `uuid::Uuid`, Toasty generates UUID v7 (time-ordered) by default. Newtype embedded structs can also be keys:

```rust
#[derive(Debug, toasty::Embed)]
struct UserId(uuid::Uuid);

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto] // proxies through UserId to <Uuid as Auto> — UUID v7
    id: UserId,
    name: String,
}
```

### Auto strategies

| Field type | `#[auto]` behavior | Explicit form |
|-----------|--------------------|---------------|
| `uuid::Uuid` | Generates UUID v7 | `#[auto(uuid(v7))]` |
| `u64`/`i64`/etc. | Auto-incrementing integer | `#[auto(increment)]` |

```rust
#[derive(Debug, toasty::Model)]
struct ExampleA {
    #[key]
    #[auto(uuid(v7))]
    id: uuid::Uuid,
    name: String,
}

#[derive(Debug, toasty::Model)]
struct ExampleB {
    #[key]
    #[auto(uuid(v4))] // random
    id: uuid::Uuid,
    name: String,
}

#[derive(Debug, toasty::Model)]
struct ExampleC {
    #[key]
    #[auto(increment)]
    id: i64,
    name: String,
}
```

UUID v7 values are time-ordered (better for indexes); v4 are random. Auto-increment requires database support (SQLite/Turso/PostgreSQL/MySQL; not DynamoDB).

### Composite keys

**Multiple `#[key]` fields:**

```rust
#[derive(Debug, toasty::Model)]
struct Enrollment {
    #[key]
    student_id: u64,
    #[key]
    course_id: u64,
    grade: Option<String>,
}

let e = Enrollment::get_by_student_id_and_course_id(&mut db, &1, &101).await?;
```

**Model-level `#[key(...)]`** (shorthand for `partition` + `local`):

```rust
#[derive(Debug, toasty::Model)]
#[key(student_id, course_id)]
struct Enrollment {
    student_id: u64,
    course_id: u64,
    grade: Option<String>,
}
```

`#[key(code)]` on the struct is equivalent to `#[key]` on the `code` field. You cannot mix plain field names with `partition`/`local` syntax in the same attribute.

**Partition and local keys** (for DynamoDB-style):

```rust
#[derive(Debug, toasty::Model)]
#[key(partition = user_id, local = id)]
struct Todo {
    #[auto]
    id: u64,
    title: String,
    user_id: u64,
}

let todo = Todo::get_by_user_id_and_id(&mut db, &1, &42).await?;
let todos = Todo::filter_by_user_id(&1).exec(&mut db).await?;
```

Multi-field partition: `#[key(partition = [tenant_id, org_id], local = [id])]`.

### Generated methods for keys

| Attribute | Generated methods |
|-----------|-------------------|
| `#[key]` on single field | `get_by_<field>()`, `filter_by_<field>()`, `upsert_by_<field>()`, `delete_by_<field>()` |
| `#[key]` on multiple fields | `get_by_<a>_and_<b>()`, `filter_by_<a>_and_<b>()`, `upsert_by_<a>_and_<b>()`, `delete_by_<a>_and_<b>()` |
| `#[key(a, b)]` on struct | Same as multiple `#[key]`; `a` is partition, `b` is local |
| `#[key(partition = a, local = b)]` | `get_by_<a>_and_<b>()`, `filter_by_<a>()`, `filter_by_<a>_and_<b>()`, `upsert_by_<a>_and_<b>()`, `delete_by_<a>_and_<b>()` |

An upsert requires every primary-key field to identify one conflict.

## Indexes and unique constraints

`#[unique]` — unique index, generates `get_by_*`, `filter_by_*`, `update_by_*`, `upsert_by_*`, `delete_by_*`. SQL uses native unique index; DynamoDB uses a separate index table with `attribute_not_exists`.

`#[index]` — non-unique index, generates `get_by_*` (errors if 0 or >1 match), `filter_by_*`, `update_by_*`, `delete_by_*`. No `upsert_by_*` (an upsert conflict must identify at most one record).

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[unique]
    email: String,
    #[index]
    country: String,
}
```

```sql
CREATE UNIQUE INDEX idx_users_email ON users (email);
CREATE INDEX idx_users_country ON users (country);
```

### Multi-column indexes

**Simple mode** (list fields; first is leading key):

```rust
#[derive(Debug, toasty::Model)]
#[index(game_title, top_score)]
struct GameScore {
    #[key]
    #[auto]
    id: u64,
    user_id: String,
    game_title: String,
    top_score: i64,
}
```

Generates prefix methods: `filter_by_game_title(...)`, `filter_by_game_title_and_top_score(...)`.

**Named mode** (`partition`/`local` for DynamoDB GSI):

```rust
#[derive(Debug, toasty::Model)]
#[index(partition = [tournament_id, region], local = [round])]
struct Match {
    #[key]
    #[auto]
    id: u64,
    tournament_id: String,
    region: String,
    round: String,
    player1_id: String,
    player2_id: String,
}
```

On SQL, `partition`/`local` is ignored — all fields form a flat composite index. On DynamoDB, `partition` = Hash, `local` = Range in the GSI KeySchema. Up to 4 partition and 4 local attributes per index.

**Custom index names:** `#[index(name = "scores_by_game", game_title, top_score)]` or `#[key(name = "...", ...)]`.

### Multi-column unique constraints

```rust
#[derive(Debug, toasty::Model)]
#[unique(coa_id, combination_hash)]
struct AccountCombination {
    #[key]
    #[auto]
    id: u64,
    coa_id: i64,
    combination_hash: String,
}
```

Generates `filter_by_coa_id`, `filter_by_coa_id_and_combination_hash`, `get_by_*`, `update_by_*`, `delete_by_*`, and `upsert_by_coa_id_and_combination_hash`. DynamoDB does NOT support composite unique constraints (`unsupported_feature`); single-column `#[unique(field)]` works.

### Indexing newtype fields

Newtypes (`struct Email(String)`) support `#[unique]`/`#[index]` on the model field. Multi-field embedded structs do NOT support `#[unique]`/`#[index]` on the parent field (column ordering is ambiguous) — index fields inside the embedded struct instead.

## Choosing `#[unique]` vs `#[index]`

Use `#[unique]` for fields that identify a single record (email, username, slug). Use `#[index]` for fields you query frequently but that repeat (country, status, category). Only `#[unique]` generates `upsert_by_*`.