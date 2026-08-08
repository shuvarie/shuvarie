# CRUD: Create, Query, Update, Upsert, Delete

## Creating records

### Single record

```rust
use toasty::Model;

#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[unique]
    email: String,
}

let user = toasty::create!(User {
    name: "Alice",
    email: "alice@example.com",
})
.exec(&mut db)
.await?;
```

The `create!` macro expands to builder calls and does NOT execute — call `.exec(&mut db)` to insert. Supports field shorthand (`name` instead of `name: name`).

```rust
// Equivalent builder form:
let user = User::create()
    .name("Alice")
    .email("alice@example.com")
    .exec(&mut db)
    .await?;
```

The returned `User` has all fields set, including auto-generated ones like `id`.

### Required vs optional fields

Required fields must be set before `.exec()`. Optional fields (`Option<T>`) default to `NULL`:

```rust
let user = toasty::create!(User { name: "Alice" }) // bio defaults to None
    .exec(&mut db).await?;

let user = toasty::create!(User { name: "Bob", bio: "Likes Rust" })
    .exec(&mut db).await?;
assert_eq!(user.bio.as_deref(), Some("Likes Rust"));
```

### Creating through a relation

Use `in` to scope creation; Toasty sets the foreign key automatically:

```rust
let user = toasty::create!(User { name: "Alice" }).exec(&mut db).await?;
let todo = toasty::create!(in user.todos() { title: "Buy groceries" })
    .exec(&mut db).await?;
assert_eq!(todo.user_id, user.id);
```

### Nested creation

Create a parent and children in one call. Use `{ ... }` for BelongsTo/HasOne, `[{ ... }, { ... }]` for HasMany:

```rust
let user = toasty::create!(User {
    name: "Alice",
    todos: [{ title: "Buy groceries" }, { title: "Write docs" }],
})
.exec(&mut db).await?;
let todos = user.todos().exec(&mut db).await?;
assert_eq!(2, todos.len());
```

Nesting works to arbitrary depth. Toasty makes a best effort for atomicity (depends on database capabilities).

### Creating many records

**Same-type batch** (`Type::[ ... ]` returns `Vec<User>`):

```rust
let users = toasty::create!(User::[
    { name: "Alice", email: "alice@example.com" },
    { name: "Bob", email: "bob@example.com" },
]).exec(&mut db).await?;
```

**Mixed-type batch** (`( ... )` returns a tuple):

```rust
let (user, post) = toasty::create!((
    User { name: "Alice" },
    Post { title: "Hello World" },
)).exec(&mut db).await?;
```

You can mix type-target and scoped forms in the same batch:

```rust
let (user, todo) = toasty::create!((
    User { name: "Carl" },
    in user.todos() { title: "Buy milk" },
)).exec(&mut db).await?;
```

**Dynamic batches** with `toasty::batch()`:

```rust
let mut insertions = vec![];
for name in names {
    insertions.push(toasty::create!(User { name, email: format!("user{i}@example.com") }));
}
let users = toasty::batch(insertions).exec(&mut db).await?;
```

**Bulk creation** with `create_many()`:

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

`create_many()` is single-model; `batch()` can mix models/queries/creates.

### Macro-to-builder reference

| Macro syntax | Builder equivalent |
|--------------|---------------------|
| `toasty::create!(User { name: "Alice" })` | `User::create().name("Alice")` |
| `toasty::create!(in user.todos() { ... })` | `user.todos().create()...` |
| Nested `{ ... }` for BelongsTo/HasOne | `.field(ChildModel::create()...)` |
| Nested `[{ ... }]` for HasMany | `.fields([ChildModel::create()...])` |
| `toasty::create!(User::[{ ... }, { ... }])` | `toasty::batch([User::create()...])` → `Vec<User>` |
| `toasty::create!((User { ... }, Post { ... }))` | `toasty::batch((User::create()..., Post::create()...))` → tuple |

### When to use the builder directly

When you need to conditionally set fields:

```rust
let mut builder = User::create().name("Alice");
if some_condition {
    builder = builder.bio("Likes Rust");
}
let user = builder.exec(&mut db).await?;
```

## Querying records

### Get by primary key

```rust
let user = User::get_by_id(&mut db, &1).await?; // errors if not found
```

Method name matches the key field. Composite keys: `get_by_student_id_and_course_id()`.

### Get all / filter / filter_by_*

```rust
let users: Vec<User> = User::all().exec(&mut db).await?;

let users = User::filter(User::fields().name().eq("Alice"))
    .exec(&mut db).await?;

let user = User::filter_by_email("alice@example.com").get(&mut db).await?;

let users = User::filter_by_country("US").exec(&mut db).await?;
```

