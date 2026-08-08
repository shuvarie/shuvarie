# Advanced Fields: Options, Embedded, Document, JSON, Vec<scalar>, Deferred

## Field options

### Custom column names

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    #[column("display_name")]
    name: String, // Rust field is user.name; DB column is display_name
}
```

### Explicit column types

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    #[column(type = varchar(100))]
    name: String, // VARCHAR(100) instead of TEXT
    #[column("display_name", type = varchar(100))]
    nickname: String, // combine name + type
}
```

Supported type values: `boolean`, `int`/`i8`/`i16`/`i32`/`i64`, `uint`/`u8`/`u16`/`u32`/`u64`, `text`, `varchar(N)`, `json`, `jsonb`, `numeric`/`numeric(P, S)`, `binary(N)`/`blob`, `timestamp(P)`, `date`, `time(P)`, `datetime(P)`.

Not all databases support all types. Toasty validates at `db.push_schema()` time. E.g. `varchar` is supported by PostgreSQL/MySQL but NOT SQLite/Turso/DynamoDB.

### Default values

```rust
#[derive(Debug, toasty::Model)]
struct Post {
    #[key]
    #[auto]
    id: u64,
    title: String,
    #[default(0)]
    view_count: i64,
}
```

Expression inside `#[default(...)]` is any Rust expression, evaluated at insert time. Applied on create and the create branch of an upsert; not on update. Override by setting the field explicitly.

### Update expressions

```rust
#[derive(Debug, toasty::Model)]
struct Post {
    #[key]
    #[auto]
    id: u64,
    title: String,
    #[update(jiff::Timestamp::now())]
    updated_at: jiff::Timestamp,
}
```

`#[update(expr)]` applies on both create and update (and both upsert branches), unless explicitly overridden.

### Combining `#[default]` and `#[update]`

```rust
#[default("draft".to_string())]
#[update("edited".to_string())]
status: String,
```

Create: `"draft"`. Update: `"edited"`. An upsert selects the corresponding value for each branch.

### Timestamps with `#[auto]`

For fields named `created_at` or `updated_at` with `jiff::Timestamp`, bare `#[auto]` is a shorthand:

| Field name | Field type | `#[auto]` expands to |
|-----------|-----------|----------------------|
| `created_at` | `jiff::Timestamp` | `#[default(jiff::Timestamp::now())]` — set once on create |
| `updated_at` | `jiff::Timestamp` | `#[update(jiff::Timestamp::now())]` — refreshed on every create and update |

On key fields, bare `#[auto]` defers to the type's default strategy (increment for integers, UUID v7 for `Uuid`). Requires the `jiff` feature.

```rust
#[derive(Debug, toasty::Model)]
struct Post {
    #[key]
    #[auto]
    id: u64,
    title: String,
    #[auto]
    created_at: jiff::Timestamp,
    #[auto]
    updated_at: jiff::Timestamp,
}
```

### Date and time fields

With `jiff` feature:

| Rust type | Description |
|-----------|-------------|
| `jiff::Timestamp` | Instant in time (UTC) |
| `jiff::civil::Date` | Date without time |
| `jiff::civil::Time` | Time of day without date |
| `jiff::civil::DateTime` | Date and time without timezone |

Control storage precision with `#[column(type = timestamp(3))]`, `#[column(type = time(0))]`.

## Embedded types

`#[derive(toasty::Embed)]` on a struct or enum stores its fields inline in the parent table (no table of its own).

### Newtype structs

A single-field tuple struct maps to a single column (no prefix):

```rust
#[derive(Debug, toasty::Embed)]
struct Email(String);

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    email: Email, // column "email", NOT "email_0"
}

let user = toasty::create!(User {
    name: "Alice",
    email: Email("alice@example.com".into()),
}).exec(&mut db).await?;
assert_eq!(user.email.0, "alice@example.com");
```

Newtypes support `#[key]`, `#[unique]`, `#[index]`, filtering, updating — same as primitives.

A newtype can be a primary key:
```rust
#[derive(Debug, toasty::Embed)]
struct UserId(String);

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    id: UserId,
    name: String,
}
```

With `#[auto]` proxying through the inner type:
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

Newtypes inside embedded structs:
```rust
#[derive(Debug, toasty::Embed)]
struct ZipCode(String);

#[derive(Debug, toasty::Embed)]
struct Address {
    city: String,
    zip: ZipCode, // column "address_zip", not "address_zip_0"
}

let users = User::filter(User::fields().address().zip().eq(ZipCode("98101".into())))
    .exec(&mut db).await?;
```

