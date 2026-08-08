# Relationships

Relationships connect models. Toasty supports BelongsTo, HasMany, HasOne, and many-to-many (a pattern composed from two direct relations + a join model).

## Relationship types

| Type | Foreign key on | Parent has | Child has | Example |
|------|----------------|-----------|-----------|---------|
| BelongsTo | This model | — | One parent | A post belongs to a user |
| HasMany | Other model | Many children | — | A user has many posts |
| HasOne | Other model | One child | — | A user has one profile |
| Many-to-Many | Join model (both endpoints) | — | — | Users join groups |

### Which model gets which attribute?

The model whose table **contains the FK column** declares `#[belongs_to]`. The other side declares `#[has_many]` or `#[has_one]`.

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[has_many]
    posts: toasty::Deferred<Vec<Post>>, // User's table has no FK
}

#[derive(Debug, toasty::Model)]
struct Post {
    #[key]
    #[auto]
    id: u64,
    #[index]
    user_id: u64, // Post's table has the FK
    #[belongs_to(key = user_id, references = id)]
    user: toasty::Deferred<User>,
    title: String,
}
```

### Lazy and eager relation fields

| Attribute | Lazy field type | Eager field type |
|-----------|-----------------|------------------|
| `#[has_many]` | `Deferred<Vec<T>>` | `Vec<T>` |
| `#[has_one]` | `Deferred<T>` or `Deferred<Option<T>>` | `T` or `Option<T>` |
| `#[belongs_to]` | `Deferred<T>` or `Deferred<Option<T>>` | `T` or `Option<T>` |

Eager relations load with every query (implicit `.include()`). Eager relations cannot form cycles — Toasty rejects schemas where eager loading would recurse forever. Wrap at least one side in `Deferred<_>`.

### Required vs optional relationships

The nullability of the FK field controls whether the relationship is required.

**Required** (`user_id: u64`): `NOT NULL`, every post must have a user.

**Optional** (`user_id: Option<u64>` with `user: Deferred<Option<User>>`): allows NULL, post can exist without a user.

### Data consistency on delete and unlink

| Action | FK required (`u64`) | FK optional (`Option<u64>`) |
|--------|---------------------|------------------------------|
| Delete parent | Child is **deleted** | Child stays, FK set to `NULL` |
| Unset relation (e.g. `update().profile(None)`) | Child is **deleted** | Child stays, FK set to `NULL` |
| Delete child | Parent unaffected | Parent unaffected |

Applied at the application level by Toasty's query engine, not database-level FK constraints.

### Relationship pairs

