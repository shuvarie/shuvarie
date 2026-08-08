# Batch Operations, Transactions, Raw SQL, Concurrency Control

## Batch operations

`toasty::batch()` executes multiple queries or creates in a single database round-trip. Batch operations are **atomic** (database permitting) — all succeed or all fail. Prefer batch over interactive transactions when you don't need read-then-branch.

### Batching queries with tuples

Up to 8 elements; return type matches the tuple structure:

```rust
let (users, posts): (Vec<User>, Vec<Post>) = toasty::batch((
    User::filter_by_name("Alice"),
    Post::filter_by_title("Hello"),
)).exec(&mut db).await?;

let (alices, bobs): (Vec<User>, Vec<User>) = toasty::batch((
    User::filter_by_name("Alice"),
    User::filter_by_name("Bob"),
)).exec(&mut db).await?;
```

### Batching with arrays and Vecs

Same-type queries — return `Vec<Vec<Model>>` (one inner Vec per query):

```rust
let results: Vec<Vec<User>> = toasty::batch([
    User::filter_by_name("Alice"),
    User::filter_by_name("Bob"),
    User::filter_by_name("Carol"),
]).exec(&mut db).await?;

// runtime-determined count:
let names = vec!["Alice", "Bob", "Carol"];
let queries: Vec<_> = names.iter().map(|n| User::filter_by_name(*n)).collect();
let results: Vec<Vec<User>> = toasty::batch(queries).exec(&mut db).await?;
```

### Batching creates with `create!`

Same-type batch (`Type::[ ... ]` → `Vec<Model>`):
```rust
let users = toasty::create!(User::[
    { name: "Alice", email: "alice@example.com" },
    { name: "Bob", email: "bob@example.com" },
]).exec(&mut db).await?;
```

Mixed-type batch (`( ... )` → tuple):
```rust
let (user, post) = toasty::create!((
    User { name: "Alice" },
    Post { title: "Hello World" },
)).exec(&mut db).await?;
```

### Batching creates with `toasty::batch()`

Mix creates and queries, or build creates dynamically:
```rust
let (user, post): (User, Post) = toasty::batch((
    toasty::create!(User { name: "Alice" }),
    toasty::create!(Post { title: "Hello World" }),
)).exec(&mut db).await?;
```

### Bulk creation with `create_many()`

```rust
let todos = Todo::create_many()
    .item(toasty::create!(Todo { title: "Buy groceries" }))
    .item(toasty::create!(Todo { title: "Write docs" }))
    .exec(&mut db).await?;

// or with closure:
let todos = Todo::create_many()
    .with_item(|c| c.title("Buy groceries"))
    .with_item(|c| c.title("Write docs"))
    .exec(&mut db).await?;
```

`create_many()` returns `Vec<Model>` including auto-generated fields.

### `create_many()` vs `batch()` for inserts

| | `create_many()` | `batch()` |
|---|----------------|-----------|
| Scope | Single model | Any mix of models, queries, creates |
| Return type | `Vec<Model>` | Matches input structure |
| Use case | Insert many records of same type | Combine diverse operations |

## Transactions

Interactive transactions on SQL databases (SQLite/Turso, PostgreSQL, MySQL). **Tip:** prefer batch operations for atomic multi-op when you don't need read-then-branch — batch is more efficient (single statement vs begin/execute/commit round-trips).

### Starting a transaction

```rust
use toasty::{Model, Executor};

let mut tx = db.transaction().await?;

toasty::create!(User { name: "Alice" }).exec(&mut tx).await?;
toasty::create!(User { name: "Bob" }).exec(&mut tx).await?;

tx.commit().await?;
```

The transaction borrows `&mut Db`, preventing other operations on the same `Db` while open. Pass `&mut tx` to query builders like `&mut db`. The `&mut` prevents accidentally bypassing the transaction on a separate pool connection.

If you need a second handle while a transaction is open:
```rust
let mut db2 = db.clone(); // clones share the underlying pool
let mut tx = db.transaction().await?;
// db2 is a separate handle; use it for unrelated work
```

### Running queries in a transaction

All operations work: creates, queries, updates, deletes. Reads see writes made earlier in the same transaction (even before commit):
```rust
let mut tx = db.transaction().await?;
toasty::create!(User { name: "Alice" }).exec(&mut tx).await?;
let users = User::all().exec(&mut tx).await?;
assert_eq!(users.len(), 1);
tx.commit().await?;
```

### Commit and rollback

```rust
tx.commit().await?; // save changes; visible outside
tx.rollback().await?; // discard changes
```