### Embedded structs

```rust
#[derive(Debug, toasty::Embed)]
struct Address {
    street: String,
    city: String,
}

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    address: Address,
}
```

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    address_street TEXT NOT NULL,
    address_city TEXT NOT NULL
);
```

Create:
```rust
let user = toasty::create!(User {
    name: "Alice",
    address: Address { street: "123 Main St".to_string(), city: "Seattle".to_string() },
}).exec(&mut db).await?;
```

Update — replace wholesale:
```rust
user.update().address(Address { street: "456 Oak Ave".to_string(), city: "Portland".to_string() })
    .exec(&mut db).await?;
```

Patch individual sub-fields with `stmt::patch`:
```rust
use toasty::stmt;
user.update().address(stmt::patch(Address::fields().city(), "Portland"))
    .exec(&mut db).await?;
```

Combine multiple sub-field updates with `stmt::apply`:
```rust
user.update().address(stmt::apply([
    stmt::patch(Address::fields().street(), "456 Oak Ave"),
    stmt::patch(Address::fields().city(), "Portland"),
])).exec(&mut db).await?;
```

### Nested embedding

```rust
#[derive(Debug, toasty::Embed)]
struct Coordinates { lat: i64, lng: i64 }

#[derive(Debug, toasty::Embed)]
struct Address {
    street: String,
    city: String,
    coords: Coordinates,
}
// User with address: Address produces: address_street, address_city, address_coords_lat, address_coords_lng
```

### Embedded enums

Unit enums map to a single column storing the variant label (`snake_case` by default):

```rust
#[derive(Debug, PartialEq, toasty::Embed)]
enum Status { Pending, Active, Done }

#[derive(Debug, toasty::Model)]
struct Task {
    #[key]
    #[auto]
    id: u64,
    title: String,
    status: Status,
}
```

PostgreSQL uses a named enum type, MySQL uses `ENUM`, SQLite uses `TEXT` with check constraint, DynamoDB stores the label as a string.

Data-carrying enums — each variant's fields become nullable columns (only active variant's columns are non-null):

```rust
#[derive(Debug, PartialEq, toasty::Embed)]
enum ContactInfo {
    Email { address: String },
    Phone { number: String },
}
// produces: discriminant column "contact", nullable "contact_address", nullable "contact_number"
```

Mixed enums (both unit and data-carrying variants) supported.

### Changing stored discriminants

Default: `snake_case`. Override with `#[column(rename_all = "...")]`:

| Rule | `PreferredSupplier` label |
|------|---------------------------|
| `lowercase` | `preferredsupplier` |
| `UPPERCASE` | `PREFERREDSUPPLIER` |
| `PascalCase` | `PreferredSupplier` |
| `camelCase` | `preferredSupplier` |
| `snake_case` | `preferred_supplier` |
| `SCREAMING_SNAKE_CASE` | `PREFERRED_SUPPLIER` |
| `kebab-case` | `preferred-supplier` |
| `SCREAMING-KEBAB-CASE` | `PREFERRED-SUPPLIER` |

Individual labels with `#[column(variant = "...")]` (takes precedence over `rename_all`):

```rust
#[derive(toasty::Embed)]
enum PartyKind {
    #[column(variant = "customer")]
    Customer,
    #[column(variant = "preferred-supplier")]
    PreferredSupplier,
}
```

Integer discriminants (`#[column(variant = N)]` on every variant, non-negative `i64`, need not be sequential):

```rust
#[derive(toasty::Embed)]
#[column(type = u8)] // narrower storage
enum Priority {
    #[column(variant = 10)]
    Low,
    #[column(variant = 20)]
    Normal,
    #[column(variant = 30)]
    High,
}
```

The type follows the enum through flattened embedded structs and transparent wrappers (`Option`, `Deferred`, `Box`, `Arc`, `Rc`). Field-level type overrides: `#[column(type = u16)] recent_priorities: Vec<Priority>`. Every discriminant must fit. Cannot mix string and integer discriminants. Integer-discriminant enums do not support `rename_all`.

### Filtering on embedded fields

Struct fields — chained accessors:
```rust
let users = User::filter(User::fields().address().city().eq("Seattle")).exec(&mut db).await?;
let users = User::filter(
    User::fields().address().city().eq("Seattle")
        .and(User::fields().address().street().eq("123 Main St")),
).exec(&mut db).await?;
```

Enum variants — `is_*()` methods:
```rust
let tasks = Task::filter(Task::fields().status().is_active()).exec(&mut db).await?;
let tasks = Task::filter(Task::fields().status().eq(Status::Active)).exec(&mut db).await?;
```