`get_by_*` executes immediately and returns the record. `filter_by_*` returns a query builder you can customize before executing.

### Terminal methods

| Method | Returns |
|--------|---------|
| `.exec(&mut db)` | `Vec<T>` — all matching records |
| `.first().exec(&mut db)` | `Option<T>` — first result or `None` |
| `.get(&mut db)` | `T` — exactly one result (errors if 0 or >1) |

### Chaining filters

Each `.filter()` adds an AND condition:

```rust
let users = User::filter_by_name("Alice")
    .filter(User::fields().age().gt(25))
    .exec(&mut db).await?;
```

### Projecting columns with `.select()`

```rust
let names: Vec<String> = User::all()
    .select(User::fields().name())
    .exec(&mut db).await?;

let pairs: Vec<(u64, String)> = User::all()
    .select((User::fields().id(), User::fields().name()))
    .exec(&mut db).await?;

let name: Option<String> = User::filter_by_email("alice@example.com")
    .select(User::fields().name())
    .first()
    .exec(&mut db).await?;
```

`.select()` can also project a multi-step (`via`) relation — `Vec<T>` for `has_many` `via`, or a single optional record for `has_one` `via`. SQL backends only.

### Sorting by most recent

```rust
let recent = Post::all()
    .latest_by(Post::fields().id())
    .limit(10)
    .exec(&mut db).await?;
```

Shorthand for `order_by(field.desc())`. Good for auto-incrementing keys, UUIDv7 keys, or `created_at`.

## Updating records

### Updating an instance

```rust
toasty::update!(user { name: "Alice Smith" })
    .exec(&mut db)
    .await?;
assert_eq!(user.name, "Alice Smith");
```

The instance reflects new values after `.exec()`. Fields not named keep their current values. Supports field shorthand.

### Updating multiple fields

```rust
toasty::update!(user {
    name: "Alice Smith",
    email: "alice.smith@example.com",
}).exec(&mut db).await?;
```

### Modifying `Vec<scalar>` fields

```rust
toasty::update!(article { tags.push("toasty") })
    .exec(&mut db).await?;
```

Lowers to `tags: toasty::stmt::push("toasty")`. Same syntax reaches `extend`, `pop`, `clear`, `remove`.

### Relative numeric updates

| Method | What it does |
|--------|--------------|
| `field.increment()` | Add 1 |
| `field.decrement()` | Subtract 1 |
| `field.add(value)` | Add `value` |
| `field.subtract(value)` | Subtract `value` |

```rust
toasty::update!(account { balance.add(100) }).exec(&mut db).await?;
toasty::update!(account { login_count.increment() }).exec(&mut db).await?;
```

Atomic against the existing column value — folds read and write into one statement. Works on every backend and every primitive numeric type.

### Updating embedded fields

```rust
toasty::update!(doc {
    meta: { version: 2, status: "published" },
}).exec(&mut db).await?;
```

Sub-fields not listed keep their values. Brace blocks nest. To replace wholesale, pass the typed value:

```rust
toasty::update!(doc {
    meta: Metadata { version: 2, status: "published".into() },
}).exec(&mut db).await?;
```

### Inserting has-many children

```rust
toasty::update!(user {
    todos: [{ title: "buy milk" }, { title: "walk dog" }],
}).exec(&mut db).await?;
```

Mix brace-block builders with `stmt::*` values:

```rust
toasty::update!(user {
    todos: [{ title: "new todo" }, toasty::stmt::remove(&old_todo)],
}).exec(&mut db).await?;
```

### Updating by query

```rust
toasty::update!(User::filter_by_id(user_id) { name: "Bob" })
    .exec(&mut db).await?;
```

Scoped queries work too: `user.todos().filter_by_done(false).update()...`.

### Update by indexed field

```rust
User::update_by_id(user_id).name("Bob").exec(&mut db).await?;
User::update_by_email("alice@example.com").name("Alice Smith").exec(&mut db).await?;
```

Shorthand for `User::filter_by_id(user_id).update()`. Generated for each `#[key]`, `#[unique]`, or `#[index]` field.

### Setting an optional field to `None`

```rust
toasty::update!(user { bio: Option::<String>::None })
    .exec(&mut db).await?;
```

### Concurrency control

With a `#[version]` field, instance updates condition the write on the loaded version and increment it atomically. A concurrent writer causes `.exec()` to error. Query-based updates increment the version but don't condition on it.

### Building updates programmatically

```rust
let mut builder = user.update().name("Alice Smith");
if some_condition {
    builder = builder.bio("Likes Rust");
}
builder.exec(&mut db).await?;
```

## Upserting records