Toasty matches `#[has_many]`/`#[has_one]` to `#[belongs_to]` by model types (field names don't matter). If ambiguous (two `BelongsTo` to the same parent type), use `pair`:

```rust
#[has_many(pair = owner)]
posts: toasty::Deferred<Vec<Post>>,
```

One-sided relationships (only `#[belongs_to]` on child, no `#[has_many]` on parent) are allowed. The reverse (a `#[has_many]` without a matching `#[belongs_to]`) is NOT allowed — Toasty needs the FK definition.

### Composite foreign keys

When the parent has a composite PK, pass arrays to `key` and `references`:

```rust
#[derive(Debug, toasty::Model)]
#[key(org_id, id)]
struct Team {
    org_id: u64,
    id: u64,
    #[has_many]
    members: toasty::Deferred<Vec<Member>>,
}

#[derive(Debug, toasty::Model)]
#[index(org_id, team_id)] // FK fields need a model-level composite index
struct Member {
    #[key]
    #[auto]
    id: u64,
    org_id: u64,
    team_id: u64,
    #[belongs_to(key = [org_id, team_id], references = [org_id, id])]
    team: toasty::Deferred<Team>,
}
```

The FK fields need a model-level composite index covering them in order. Two single-column indexes don't compose.

## BelongsTo

A BelongsTo connects a child to a parent through a FK on the child.

### Inferring the foreign key

`#[belongs_to]` names `key` (FK field on child) and `references` (field on parent). Both inferred when omitted:
- `key` defaults to `<field>_id` (e.g. `user_id` for `user`)
- `references` defaults to `id`

```rust
#[belongs_to(key = owner_id)]
owner: toasty::Deferred<User>,

#[belongs_to(key = [user_id, user_revision], references = [id, revision])]
user: toasty::Deferred<User>,
```

The FK field should have `#[index]` for efficient lookups. For eager loading, omit `Deferred`:

```rust
#[belongs_to]
user: User, // implicit .include(Post::fields().user())
```

### Optional BelongsTo

```rust
#[index]
user_id: Option<u64>,
#[belongs_to]
user: toasty::Deferred<Option<User>>,
// eager:
#[belongs_to]
user: Option<User>,
```

### Accessing the related record

```rust
let user = post.user().exec(&mut db).await?; // Result<User>
// optional:
match post.user().exec(&mut db).await? {
    Some(user) => println!("Author: {}", user.name),
    None => println!("No author"),
}
```

Each call executes a query. Use preloading to avoid repeated queries.

### Setting the relation on create

By parent reference (Toasty extracts the PK and sets the FK):
```rust
let post = toasty::create!(Post { title: "Hello World", user: &user })
    .exec(&mut db).await?;
assert_eq!(post.user_id, user.id);
```

By FK value:
```rust
let post = toasty::create!(Post { title: "Hello World", user_id: user.id })
    .exec(&mut db).await?;
```

## HasMany

A HasMany connects a parent to multiple children. The child has the FK.

### Defining

```rust
#[has_many]
posts: toasty::Deferred<Vec<Post>>,
// eager:
#[has_many]
posts: Vec<Post>,
```

`#[has_many]` adds no columns to the parent's table. The relationship is stored entirely in the child's FK column.

### Querying children

```rust
let posts: Vec<Post> = user.posts().exec(&mut db).await?;
// SELECT * FROM posts WHERE user_id = ?
```

All queries through the relation accessor are scoped to the parent.

### Creating through the relation

```rust
let post = toasty::create!(in user.posts() { title: "Hello World" })
    .exec(&mut db).await?;
assert_eq!(post.user_id, user.id);
```

### Nested creation

```rust
let user = toasty::create!(User {
    name: "Alice",
    posts: [{ title: "First post" }, { title: "Second post" }],
}).exec(&mut db).await?;
let posts = user.posts().exec(&mut db).await?;
assert_eq!(2, posts.len());
```

### Inserting and removing children

```rust
user.posts().insert(&mut db, &post).await?; // updates child's FK
user.posts().insert(&mut db, &[post1, post2, post3]).await?;

user.posts().remove(&mut db, &post).await?;
// required FK (u64): deletes the child
// optional FK (Option<u64>): sets FK to NULL
```

If the child is already associated with a different parent, `.insert()` moves it.

### Scoped queries

```rust
let drafts = user.posts()
    .filter(Post::fields().published().eq(false))
    .exec(&mut db).await?;

let post = user.posts().get_by_id(&mut db, &post_id).await?; // errors if belongs to another user

user.posts().filter_by_id(post_id).update().title("New title").exec(&mut db).await?;

user.posts().filter_by_id(post_id).delete().exec(&mut db).await?;
```

### Filtering parents by children

```rust
let users = User::filter(
    User::fields().posts().any(Post::fields().published().eq(true))
).exec(&mut db).await?;
```

### Multi-step relations (`via`)

A HasMany can reach its target through a path of existing relations:

```rust
#[has_many(via = comments.article)]
commented_articles: toasty::Deferred<Vec<Article>>,
```

Read left-to-right from this model. A `via` relation:
- owns no FK (derived from the relations it traverses) — needs no `pair`
- yields **distinct targets** (an article commented on twice appears once)
- is **read-only** (no `create`/`insert`/`remove` — mutate underlying relations directly)
- is preloadable with `.include()` and projectable with `.select()` (SQL backends only)

```rust
let articles = user.commented_articles().exec(&mut db).await?;
let recent = user.commented_articles()
    .filter(Article::fields().title().eq("Rust"))
    .exec(&mut db).await?;
```

## HasOne

A HasOne connects a parent to a single child. The child has the FK, which must be `#[unique]` (so each parent maps to at most one child).

### Defining

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[has_one]
    profile: toasty::Deferred<Option<Profile>>,
}