Data-carrying — `.matches()`:
```rust
let users = User::filter(
    User::fields().contact().email().matches(|e| e.address().eq("alice@example.com")),
).exec(&mut db).await?;
```

### Indexing embedded fields

Add `#[index]` or `#[unique]` to fields inside an embedded type. The index applies to the flattened column:
```rust
#[derive(Debug, toasty::Embed)]
struct Contact {
    #[unique]
    email: String,
    #[index]
    country: String,
}
```

## `#[document]` fields

`#[document]` stores an embedded struct in one structured column instead of expanding fields. Toasty retains the schema, so queries can address scalar fields inside the stored object.

```rust
#[derive(Debug, toasty::Embed)]
struct Address { city: String, postal_code: String }

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    #[document]
    address: Address,
}
```

### Storage by backend

| Driver | Stored representation |
|--------|----------------------|
| PostgreSQL | `jsonb` |
| MySQL | `JSON` |
| SQLite and Turso | JSON text |
| DynamoDB | Map (`M`) |

Do NOT add `#[column(type = ...)]` to a document field — the driver selects the representation.

### Creating/reading/filtering

```rust
let user = toasty::create!(User {
    address: Address { city: "Seattle".to_string(), postal_code: "98101".to_string() },
}).exec(&mut db).await?;
assert_eq!(user.address.city, "Seattle");

let users = User::filter(User::fields().address().city().eq("Seattle"))
    .exec(&mut db).await?;
```

### Updating

Replace the complete document (no in-place sub-field mutation yet):
```rust
user.update().address(Address { city: "Portland".to_string(), postal_code: "97205".to_string() })
    .exec(&mut db).await?;
```

### Collections of documents

`Vec<T>` where `T` is an embedded struct stores a document array (no attribute needed):
```rust
#[derive(Debug, toasty::Embed)]
struct LineItem { sku: String, quantity: i64 }

#[derive(Debug, toasty::Model)]
struct Order {
    #[key]
    #[auto]
    id: u64,
    items: Vec<LineItem>,
}
```

Whole-value create/read/replace work. `stmt::push` appends one embedded value. Element predicates and removal operations are not yet supported.

### Restrictions

- Embedded enums cannot be inside a document
- Tuple structs rejected (no field names for document keys)
- Relations cannot appear inside a document
- `jiff::Zoned` rejected (no supported document representation)
- `Vec<u8>` rejected (JSON has no binary scalar)
- `#[column]` renames inside an embedded document rejected (keys use Rust field names)
- `#[index]`, `#[unique]`, `#[column]` cannot be placed on the `#[document]` field
- Optional document root not yet supported (optional fields inside the document are supported)

## JSON encoding

`toasty::Json<T>` stores a serde-compatible Rust value in one column. Requires `serde` feature + driver feature.

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toasty = { version = "0.9", features = ["postgresql", "serde"] }
```

### Typed payload

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Metadata {
    version: u32,
    labels: Vec<String>,
}

#[derive(Debug, toasty::Model)]
struct Post {
    #[key]
    #[auto]
    id: u64,
    title: String,
    #[column(type = json)]
    metadata: toasty::Json<Metadata>,
}
```

Every `Json<T>` field requires an explicit `#[column(type = ...)]`. Column types: `text`, `varchar(N)`, `json` (PostgreSQL/MySQL), `jsonb` (PostgreSQL). Use `text` on SQLite/Turso/DynamoDB.

Setters accept the inner `T` (no need to construct `Json(T)`):
```rust
let mut post = toasty::create!(Post {
    title: "Encoding data",
    metadata: Metadata { version: 1, labels: vec!["rust".to_string()] },
}).exec(&mut db).await?;

post.update().metadata(Metadata { version: 2, labels: vec!["rust".to_string(), "orm".to_string()] })
    .exec(&mut db).await?;
```

`Json<T>` implements `Deref` and `AsRef`:
```rust
assert_eq!(post.metadata.version, 2);
let metadata: &Metadata = post.metadata.as_ref();
```

An update replaces the entire encoded value. Toasty does NOT generate field paths into `Json<T>` — the query engine sees the encoded value as a string.

### Dynamic JSON values

```rust
#[derive(Debug, toasty::Model)]
struct Event {
    #[key]
    #[auto]
    id: u64,
    #[column(type = jsonb)]
    payload: serde_json::Value,
}

let event = toasty::create!(Event {
    payload: serde_json::json!({ "kind": "published", "article_id": 42, "tags": ["rust", "orm"] }),
}).exec(&mut db).await?;
```