### Auto-rollback on drop

If dropped without `.commit()` or `.rollback()`, automatically rolls back. Useful in `Result`-returning functions where `?` drops the transaction on error:
```rust
async fn transfer(db: &mut Db) -> toasty::Result<()> {
    let mut tx = db.transaction().await?;
    let user = User::get_by_id(&mut tx, &1).await?;
    user.update().balance(user.balance - 100).exec(&mut tx).await?;
    let other = User::get_by_id(&mut tx, &2).await?;
    other.update().balance(other.balance + 100).exec(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}
```

### Nested transactions (savepoints)

```rust
let mut tx = db.transaction().await?;
toasty::create!(User { name: "Alice" }).exec(&mut tx).await?;

{
    let mut nested = tx.transaction().await?; // savepoint
    toasty::create!(User { name: "Bob" }).exec(&mut nested).await?;
    nested.commit().await?; // releases the savepoint
}

tx.commit().await?; // commits both Alice and Bob
```

Rolling back a nested transaction only undoes work inside it; the outer transaction continues:
```rust
{
    let mut nested = tx.transaction().await?;
    toasty::create!(User { name: "Bob" }).exec(&mut nested).await?;
    nested.rollback().await?; // Bob discarded
}
tx.commit().await?; // only Alice
```

Nested transactions also auto-rollback on drop.

### Transaction options

```rust
use toasty::IsolationLevel;

let mut tx = db.transaction_builder()
    .isolation(IsolationLevel::Serializable)
    .read_only(true)
    .begin()
    .await?;
```

#### Isolation levels

| Level | Description |
|-------|-------------|
| `ReadUncommitted` | Allows dirty reads |
| `ReadCommitted` | Only reads committed data |
| `RepeatableRead` | Consistent reads within the transaction |
| `Serializable` | Full isolation |

SQLite and Turso only support `Serializable`. PostgreSQL and MySQL support all four.

#### Read-only transactions

`.read_only(true)` — the database rejects write operations inside.

#### Lock-acquisition modes

`TransactionMode` is a separate axis from isolation level (describes when locks are acquired):