An upsert creates a record when its key is absent and updates the matching record when present — one atomic operation.

### Creating or updating by a unique field

```rust
let user = User::upsert_by_email("alice@example.com")
    .name("Alice")
    .login_count(1)
    .exec(&mut db).await?;
```

The conflict-target argument supplies the create value and never changes on update. No `email` setter exists on the builder. Toasty does NOT generate `upsert_by_*` for ordinary `#[index]` (could match multiple records).

Composite unique constraint: `upsert_by_org_id_and_user_id(org_id, user_id)`.

Toasty does NOT provide an unqualified `User::upsert()` — naming the target prevents a new unique constraint from changing semantics.

### Applying one assignment to both branches

A shared mutation (`increment`, `subtract`, `push`) reads the existing value on update. Add `#[default]` to define what the mutation reads on create:

```rust
#[derive(Debug, toasty::Model)]
struct Counter {
    #[key]
    name: String,
    #[default(0)]
    count: i64,
}

let counter = Counter::upsert_by_name("requests")
    .count(toasty::stmt::increment())
    .exec(&mut db).await?;
// Inserts count=1 (increment applied to default 0); on conflict increments stored value.
```

A shared mutation on a field without `#[default]` returns `invalid_statement`. Replacement assignments (`set`, `clear`) don't read an existing value and don't require `#[default]`.

### Setting different values on create and update

```rust
let user = User::upsert_by_email("alice@example.com")
    .on_create(|user| {
        user.name("Alice").login_count(0)
    })
    .on_update(|user| {
        user.login_count(toasty::stmt::increment())
    })
    .exec(&mut db).await?;
```

`on_update` accepts the same assignment operators as a normal update. Its `incoming()` method references values proposed by the create branch; model field paths reference stored values:

```rust
User::upsert_by_email(email)
    .name(proposed_name)
    .login_count(1)
    .on_update(|user| {
        let incoming = user.incoming();
        user.name(incoming.name()).login_count(toasty::stmt::increment())
    })
    .exec(&mut db).await?;
```

### Inserting or ignoring a conflict

```rust
let inserted: Option<User> = User::upsert_by_email("alice@example.com")
    .name("Alice")
    .or_ignore()
    .exec(&mut db)
    .await?;
```

`Some(user)` when created, `None` when the target conflicts. Only suppresses that target's conflict; FK failures, missing required values, or conflicts on a different unique constraint remain errors.

### Defaults and omitted fields

| Value source | Create branch | Update branch |
|--------------|---------------|---------------|
| Conflict-target argument | Applied | Unchanged |
| Ordinary replacement setter | Applied | Applied |
| Ordinary mutation | Applied to `#[default]` | Applied to stored value |
| `on_create` setter | Applied | Unchanged |
| `on_update` setter | Omitted | Applied |
| `#[default]` | Applied | Unchanged |
| `#[update]` | Applied | Applied |

Omitted fields use normal create behavior on insert; unchanged on update. The create branch must supply every required field with no automatic value or default. A regular upsert must contain at least one update assignment — use `or_ignore()` for a no-change conflict branch.

### Database support

| Backend | Primary key | Unique constraint | `on_create`/`on_update` | `or_ignore` |
|---------|-------------|-------------------|-------------------------|-------------|
| PostgreSQL | Yes | Yes | Yes | Yes |
| SQLite | Yes | Yes | Yes | Yes |
| Turso | Yes | Yes | Yes | Yes |
| DynamoDB | Yes | No | Required fields / No | Yes |
| MySQL | No | No | No | No |

DynamoDB: regular primary-key upsert with one `UpdateItem`; create-only assignment on a required field lowers to `if_not_exists`; shared arithmetic/append uses `#[default]` as the `if_not_exists` operand. Nullable create-only assignments and all update-only assignments return `unsupported_feature`. DynamoDB rejects regular upserts that assign a Toasty-managed `#[unique]` field; `or_ignore()` can initialize unique fields (only a create branch).

MySQL: `ON DUPLICATE KEY UPDATE` reacts to any unique conflict, not the named target — Toasty returns `unsupported_feature`.

Upsert handles one record at a time; no relation setters or nested-create builders.

## Deleting records

### Deleting an instance

```rust
user.delete().exec(&mut db).await?; // consumes self
```

With a `#[version]` field, instance deletes are version-guarded (errors if a concurrent writer modified the record).

### Deleting by primary key

```rust
User::delete_by_id(&mut db, user.id).await?; // no SELECT first
```

### Deleting by query

```rust
User::filter_by_email("alice@example.com").delete().exec(&mut db).await?;
User::all().delete().exec(&mut db).await?;
```

Any query builder's `.delete()` converts it into a delete operation.