### SQL NULL vs JSON null

| Field type | Rust value | Stored value |
|-----------|------------|--------------|
| `Option<Json<T>>` | `None` | SQL `NULL` |
| `Json<Option<T>>` | `Json(None)` | JSON `null` in non-null column |
| `Option<serde_json::Value>` | `None` | SQL `NULL` |
| `serde_json::Value` | `Value::Null` | JSON `null` in non-null column |

### Deferring a JSON field

```rust
#[column(type = jsonb)]
payload: toasty::Deferred<toasty::Json<Payload>>,
```

Follows normal deferred loading rules. `toasty::Deferred<serde_json::Value>` also supported.

### Choosing a field representation

| Rust field | Use it for | Query support |
|-----------|-----------|---------------|
| `toasty::Json<T>` | Any `T` with serde | No typed paths into `T` |
| `#[document]` on embedded struct | Fixed object whose fields Toasty knows | Filters on scalar fields inside |
| `Vec<T>` (scalar `T`) | Homogeneous list | Collection predicates + incremental mutations |

## `Vec<scalar>` fields

A `Vec<scalar>` stores a homogeneous, ordered collection in a single column. Element type must be a scalar (any primitive except `u8`, plus `String`, `Uuid`, decimal types, `jiff` date/time types, and unit enums with `toasty::Embed`). `Vec<u8>` is a binary blob, not a collection.

### Storage by driver

| Driver | Representation |
|--------|---------------|
| PostgreSQL | Native array column (`text[]`, `int8[]`, …) |
| MySQL | `JSON` column |
| SQLite, Turso | JSON-encoded text |
| DynamoDB | List `L` attribute |

A `Vec<scalar>` field is always present — no `NULL`; empty rows hold an empty list.

### Defining

```rust
#[derive(Debug, toasty::Model)]
struct Article {
    #[key]
    #[auto]
    id: u64,
    title: String,
    tags: Vec<String>,
    scores: Vec<i64>,
}
```

Unit enum collections (each discriminant is the scalar):
```rust
#[derive(Clone, Copy, Debug, toasty::Embed)]
#[column(type = u16)]
enum Priority {
    #[column(variant = 10)]
    Low,
    #[column(variant = 20)]
    High,
}

#[derive(Debug, toasty::Model)]
struct Task {
    #[key]
    id: u64,
    priorities: Vec<Priority>, // enum-level u16 per element
    #[column(type = u8)]
    compact_priorities: Vec<Priority>, // field-level override
}
```

### Creating

Accepts `Vec<T>`, array literal `[T; N]`, or slice:
```rust
let article = toasty::create!(Article {
    title: "Hello",
    tags: ["rust", "toasty"], // no vec! needed
    scores: [1, 2, 3],
}).exec(&mut db).await?;

let article = Article::create()
    .title("Hello")
    .tags(["rust", "toasty"])
    .scores(vec![1, 2, 3])
    .exec(&mut db)
    .await?;
```

### Querying predicates

| Method | Meaning |
|--------|---------|
| `.contains(value)` | Array contains `value` |
| `.is_superset(values)` | Array contains every element of `values` |
| `.intersects(values)` | Array shares at least one element with `values` |
| `.len()` | Array's length, as `Expr<i64>` |
| `.is_empty()` | Array is empty |

```rust
let tagged = Article::filter(Article::fields().tags().contains("rust")).exec(&mut db).await?;
let both = Article::filter(Article::fields().tags().is_superset(["rust", "orm"])).exec(&mut db).await?;
let many = Article::filter(Article::fields().tags().len().gt(3)).exec(&mut db).await?;
let untagged = Article::filter(Article::fields().tags().is_empty()).exec(&mut db).await?;
```

### Updating

**Replace whole list:**
```rust
article.update().tags(["x", "y", "z"]).exec(&mut db).await?;
// explicit:
article.update().tags(toasty::stmt::set(["x", "y", "z"])).exec(&mut db).await?;
```

**Incremental mutations** (each produces one update statement, refreshes in-memory field after `.exec()`):

| Function | What it does |
|----------|--------------|
| `stmt::push(value)` | Append one element |
| `stmt::extend(iter)` | Append every element of an iterator, in order |
| `stmt::pop()` | Remove the last element |
| `stmt::remove(value)` | Remove every element equal to `value` |
| `stmt::remove_at(idx)` | Remove element at 0-based index |
| `stmt::clear()` | Replace with empty list |
| `stmt::apply([ops])` | Apply several in order, in one statement |

