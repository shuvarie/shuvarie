---
name: toasty
description: >-
  Toasty is an async ORM for Rust supporting SQL databases (SQLite, Turso,
  PostgreSQL, MySQL) and NoSQL (DynamoDB). You define models as Rust structs
  with `#[derive(toasty::Model)]` and Toasty infers the schema, generates query
  builders, create/update/upsert/delete builders, and relationship accessors at
  compile time. This skill covers model definition, CRUD, relationships,
  filtering expressions, pagination, embedded types, document/JSON fields,
  Vec<scalar> collections, batch operations, transactions, migrations (CLI +
  embedded), and concurrency control. Docs: guide at
  https://tokio-rs.github.io/toasty/0.10.0/guide/introduction.html and API docs
  at https://docs.rs/toasty/latest/toasty/.
license: MIT
metadata:
  author: shuvarie
  version: 0.10.0
  category: Backend Development
  tags:
    - rust
    - orm
    - async
    - database
    - sqlite
    - postgresql
    - mysql
    - dynamodb
    - turso
---

# Toasty — Async ORM for Rust

Toasty is an async ORM for Rust. You define models as Rust structs annotated with `#[derive(toasty::Model)]`. Toasty infers the database schema from your annotated structs — field types map to column types, and attributes like `#[key]`, `#[unique]`, and `#[index]` control the schema. The derive macro generates query builders, create/update/upsert builders, and relationship accessors at compile time.

Supported databases (each behind a feature flag): SQLite (`sqlite`), Turso (`turso`), PostgreSQL (`postgresql`), MySQL (`mysql`), DynamoDB (`dynamodb`). Additional features: `jiff` (date/time), `rust_decimal`, `bigdecimal`, `serde` (JSON), `net` (IP/MAC address types via `cidr` and `macaddr`), `migration` (embedded migrations via `embed_migrations!` + the `toasty::migration` module), `rustls`/`native-tls` (MySQL TLS; `rustls` is the default).

Official docs: **Guide** at https://tokio-rs.github.io/toasty/0.10.0/guide/introduction.html and **API docs** at https://docs.rs/toasty/latest/toasty/.

## Critical Rules

Before writing Toasty code, know these constraints:

- **No eager-load cycles** — Toasty rejects schemas where eager relations recurse (e.g. `User.posts: Vec<Post>` + `Post.user: User`). Wrap at least one side in `Deferred<_>`.
- **`#[auto]` on non-key fields only matches `created_at`/`updated_at`** with `jiff::Timestamp`; otherwise use `#[default(...)]` / `#[update(...)]` explicitly.
- **MySQL upserts unsupported** — Toasty returns `unsupported_feature` for `upsert_by_*` on MySQL (its `ON DUPLICATE KEY UPDATE` reacts to any unique conflict, not the named target).
- **MySQL 0.10 uses SQLx + rustls** — the old `mysql_async` URL options (`require_ssl`, `verify_ca`, `verify_identity`) are ignored; use SQLx options like `?ssl-mode=verify_identity`. Keep native TLS with `native-tls` feature.
- **DynamoDB: no composite unique constraints** — multi-column `#[unique(...)]` returns `unsupported_feature`; single-column `#[unique]` works. `via` relations, `.any()`/`.all()` on associations, and preloading/projecting `via` are SQL-only.
- **`pop`/`remove`/`remove_at` on `Vec<scalar>` require PostgreSQL** — other drivers return an error.
- **`.like()` is SQL-only** (panics on DynamoDB); `.ilike()` is PostgreSQL-only.
- **`#[document]` rejects enums, tuple structs, `Vec<u8>`, relations, `jiff::Zoned`, and `#[column]` renames inside the document.** `#[index]`/`#[unique]`/`#[column]` cannot be placed on the `#[document]` field. Optional document roots unsupported; optional fields inside are fine.
- **Raw SQL is SQL-only** — DynamoDB returns `unsupported_feature`. Use placeholders reported by `db.capability().sql_placeholder` (e.g. `?1` for SQLite/Turso, `$1` for PostgreSQL, `?` for MySQL).
- **Transactions are SQL-only.** Prefer `toasty::batch()` for atomic multi-op when you don't need read-then-branch.
- **`#[auto]` UUID default is v7** (time-ordered) — better for indexes than v4.
- **Embedded relations** — `#[belongs_to]` works inside embedded structs/enums (0.10), with normal key/references inference, but must be `Deferred` — eager loading via `.include()` is NOT supported there.
- **Ordering operators are newtype-only** — `ne`/`gt`/`ge`/`lt`/`le`/`asc`/`desc` exist on newtype embeds (comparing the wrapped column); multi-field embeds support only `eq`/`ne`. No more `._0()` descent for comparisons.

## Installation

```toml
[dependencies]
toasty = { version = "0.10", features = ["sqlite"] }
tokio = { version = "1", features = ["full"] }
# Optional: jiff for timestamps, serde for JSON, net for IP/MAC types, migration for embed_migrations!
# toasty = { version = "0.10", features = ["sqlite", "jiff", "serde"] }
```

Swap the feature flag for your backend: `sqlite`, `turso`, `postgresql`, `mysql`, `dynamodb`.

## Quick Start

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

