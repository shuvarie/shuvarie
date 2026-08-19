# Database Setup, Migrations, and Schema Management

## Opening a database

Two steps: register your models, then connect. `Db::builder()` handles both:

```rust
let mut db = toasty::Db::builder()
    .models(toasty::models!(User, Post))
    .connect("sqlite::memory:")
    .await?;
```

## Registering models

The `models!` macro builds a `ModelSet` — the collection of model definitions Toasty uses to generate the schema. Three forms, combinable:

```rust
toasty::models!(
    crate::*,              // all models from the current crate
    third_party_models::*, // all models from an external crate
    User,                  // individual model
    other_module::Post,
)
```

`crate::*` finds all `#[derive(Model)]` and `#[derive(Embed)]` types in your crate at compile time. You don't need to list every model — registering a model also registers models reachable through its fields (`BelongsTo`, `HasMany`, `HasOne`, embedded types).

## Connection URLs

| Scheme | Database | Feature flag |
|--------|----------|--------------|
| `sqlite` | SQLite | `sqlite` |
| `turso` | Turso (SQLite-compatible, async-native) | `turso` |
| `postgresql` or `postgres` | PostgreSQL | `postgresql` |
| `mysql` | MySQL | `mysql` |
| `dynamodb` | DynamoDB | `dynamodb` |

```rust
.connect("sqlite::memory:")                       // in-memory SQLite
.connect("sqlite:./path/to/db.sqlite")             // SQLite file
.connect("postgresql://user:pass@localhost:5432/mydb")
.connect("mysql://user:pass@localhost:3306/mydb")
.connect("dynamodb://us-east-1")                   // uses AWS config from environment
```

## Using a driver directly

```rust
let driver = toasty_driver_sqlite::Sqlite::in_memory();
let mut db = toasty::Db::builder()
    .models(toasty::models!(User))
    .build(driver)
    .await?;
```

## MySQL TLS (v0.10 uses SQLx)

Since 0.10 the MySQL driver is built on SQLx and defaults to rustls. The old `mysql_async` URL options (`require_ssl`, `verify_ca`, `verify_identity`) are ignored — leaving them can allow `ssl-mode=preferred` to fall back to plaintext. Use SQLx options instead:

```
# v0.9 (ignored in v0.10)
mysql://app:secret@db.internal/store?require_ssl=true&verify_identity=true

# v0.10
mysql://app:secret@db.internal/store?ssl-mode=verify_identity
```

To keep native TLS, disable default features and enable `mysql` plus `native-tls`.

## Connection pool

`Db` owns a connection pool. Each query checks out a connection, returns it when finished.

```rust
use std::time::Duration;

let mut db = toasty::Db::builder()
    .models(toasty::models!(crate::*))
    .max_pool_size(32)
    .pool_wait_timeout(Some(Duration::from_secs(5)))
    .pool_create_timeout(Some(Duration::from_secs(10)))
    .connect("postgresql://user:pass@localhost/mydb")
    .await?;
```

| Builder method | Default | Purpose |
|----------------|---------|---------|
| `max_pool_size(n)` | `num_cpus * 2` | Cap on simultaneous open connections (drivers may enforce lower, e.g. in-memory SQLite is single-connection) |
| `pool_wait_timeout(Some(d))` | `None` | Max time `Db` waits for a free connection before erroring. `None` waits indefinitely |
| `pool_create_timeout(Some(d))` | `None` | Max time to spend opening a new connection |
| `pool_health_check_interval(Some(d))` | `Some(60s)` | How often the background sweep pings an idle connection. `None` disables |
| `pool_pre_ping(true)` | `false` | Ping every connection before handing it out (adds one round-trip per checkout) |

### Recovering from a backend restart

The pool handles silently-broken backends two ways:
- **Background sweep:** every `pool_health_check_interval`, pings one idle connection. If it fails, drops it and eagerly pings the rest so a single bad result drains every dead connection in one pass.
- **Reactive sweep:** when a user query observes a connection-lost error, the same eager sweep runs immediately. A backend restart typically costs one failed user query rather than one per pooled connection.

Enable `pool_pre_ping(true)` if even one failed query is unacceptable. Cost is one extra round-trip per checkout.

## Table name prefix

```rust
let mut db = toasty::Db::builder()
    .models(toasty::models!(crate::*))
    .table_name_prefix("myapp_")
    .connect("sqlite::memory:")
    .await?;
```

Useful when multiple services share a database.

## Schema management

### Quick setup with `push_schema`

```rust
db.push_schema().await?;
```

Issues `CREATE TABLE` and `CREATE INDEX` directly. Good for prototyping and tests. Does NOT track changes between runs — pushes the full schema every time. For a database with data, use migrations.

### The migration system

Compares current model definitions against a stored snapshot, computes the diff, generates a SQL migration file with only the changes.

Migrations are managed through a small CLI binary you create in your project using the `toasty-cli` library crate. Toasty cannot ship a ready-made CLI because the tool needs access to your model types to compute the schema.

| Command | What it does |
|---------|--------------|
| `migration generate` | Diffs current schema vs last snapshot, writes a SQL migration file |
| `migration apply` | Runs pending migrations against the database |
| `migration snapshot` | Prints the current schema as TOML |
| `migration drop` | Removes a migration from history and deletes its files |
| `migration reset` | Drops all tables and optionally re-applies all migrations |