```rust
article.update().tags(toasty::stmt::push("toasty")).exec(&mut db).await?;
article.update().tags(toasty::stmt::extend(["orm", "async"])).exec(&mut db).await?;
article.update().tags(toasty::stmt::pop()).exec(&mut db).await?;
article.update().tags(toasty::stmt::remove("orm")).exec(&mut db).await?;
article.update().tags(toasty::stmt::remove_at(0usize)).exec(&mut db).await?;
article.update().tags(toasty::stmt::clear()).exec(&mut db).await?;
article.update().tags(toasty::stmt::apply([toasty::stmt::push("rust"), toasty::stmt::push("toasty")]))
    .exec(&mut db).await?;
```

Each operation is atomic against the existing column value. `pop` on empty, `remove` of absent value, `remove_at` past end are no-ops. `remove` deletes every matching element.

### Driver support

| Operation | PostgreSQL | MySQL | SQLite | DynamoDB |
|-----------|-------------|-------|--------|----------|
| Define/create/read | ✓ | ✓ | ✓ | ✓ |
| `contains`, `len`, `is_empty` | ✓ | ✓ | ✓ | ✓ |
| `is_superset`, `intersects` | ✓ | ✓ | ✓ | literal RHS only |
| Replace, `set`, `push`, `extend`, `clear` | ✓ | ✓ | ✓ | ✓ |
| `pop`, `remove`, `remove_at` | ✓ | — | — | — |

`pop`/`remove`/`remove_at` currently require PostgreSQL (`array_remove` and array slicing).

## Deferred fields

A deferred field is a column Toasty omits from the default `SELECT`. Fits large, expensive, or rarely-read columns. The API mirrors deferred relations: `.get()` (sync, reads loaded), `.include()` (preloads).

### Marking a field deferred

```rust
#[derive(Debug, toasty::Model)]
struct Document {
    #[key]
    #[auto]
    id: u64,
    title: String,
    body: toasty::Deferred<String>,
}
```

A record from an ordinary query has `body` unloaded:
```rust
let doc = Document::filter_by_id(created.id).get(&mut db).await?;
assert!(doc.body.is_unloaded());
```

Use `.include()`:
```rust
let doc = Document::filter_by_id(created.id)
    .include(Document::fields().body())
    .get(&mut db)
    .await?;
let body: &String = doc.body.get(); // sync, no query
```

`Deferred<T>` is supported on primitives and embedded types. Does NOT compose with `#[belongs_to]`/`#[has_many]`/`#[has_one]` (relation fields use `Deferred<_>` in the field type itself). `Deferred<T>` inside an embedded struct defers just that column; other embed fields still load.

```rust
#[derive(Debug, toasty::Embed)]
struct Metadata {
    author: String,
    notes: toasty::Deferred<String>,
}

// load sub-field on parent query:
let doc = Document::filter_by_id(id)
    .include(Document::fields().metadata().notes())
    .get(&mut db)
    .await?;
```

### Loaded state on create vs query

The record returned by `create!` is loaded with the deferred value the caller just wrote — `.get()` works without a round-trip. A subsequent query returns a separate record with the deferred field unloaded.

Calling `.get()` on an unloaded field **panics**. Use `.try_get()` for uncertain state.

### Preloading

```rust
let doc = Document::filter_by_id(id)
    .include(Document::fields().body())
    .include(Document::fields().summary())
    .include(Document::fields().author()) // relation
    .get(&mut db)
    .await?;
```

Multiple `.include()` calls coalesce. Across a result set, `.include()` avoids N+1.

### Filtering and sorting on deferred fields

Filtering/sorting references the column in `WHERE`/`ORDER BY` without loading the value:
```rust
let docs = Document::filter_by_id(alpha.id)
    .filter(Document::fields().body().eq("alpha body".to_string()))
    .exec(&mut db)
    .await?;
assert_eq!(1, docs.len());
assert!(docs[0].body.is_unloaded());
```

### Updating

Updating a deferred field does not require it to be loaded — the field is loaded with the new value after the update:
```rust
let mut doc = Document::filter_by_id(created.id).get(&mut db).await?;
assert!(doc.body.is_unloaded());
doc.update().body("new body".to_string()).exec(&mut db).await?;
assert_eq!("new body", doc.body.get());
```

### Optional deferred fields

```rust
summary: toasty::Deferred<Option<String>>,
```

A required `Deferred<T>` (where `T` is not `Option`) is a required argument to `create!`.

### Driver support

Supported on every driver. SQL backends shorten the `SELECT` column list; DynamoDB shortens the `ProjectionExpression`.