| Mode | SQLite SQL | Purpose |
|------|-----------|---------|
| `Default` | `BEGIN` | The driver's natural default |
| `Deferred` | `BEGIN` | Explicit deferred locking |
| `Immediate` | `BEGIN IMMEDIATE` | Acquire write lock at begin (later writes can't fail with BUSY) |
| `Exclusive` | `BEGIN EXCLUSIVE` | Hold exclusive lock for entire transaction |

```rust
use toasty_core::driver::operation::TransactionMode;

let mut tx = db.transaction_builder()
    .mode(TransactionMode::Immediate)
    .begin()
    .await?;
```

PostgreSQL and MySQL accept only `Default` and `Deferred`; `Immediate`/`Exclusive` return `Error::UnsupportedFeature`. `Default` and `Deferred` look identical on SQLite (both `BEGIN`) but diverge on Turso under `concurrent_writes()`: `Default` issues `BEGIN CONCURRENT` (MVCC), `Deferred` opts out to classic locking.

## Raw SQL

Raw SQL runs backend SQL through Toasty's handles. Use when you need a feature the query builders don't expose. **SQL backends only** (SQLite/Turso, PostgreSQL, MySQL); DynamoDB returns `unsupported_feature`.

### Statements (no rows returned)

Returns the number of affected rows:
```rust
let updated = toasty::sql::statement("UPDATE users SET name = ?1 WHERE id = ?2")
    .bind("Alice")
    .bind(1_i64)
    .exec(&mut db)
    .await?;
assert_eq!(updated, 1);
```

### Queries (rows returned)

Returns `Vec<Value>`; each row is `Value::Record` with fields in selected-column order:
```rust
let rows = toasty::sql::query("SELECT id, name FROM users WHERE active = ?1")
    .bind(true)
    .exec(&mut db)
    .await?;

for row in rows {
    let toasty::stmt::Value::Record(row) = row else { unreachable!() };
    println!("id={:?} name={:?}", row[0], row[1]);
}
```

Raw SQL queries do NOT hydrate models. They return dynamic values for any expression, function call, join result, or database-specific value.

### Placeholders

Toasty does NOT rewrite placeholders. Use the syntax reported by `db.capability().sql_placeholder`:

| Backend | `SqlPlaceholder` | Syntax |
|---------|------------------|--------|
| SQLite | `NumberedQuestionMark` | `?1`, `?2`, … |
| Turso | `NumberedQuestionMark` | `?1`, `?2`, … |
| PostgreSQL | `DollarNumber` | `$1`, `$2`, … |
| MySQL | `QuestionMark` | `?`, `?`, … |

Values bind in the order you call `.bind(...)`.

### Binding values

`.bind(value)` infers the database type from common Toasty values: booleans, integers, floats, strings, bytes, UUIDs, decimals, date/time values, non-empty lists.

Use `.bind_typed(value, db_type)` when the type is ambiguous (e.g. `NULL` or an empty list):
```rust
use toasty::schema::db;

toasty::sql::statement("UPDATE users SET archived_at = ?1 WHERE id = ?2")
    .bind_typed(toasty::stmt::Value::Null, db::Type::Timestamp(6))
    .bind(1_i64)
    .exec(&mut db)
    .await?;
```

### Decoding query results

By default, `query(...).exec(...)` infers result value types from database metadata. Some values are ambiguous (SQLite stores booleans as integers and UUIDs as blobs — decoded as `I64` and `Bytes` without hints).

Use `.column_types(...)` to provide Toasty result types:
```rust
use toasty::stmt;

let rows = toasty::sql::query("SELECT id, enabled FROM users WHERE id = ?1")
    .bind(1_i64)
    .column_types([stmt::Type::I64, stmt::Type::Bool])
    .exec(&mut db)
    .await?;
```

Column type hints affect decoding only, not the SQL statement.

### Connections and transactions

Raw SQL uses the same executor interface — pass any `Db`, `Connection`, or `Transaction` handle to `.exec(...)`.

Dedicated connection (for temporary tables or session variables):
```rust
let mut conn = db.connection().await?;
toasty::sql::statement("CREATE TEMP TABLE temp_ids (id INTEGER)")
    .exec(&mut conn).await?;
let rows = toasty::sql::query("SELECT id FROM temp_ids")
    .exec(&mut conn).await?;
```

Transaction (raw SQL commits/rolls back with other Toasty operations):
```rust
let mut tx = db.transaction().await?;
toasty::sql::statement("UPDATE users SET name = ?1 WHERE id = ?2")
    .bind("Alice").bind(1_i64)
    .exec(&mut tx).await?;
User::filter_by_id(1).delete().exec(&mut tx).await?;
tx.commit().await?;
```

Nested transactions work the same way — raw SQL through the nested transaction is part of that savepoint.

## Concurrency control

Optimistic concurrency control (OCC) through `#[version]`. Toasty conditions each write on a version field and atomically increments it, so a stale writer's update fails instead of silently overwriting.

### Enabling OCC

Add `#[version]` to a `u64` field:
```rust
#[derive(Debug, toasty::Model)]
struct Document {
    #[key]
    #[auto]
    id: uuid::Uuid,
    content: String,
    #[version]
    version: u64,
}
```

Toasty manages the field — you declare it but never set it manually.

### Behavior

- **Create:** Toasty sets the version to `1`.
- **Instance update:** `doc.update()...exec()` conditions the write on the current version and atomically increments it. A concurrent writer causes `.exec()` to error.
- **Instance delete:** `doc.delete().exec()` conditions the delete on the current version.
- **Query-based update:** `Document::filter_by_id(id).update()...exec()` atomically increments the version on every matched row (`version = version + 1`) but does NOT condition the write on a prior version — atomic at the database level, may span many rows. Advancing the counter makes a concurrent instance update/delete from a stale snapshot fail its version check instead of overwriting.

```rust
let mut doc = toasty::create!(Document { content: "hello" }).exec(&mut db).await?;
assert_eq!(doc.version, 1);

let mut stale = Document::get_by_id(&mut db, &doc.id).await?; // both at version 1

doc.update().content("world").exec(&mut db).await?; // doc at version 2
assert_eq!(doc.version, 2);

let result = stale.update().content("conflict").exec(&mut db).await; // stale at version 1 — FAILS
assert!(result.is_err());
```

Only instance updates and instance deletes **check** the version and fail on conflict. Query-based updates increment but apply unconditionally.

### Driver support

Works on every driver (DynamoDB, SQLite, PostgreSQL, MySQL). Mechanism varies:
- DynamoDB: conditions the write on the version value in a single request
- PostgreSQL: bundles the check and update into one statement
- SQLite and MySQL: run the check and write inside a transaction, reading the current version (locking the row where supported) before applying

A conflicting write returns `Error::condition_failed`; recover by reloading and retrying. If the record was deleted since loading, returns `Error::record_not_found`.