#[derive(Debug, toasty::Model)]
struct Profile {
    #[key]
    #[auto]
    id: u64,
    #[unique] // guarantees at most one profile per user
    user_id: Option<u64>,
    #[belongs_to(key = user_id, references = id)]
    user: toasty::Deferred<Option<User>>,
    bio: String,
}
```

```sql
CREATE UNIQUE INDEX idx_profiles_user_id ON profiles (user_id);
```

### Optional vs required

- `Deferred<Option<Profile>>` / `Option<Profile>`: parent may or may not have a child.
- `Deferred<Profile>` / `Profile`: parent must have a child (create requires providing it).

### Accessing

```rust
let profile = user.profile().exec(&mut db).await?; // Option<Profile>
// required:
let profile = user.profile().exec(&mut db).await?; // Profile (not wrapped)
```

### Creating through the relation

```rust
let profile = toasty::create!(in user.profile() { bio: "A person" })
    .exec(&mut db).await?;
assert_eq!(profile.user_id, Some(user.id));

// or together:
let user = toasty::create!(User { name: "Alice", profile: { bio: "A person" } })
    .exec(&mut db).await?;
```

### Updating the relation

Replace with a new child:
```rust
user.update().profile(toasty::create!(Profile { bio: "New bio" }))
    .exec(&mut db).await?;
```

Associate an existing child:
```rust
User::filter_by_id(user.id).update().profile(&profile).exec(&mut db).await?;
```

Unset (optional HasOne):
```rust
user.update().profile(None).exec(&mut db).await?;
// required FK: deletes the child; optional FK: sets FK to NULL
```

### Multi-step `via`

```rust
#[has_one(via = account.subscription)]
subscription: toasty::Deferred<Option<Subscription>>,
```

Same rules as HasMany `via` (distinct, read-only, SQL-only preloading/projecting).

## Many-to-Many

A many-to-many connects multiple records on each side through a join model with two BelongsTo relations. Endpoint models each declare a direct HasMany to the join model and a derived `has_many(via = ...)` for read-only traversal.

### Defining the join model

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[has_many]
    memberships: toasty::Deferred<Vec<Membership>>,
    #[has_many(via = memberships.group)]
    groups: toasty::Deferred<Vec<Group>>,
}

#[derive(Debug, toasty::Model)]
struct Group {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[has_many]
    memberships: toasty::Deferred<Vec<Membership>>,
    #[has_many(via = memberships.user)]
    users: toasty::Deferred<Vec<User>>,
}

#[derive(Debug, toasty::Model)]
#[key(user_id, group_id)] // one membership per user-group pair
struct Membership {
    #[index]
    user_id: u64,
    #[belongs_to(key = user_id, references = id)]
    user: toasty::Deferred<User>,
    #[index]
    group_id: u64,
    #[belongs_to(key = group_id, references = id)]
    group: toasty::Deferred<Group>,
    role: String, // data about the connection
}
```

### Creating a link

```rust
let membership = toasty::create!(Membership {
    user: &user,
    group: &group,
    role: "member",
}).exec(&mut db).await?;
```

The `user` and `group` setters fill `user_id` and `group_id`. The derived `user.groups()` is read-only — create/delete `Membership` records to change links.

### Querying both directions

```rust
let groups: Vec<Group> = user.groups().exec(&mut db).await?;
let users: Vec<User> = group.users().exec(&mut db).await?;

let rust_groups = user.groups()
    .filter(Group::fields().name().eq("Rust"))
    .order_by(Group::fields().name().asc())
    .exec(&mut db).await?;
```

A derived `via` relation returns distinct targets.

### Filtering endpoints

By opposite endpoint (use derived relation):
```rust
let rust_users = User::filter(
    User::fields().groups().any(Group::fields().name().eq("Rust")),
).exec(&mut db).await?;
```

By join metadata (use direct join-model relation):
```rust
let owners = User::filter(
    User::fields().memberships().any(Membership::fields().role().eq("owner")),
).exec(&mut db).await?;

let rust_users = User::filter(
    User::fields().memberships().any(Membership::fields().group().name().eq("Rust")),
).exec(&mut db).await?;
```

### Preloading endpoints

```rust
let users = User::all().include(User::fields().groups()).exec(&mut db).await?;
for user in &users {
    let groups: &[Group] = user.groups.get();
    println!("{} belongs to {} groups", user.name, groups.len());
}
```

### Updating and removing a link

```rust
membership.update().role("owner").exec(&mut db).await?;
membership.delete().exec(&mut db).await?;
```

Deleting a membership leaves `User` and `Group` intact. Deleting an endpoint removes its required membership rows but leaves opposite-endpoint records intact.

### Self-referential many-to-many