#[tokio::main]
async fn main() -> toasty::Result<()> {
    let mut db = toasty::Db::builder()
        .models(toasty::models!(crate::*))
        .connect("sqlite::memory:")
        .await?;

    db.push_schema().await?;

    let user = toasty::create!(User {
        name: "Alice",
        email: "alice@example.com",
    })
    .exec(&mut db)
    .await?;

    let found = User::get_by_id(&mut db, &user.id).await?;
    println!("Found: {:?}", found.email);
    Ok(())
}
```

## Feature Decision Tree

Use this to decide which reference file to load:

**Need to define models, keys, auto-generation, or composite keys?**
→ Read `references/models-and-keys.md`

**Need CRUD (create / query / update / upsert / delete)?**
→ Read `references/crud.md`

**Need relationships (BelongsTo / HasMany / HasOne / many-to-many / preloading)?**
→ Read `references/relationships.md`

**Need filter expressions, sorting, limits, offset, or cursor pagination?**
→ Read `references/querying.md`

**Need embedded types, `#[document]`, JSON encoding (`Json<T>` / `serde_json::Value`), or `Vec<scalar>` fields?**
→ Read `references/fields-advanced.md`

**Need batch operations, transactions (nested/savepoints), raw SQL, or concurrency control (`#[version]`)?**
→ Read `references/transactions-and-advanced.md`

**Need database setup, connection URLs, connection pool, migrations (CLI or embedded `embed_migrations!`), or schema management?**
→ Read `references/database-setup.md`

## Generated Methods Cheat Sheet

For a `User` model with `#[key]` on `id`, `#[unique]` on `email`, `#[index]` on `country`:

| Method | Returns | Description |
|--------|--------|-------------|
| `toasty::create!(User { ... })` | create builder | Insert (call `.exec(&mut db).await?`) |
| `User::create()` | create builder | Builder form |
| `User::create_many()` | bulk builder | Insert many of same model |
| `User::get_by_id(&mut db, &id)` | `Result<User>` | Fetch by primary key (immediate) |
| `User::get_by_email(&mut db, email)` | `Result<User>` | Fetch by unique field (immediate) |
| `User::all()` | query builder | All records |
| `User::filter(expr)` | query builder | Filter by expression |
| `User::filter_by_id(id)` | query builder | Filter by key |
| `User::filter_by_email(email)` | query builder | Filter by unique field |
| `User::filter_by_country(country)` | query builder | Filter by indexed field |
| `User::update_by_id(id)` | update builder | Update by key |
| `User::upsert_by_email(email)` | upsert builder | Create or update by unique field |
| `User::delete_by_id(&mut db, id)` | `Result<()>` | Delete by key (immediate) |
| `user.update()` | update builder | Instance update (mutates self) |
| `user.delete()` | delete builder | Instance delete (consumes self) |
| `User::fields()` | field accessors | Build filter expressions |

Query builder terminal methods: `.exec(&mut db)` → `Vec<T>`, `.first().exec(&mut db)` → `Option<T>`, `.get(&mut db)` → `T` (exactly one).

## Attribute Summary

| Attribute | Purpose |
|-----------|---------|
| `#[derive(toasty::Model)]` | Mark a struct as a database-backed model |
| `#[derive(toasty::Embed)]` | Mark a struct/enum as embedded (flattened into parent table) |
| `#[key]` / `#[key(...)]` | Primary key (field-level or struct-level for composite) |
| `#[auto]` / `#[auto(uuid(v7))]` / `#[auto(increment)]` | Auto-generate value on insert |
| `#[unique]` / `#[unique(...)]` | Unique constraint (single or composite) |
| `#[index]` / `#[index(...)]` | Non-unique index (single or composite) |
| `#[column("name")]` | Custom column name |
| `#[column(type = ...)]` | Explicit column type |
| `#[default(expr)]` | Default value on create |
| `#[update(expr)]` | Auto value on create and update |
| `#[version]` | Optimistic concurrency control (u64) |
| `#[belongs_to(key = ..., references = ...)]` | Foreign key relation (child side) |
| `#[has_many]` / `#[has_many(via = ...)]` / `#[has_many(pair = ...)]` | One-to-many relation (parent side) |
| `#[has_one]` / `#[has_one(via = ...)]` | One-to-one relation (parent side) |
| `#[document]` | Store embedded struct as one structured column |
| `#[table = "name"]` | Override table name |
| `#[column(rename_all = "...")]` | Enum variant naming rule |
| `#[column(variant = ...)]` | Explicit enum variant label |

## Complete File Index

| File | Description |
|------|-------------|
| `SKILL.md` | Main entry point — decision tree, cheat sheet, quick start |
| `references/models-and-keys.md` | Defining models, supported types, optional fields, table names, keys (single/composite/partition-local), auto-generation, newtype keys |
| `references/crud.md` | Creating (single/nested/batch), querying (get/filter/select/latest_by), updating (instance/query/relative/embedded), upserting (on_create/on_update/or_ignore), deleting |
| `references/relationships.md` | BelongsTo, HasMany, HasOne, many-to-many (join model + via), preloading (.include/.get/.try_get), data consistency on delete, composite foreign keys |
| `references/querying.md` | Filter expressions (eq/ne/gt/lt/in_list/is_none/is_some/starts_with/like/ilike/and/or/not), association filters (any/all), sorting, limit/offset, cursor pagination (Page) |
| `references/fields-advanced.md` | Field options (column name/type/default/update), timestamps with #[auto], embedded types (newtype/struct/enum), #[document] fields, JSON encoding (Json<T>/serde_json::Value), Vec<scalar> fields (predicates + incremental mutations), deferred fields |
| `references/transactions-and-advanced.md` | Batch operations (toasty::batch / create_many), transactions (nested/savepoints/options), raw SQL (statement/query/placeholders), concurrency control (#[version]) |
| `references/database-setup.md` | Db::builder, registering models, connection URLs, driver feature flags, connection pool tuning, table name prefix, push_schema vs migration system, toasty-cli workflow, embedded migrations (embed_migrations!) |