### Setting up the CLI

```toml
[dependencies]
toasty = { version = "0.10", features = ["sqlite"] }
toasty-cli = "0.10"
tokio = { version = "1", features = ["full"] }
anyhow = "1"
```

Create a CLI binary in `src/bin/cli.rs`:
```rust
use toasty_cli::{Config, ToastyCli};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = Config::load()?;

    let db = toasty::Db::builder()
        .models(toasty::models!(crate::*))
        .connect("sqlite:./my_app.db")
        .await?;

    let cli = ToastyCli::with_config(db, config);
    cli.parse_and_run().await?;

    Ok(())
}
```

Add a `Toasty.toml` in your project root:
```toml
[migration]
path = "toasty"
prefix_style = "Sequential"
checksums = false
statement_breakpoints = true
```

### Configuration options

| Option | Default | Description |
|--------|---------|-------------|
| `path` | `"toasty"` | Base directory for migration files, snapshots, history |
| `prefix_style` | `"Sequential"` | `Sequential` (0001_, 0002_) or `Timestamp` (20240112_153045_) |
| `checksums` | `false` | When true, stores MD5 checksums in history to detect modified migration files |
| `statement_breakpoints` | `true` | Adds `-- #[toasty::breakpoint]` comments between SQL statements so drivers can split them |

### Generating a migration

```bash
cargo run --bin my-cli -- migration generate
cargo run --bin my-cli -- migration generate --name add_posts_table
```

Creates three things inside the configured `path` directory:
```
toasty/
├── history.toml
├── migrations/
│   └── 0000_migration.sql       # SQL DDL for this migration
└── snapshots/
    └── 0000_snapshot.toml       # full schema snapshot at this point
```

The next `generate` diffs against the latest snapshot.

### Rename detection

When the diff contains a dropped table and an added table (or dropped/added columns), the CLI asks whether this is a rename. Choosing rename generates `ALTER TABLE ... RENAME` instead of `DROP TABLE` + `CREATE TABLE`.

### Applying migrations

```bash
cargo run --bin my-cli -- migration apply
```

Reads `history.toml`, queries the `__toasty_migrations` tracking table, executes each pending migration in order inside a transaction, records it. If all are already applied, prints a message and exits.

### Embedding migrations in the application binary

(v0.10) Single-binary apps can compile the generated migrations in — no CLI at runtime. Requires the `migration` feature. `embed_migrations!` validates `history.toml` and its referenced SQL files at compile time (compile error on invalid history, duplicate IDs/names, or missing SQL files), then packages them in the binary.

```rust
static MIGRATIONS: toasty::migration::MigrationSet = toasty::embed_migrations!();

async fn migrate(db: &toasty::Db) -> toasty::Result<()> {
    let report = MIGRATIONS.apply(db).await?;
    println!("applied {}, skipped {}", report.applied(), report.skipped());
    Ok(())
}
```

- Default path is the `toasty/` migration directory; pass a path relative to `Cargo.toml` when elsewhere: `toasty::embed_migrations!("migrations/primary")`.
- Snapshot files are NOT embedded (not needed to apply); only `history.toml` and the named `migrations/*.sql` files.
- `MigrationSet::apply(&db)` checks the `__toasty_migrations` table, applies pending migrations in order, and returns a `MigrationReport` with `applied()` / `skipped()` counts.
- One set per database; the application decides which set applies to which `Db`.

This is what `shuvarie-db` does (`crates/db/src/store.rs`): a static `MIGRATIONS` applied in `Store::open` so the schema upgrades automatically on startup.

### Inspecting the current schema

```bash
cargo run --bin my-cli -- migration snapshot
```

Outputs the full schema as TOML. Does not modify files.

### Dropping a migration

```bash
cargo run --bin my-cli -- migration drop --name 0001_add_posts_table.sql
cargo run --bin my-cli -- migration drop --latest
cargo run --bin my-cli -- migration drop  # interactive picker
```

Removes the SQL file, snapshot file, and `history.toml` entry. Does NOT undo applied changes — use `migration reset` and re-apply.

### Resetting the database

```bash
cargo run --bin my-cli -- migration reset
cargo run --bin my-cli -- migration reset --skip-migrations
```

Drops all tables, optionally re-applies every migration in history. Prompts for confirmation.

### Generated SQL

Database-specific DDL. Example for SQLite:
```sql
CREATE TABLE "users" (
    "id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "email" TEXT NOT NULL,
    PRIMARY KEY ("id")
);
-- #[toasty::breakpoint]
CREATE UNIQUE INDEX "index_users_by_email" ON "users" ("email");
```

`-- #[toasty::breakpoint]` comments mark boundaries for drivers to split statements. Some databases (PostgreSQL) can execute multiple statements in a batch; others require one at a time.

### Migration tracking

Toasty tracks applied migrations in a `__toasty_migrations` table it creates automatically. Each row stores the migration's ID (random 64-bit integer from `history.toml`), name, and timestamp.

### Typical workflow

1. Edit your model structs (add a field, change a type, add an index)
2. `migration generate --name describe_change`
3. Review the generated SQL file
4. `migration apply`
5. Commit the migration files, snapshot, and updated history alongside your code

For early development when the schema changes frequently, `push_schema` is simpler. Switch to migrations when your database has data you want to preserve.