```rust
#[derive(Debug, toasty::Model)]
struct User {
    #[key]
    #[auto]
    id: u64,
    name: String,
    #[has_many(pair = follower)]
    outgoing_follows: toasty::Deferred<Vec<Follow>>,
    #[has_many(pair = followed)]
    incoming_follows: toasty::Deferred<Vec<Follow>>,
    #[has_many(via = outgoing_follows.followed)]
    following: toasty::Deferred<Vec<User>>,
    #[has_many(via = incoming_follows.follower)]
    followers: toasty::Deferred<Vec<User>>,
}

#[derive(Debug, toasty::Model)]
#[key(follower_id, followed_id)]
struct Follow {
    #[index]
    follower_id: u64,
    #[belongs_to(key = follower_id, references = id)]
    follower: toasty::Deferred<User>,
    #[index]
    followed_id: u64,
    #[belongs_to(key = followed_id, references = id)]
    followed: toasty::Deferred<User>,
}
```

`pair` disambiguates the two `BelongsTo<User>` fields.

### Backend support

`via` traversal, `.any()`/`.all()` on associations, and `.include()`/`.select()` on `via` relations require a SQL backend. Not available on DynamoDB. Creating/updating/querying/deleting the join model uses its ordinary model APIs (all backends).

## Preloading associations

**Rule: if you `.await` it, it hits the database.** `user.posts().exec(&mut db).await?` is async (query); `user.posts.get()` is sync (reads loaded data). Scan any code path for `.await` to know where round-trips happen.

### The N+1 problem

```rust
// 1 query + N queries (one per user) — BAD
let users = User::all().exec(&mut db).await?;
for user in &users {
    let posts = user.posts().exec(&mut db).await?;
}
```

### Using `.include()`

```rust
let user = User::filter_by_id(user.id)
    .include(User::fields().posts())
    .get(&mut db)
    .await?;
let posts: &[Post] = user.posts.get(); // sync, no query
```

`.include()` loads the relation as part of the query. After preloading, access via `.get()` (sync).

### Eager relation fields

A relation field not wrapped in `Deferred<_>` is loaded by every query (implicit `.include()`):
```rust
#[has_many]
posts: Vec<Post>,
let user = User::filter_by_id(user_id).get(&mut db).await?;
let post_count = user.posts.len();
```

### Access patterns

| Access pattern | Async | When to use |
|----------------|-------|------------|
| `user.posts().exec(&mut db).await?` | Yes | Lazy relation not preloaded |
| `user.posts.get()` | No | `Deferred<_>` preloaded with `.include()` |
| `user.posts` | No | Eager relation field (not `Deferred<_>`) |

Calling `.get()` on an unloaded relation **panics**. Only use it when you know the relation was preloaded.

### `.try_get()` when load state is uncertain

```rust
fn post_count(user: &User) -> Option<usize> {
    user.posts.try_get().map(<[_]>::len)
}
```

| Field type | `.get()` returns | `.try_get()` returns |
|-----------|------------------|----------------------|
| `Deferred<T>` | `&T` | `Option<&T>` |
| `Deferred<Option<T>>` | `&Option<T>` | `Option<&Option<T>>` |
| `Deferred<Vec<T>>` | `&Vec<T>` | `Option<&Vec<T>>` |

For `Deferred<Vec<T>>`, an empty vec means loaded-but-empty; `None` from `.try_get()` means not loaded.

Prefer `.get()` in code that controls the query; reserve `.try_get()` for code accepting records from elsewhere.

### Preloading BelongsTo, HasOne, multi-step

```rust
// BelongsTo (parent from child):
let post = Post::filter_by_id(post_id)
    .include(Post::fields().user())
    .get(&mut db).await?;
let user: &User = post.user.get();

// HasOne (child from parent):
let user = User::filter_by_id(user.id)
    .include(User::fields().profile())
    .get(&mut db).await?;
let profile = user.profile.get().as_ref().unwrap();

// via (many-to-many or multi-step):
let users = User::all()
    .include(User::fields().commented_articles())
    .exec(&mut db).await?;
for user in &users {
    let articles: &[Article] = user.commented_articles.get();
}
```

### Multiple includes

```rust
let user = User::filter_by_id(user_id)
    .include(User::fields().profile())   // HasOne
    .include(User::fields().posts())     // HasMany
    .get(&mut db).await?;
```

Works with `.exec()` (collection queries) too — all records have their relations preloaded.