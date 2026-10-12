# Vector Stores & RAG

Retrieval-Augmented Generation (RAG) retrieves relevant documents from a store based on a query and includes them in an LLM prompt, grounding responses in factual information. It reduces hallucinations and lets a model use data not in its training set. Rig provides RAG building blocks — embeddings, vector stores, and RAG-enabled agents — out of the box.

Official docs: https://rig.rs/docs/concepts/rag · API: https://docs.rs/rig-core/latest/rig_core/vector_store/index.html · Guide: https://rig.rs/docs/guides/rag/rag_system

## How RAG works

- **Embeddings** — numerical vectors carrying semantic meaning (see `embeddings.md`). One embedding is `rig_core::embeddings::Embedding { document: String, vec: Vec<f64> }`.
- **Cosine similarity** — the default metric; higher score = more similar. Cheap and effective, hence the default for RAG/recommender/hybrid-search. The metrics live on `rig_core::embeddings::distance::VectorDistance`: `dot_product`, `cosine_similarity(other, normalized)`, `angular_distance`, `euclidean_distance`, `manhattan_distance`, `chebyshev_distance`.

Two phases:

1. **Ingestion** — split documents into chunks (fixed token sizes 512–1000, or semantic boundaries like paragraphs), embed each chunk, insert embeddings + metadata into a vector store.
2. **Retrieval** — embed the query with the **same model** used for the documents, run a vector search, include the top results + metadata in the LLM prompt.

### Do I need RAG?

Essential for support bots / chatbots grounded in docs you own. Probably unnecessary for simple classification where categories are already well-known.

## RAG in Rig

Two traits in `rig_core::vector_store`:

| Trait | Item | Signature |
|-------|------|-----------|
| `VectorStoreIndex` | `type Filter` | `Filter: SearchFilter + WasmCompatSend + WasmCompatSync` — the backend's *native* filter type (the in-memory index uses `Filter<serde_json::Value>`) |
| | `top_n<T: DeserializeOwned + WasmCompatSend>` | `(&self, req: VectorSearchRequest<Self::Filter>) -> impl Future<Output = Result<Vec<VectorSearchResult<T>>, VectorStoreError>> + WasmCompatSend` |
| | `top_n_ids` | `(&self, req: VectorSearchRequest<Self::Filter>) -> impl Future<Output = Result<Vec<VectorSearchIdResult>, VectorStoreError>> + WasmCompatSend` |
| `InsertDocuments` | `insert_documents<Doc: Serialize + Embed + WasmCompatSend>` | `(&self, documents: Vec<(Doc, Vec<Embedding>)>) -> impl Future<Output = Result<(), VectorStoreError>> + WasmCompatSend` |

```rust
pub struct VectorSearchResult<T> { pub score: f64, pub id: String, pub document: T }
pub struct VectorSearchIdResult { pub score: f64, pub id: String }
```

**Changed in 0.44 (breaking, #2688):** search results are *named*. 0.43 returned `Vec<(f64, String, T)>` from `top_n` and `Vec<(f64, String)>` from `top_n_ids`; 0.44 returns `Vec<VectorSearchResult<T>>` / `Vec<VectorSearchIdResult>`. Score scale/direction and ordering are the backend's — the in-memory index orders by descending best finite cosine similarity, then ascending document id.

- `flatten_embedded<Doc: Serialize, R>(documents, f)` — serializes each document to JSON once and applies `f(&Value, Embedding) -> Result<R, VectorStoreError>` per embedding; the shared shape of most `InsertDocuments` implementations.
- `InMemoryVectorStore` implements **neither** trait: documents enter through its constructors/builder and searching happens on `InMemoryVectorIndex` (see "In-memory store & LSH"). `vector_store::lsh` defines no index impl either — it is a candidate generator the in-memory store calls.
- **Changed in 0.44 (#2645):** the in-memory types no longer require `D: Eq` (`impl<D: Serialize> InMemoryVectorStore<D>`) and the `…_with_id_f` id callbacks take `impl FnMut(&D) -> String`, not a bare `fn` pointer — the callback may capture and mutate state. 0.43 required `Eq` (`rig-core-0.43.0/src/vector_store/in_memory_store.rs:44,414`) and a `fn(&D) -> String` (`:104`).

### `VectorStoreError`

| Variant | Carries / `#[from]` |
|---------|---------------------|
| `EmbeddingError(ProviderError)` | the embedding call behind a query or insert failed (`#[from]`) |
| `JsonError(serde_json::Error)` | document (de)serialization (`#[from]`) |
| `DatastoreError(Box<dyn Error + Send + Sync>)` | a backend error; build it with `VectorStoreError::datastore(e)`. On wasm the boxed error is not `Send + Sync` |
| `FilterError(FilterError)` | filter construction or translation (`#[from]`) |
| `MissingIdError(String)` | a document reached the backend without an id it requires |
| `Http(crate::http_client::Error)` | transport failure for a remote store; non-success replies keep their status-bearing variants (`#[from]`) |
| `ExternalAPIError(StatusCode, String)` | the store's API answered an error code + message |
| `SamplesOutOfRange { requested: u64, max: u64 }` | the requested sample count exceeds what the backend can express |

**Changed in 0.44 (#2683):** `BuilderError(String)` is gone — the builder is typestate-checked, and a backend that refuses an oversized `samples` reports `SamplesOutOfRange` (`rig-core-0.43.0/src/vector_store/mod.rs:73-75`).

Rig primarily uses cosine similarity. An in-memory store ships by default (dev + small apps, no external deps), plus an approximate LSH index (`vector_store::lsh`). Durable stores (LanceDB, MongoDB, Neo4j, PostgreSQL, Qdrant, Milvus, SQLite, …) are companion crates — for 0.44 they are republished and feature-gated on the `rig` facade (see "Stores"). Their contracts (`VectorStoreIndex` / `InsertDocuments` / `SearchFilter` / `DynamicSearchFilter`) live in `rig-core`, so implement the traits over your own store when needed.

## Minimal RAG agent

```rust
use rig_agent::AgentBuilder;
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::{self, OpenAI};
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let embed_model = openai
        .embedding(openai::TEXT_EMBEDDING_3_SMALL, None)
        .erase(); // DynModel<operation::Embedding>

    let embeddings = EmbeddingsBuilder::new(embed_model.clone())
        .documents(vec![
            "Rig is a Rust library for building LLM-powered applications.",
            "RAG combines retrieval and generation for better accuracy.",
            "Vector stores enable semantic search over documents.",
        ])?
        .build()
        .await?; // Vec<(doc, Vec<Embedding>)>

    let vector_store = InMemoryVectorStore::from_documents(embeddings);
    let index = vector_store.index(embed_model); // InMemoryVectorIndex: VectorStoreIndex

    let agent = AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You answer questions using the provided context.")
        .dynamic_context(2, index) // top 2 relevant docs per query
        .build();

    let response = agent
        .prompt("What is Rig and how does it help with LLM applications?")
        .await?;
    println!("{}", response.output()); // `output()` is a method in 0.44, not a field
    Ok(())
}
```

`dynamic_context(n, index)` retrieves `n` relevant documents per query and injects them into the model's context. It requires `I: VectorStoreIndex<Filter = F> + 'static` where `F: DynamicSearchFilter + WasmCompatSend + WasmCompatSync + 'static`.

| Builder method | What it registers | Result |
|----------------|-------------------|--------|
| `dynamic_context(samples, index)` | a `RetrieveAdapter` for `index` under `<owner>/retrieve:context#<n>` plus a `DynamicContext` completion-call hook (the key is minted at build) | documents |
| `dynamic_context_handler(samples, handler)` | any retrieval-family `Serve` handler under the next context key, same hook lifecycle (e.g. a replayer answering a recorded index) | documents |
| `retrieved_tools(sample, index, toolset)` | a `RetrieveAdapter` under `<owner>/retrieve:tools#<n>` on the builder's tool registry | tool ids |
| `retrieved_tools_handler(sample, handler, toolset)` | a retrieval-family handler under the next retrieval-tools key | tool ids |

The `DynamicContext` hook runs at the completion-call boundary (see `hooks.md`): it takes the prompt's `rag_text()` or the most recent history message that has one, builds `VectorSearchRequest::builder().query(q).samples(n).build()`, calls `top_n::<serde_json::Value>` on the bound index, and patches every hit into the request as `Document { id: <store id>, text: <pretty-printed JSON of the document> }`.

Retrieval through the bus:

| Item | Type / shape |
|------|--------------|
| `rig_agent::bus::IndexHandle` | `Handle<family::Retrieve>`: `top_n<T>(req) -> Retrieval<T>`, `top_n_ids(req) -> Typed<family::Retrieve, Vec<VectorSearchIdResult>>`, plus `key()`, `descriptor()`, `is_closed()` |
| `rig_agent::bus::Retrieval<T>` | `Typed<family::Retrieve, Vec<VectorSearchResult<T>>>` — **changed in 0.44** from `Typed<family::Retrieve, Vec<(f64, String, T)>>` |
| `Typed<F, T>` | `Future<Output = Result<T, ErrorReport>>`; dropping it cancels the dispatch |
| `HookContext::bind(&Key<family::Retrieve>)`, `HookContext::index(&HandlerKey)` | a `RunHandle<'_, family::Retrieve>` borrowing the hook context, with `top_n::<T>(req) -> Retrieval<T>` — a hook retrieving for itself |

On the wire the family still carries tuples: `EffectKind::Retrieve { query }` with `RetrieveQuery::{TopN, TopNIds}` holding a `VectorSearchRequest<Filter<serde_json::Value>>`, answered by `RetrievedDocuments::{Scored(Vec<(f64, String, serde_json::Value)>), Ids(Vec<(f64, String)>)}`. `RetrieveAdapter` / `IndexHandle` translate the canonical filter to the backend's and deserialize documents on the caller's side.

## Retrieving documents directly

Query the index with a `VectorSearchRequest` when you want the docs yourself rather than letting an agent inject them:

```rust
use rig_core::vector_store::request::{Filter, VectorSearchRequest};
use rig_core::vector_store::VectorStoreIndex;

let request: VectorSearchRequest = VectorSearchRequest::builder()
    .query("What is Rig?")
    .samples(2)
    .threshold(0.7)                                   // optional
    .filter(Filter::eq("category", serde_json::json!("manual"))) // optional
    .build();

// Vec<VectorSearchResult<String>>
let results = index.top_n::<String>(request.clone()).await?;
for result in results {
    println!("score={} id={} doc={}", result.score, result.id, result.document);
}

// Vec<VectorSearchIdResult> — ids only, no document payload
for result in index.top_n_ids(request).await? {
    println!("score={} id={}", result.score, result.id);
}
```

Feed `result.document` / `result.id` into a completion request from here. A second index over the same store is free: `InMemoryVectorStore::index(model)` consumes the store, and `InMemoryVectorIndex::new(model, store)` / the `pub store` field take it back.

## VectorSearchRequest & filters

### `VectorSearchRequest<F = Filter<serde_json::Value>>`

Private fields, read through accessors (`Clone + Debug + Serialize + Deserialize`):

| Field | Type | Accessor |
|-------|------|----------|
| `query` | `String` | `query() -> &str` |
| `samples` | `u64` | `samples() -> u64` |
| `threshold` | `Option<f64>` | `threshold() -> Option<f64>` |
| `filter` | `Option<F>` | `filter() -> Option<&F>` |

- `map_filter<T>(self, f: impl Fn(F) -> T) -> VectorSearchRequest<T>` — converts between filter representations (canonical JSON → backend filter).
- `try_map_filter<T>(self, f: impl Fn(F) -> Result<T, FilterError>) -> Result<VectorSearchRequest<T>, FilterError>` — the fallible form, for filters a backend cannot represent.
- No public field, and no `map`/`into_*` helpers: build a new request through the builder.

### `VectorSearchRequestBuilder<F = Filter<serde_json::Value>, Q = Missing, S = Missing>`

Typestate over `rig_core::markers::{Missing, Provided<T>}`:

| Method | Type change | Notes |
|--------|-------------|-------|
| `VectorSearchRequest::builder()` | `VectorSearchRequestBuilder<F, Missing, Missing>` | `F` is inferred from use |
| `.query(impl Into<String>)` | `-> …<F, Provided<String>, S>` | required |
| `.samples(u64)` | `-> …<F, Q, Provided<u64>>` | required |
| `.threshold(f64)` | `Self` | optional; results below it may be omitted |
| `.filter(F)` | `Self` | optional |
| `.build()` | `VectorSearchRequest<F>` | only on `…<F, Provided<String>, Provided<u64>>`; it validates nothing — not query emptiness, sample count, nor threshold |

### Filters

| Item | Shape |
|------|-------|
| `SearchFilter` | tagless-final, backend-agnostic: `type Value`; `eq`/`gt`/`lt`: `(key: impl AsRef<str>, value: Self::Value) -> Self`; `and`/`or`: `(self, rhs: Self) -> Self` |
| `Filter<V>` | canonical serializable filter (`serde(rename_all = "lowercase")`): `Eq(String, V)`, `Gt(String, V)`, `Lt(String, V)`, `And(Box<Self>, Box<Self>)`, `Or(Box<Self>, Box<Self>)`. Implements `SearchFilter<Value = V>` for `V: Debug + Clone + Serialize + DeserializeOwned` |
| `Filter::<serde_json::Value>::satisfies(&Value) -> bool` | in-process match for the in-memory store: top-level object fields only, missing fields fail every leaf; `Eq` compares numerically (`5` matches `5.0`) or structurally; `Gt`/`Lt` compare number/string/bool/null pairs of the same kind; `And`/`Or` short-circuit |
| `Filter::interpret::<F: SearchFilter<Value = V>>()`, `interpret_with(conv)`, `try_interpret(conv)` | convert the canonical filter into a backend's filter type, optionally mapping each leaf value (fails on the first value that does not convert) |
| `DynamicSearchFilter: SearchFilter + Sized` | what a *backend* filter type implements so the bus can hand it the canonical filter: `from_dynamic_filter(Filter<serde_json::Value>) -> Result<Self, FilterError>` and `normalize_dynamic_document(Value) -> Value`. The blanket impl for `SearchFilter<Value = Value>` interprets the filter and prunes arrays longer than 400 elements from returned documents (a removed root array becomes `null`) |
| `SqlCondition<P>` | a rendered SQL condition plus positional bind parameters, for the SQL backends (sqlite renders through it; `rig_core::vector_store::request::SqlCondition`) |

`SqlCondition<P>` (`Clone + Debug + Serialize + Deserialize`, `Default` = empty condition): `binary(key, op, placeholder, value)`, `list(key, op, placeholder, values)`, `range(key, placeholder, lo, hi)`, `between(key, placeholder, lo, hi)`, `raw(condition)`, `and`/`or`/`not`, `condition() -> &str`, `params() -> &[P]`, `render_placeholders(|i| format!("${}", i + 1)) -> String`, `into_parts() -> (String, Vec<P>)`. Keys, operators and placeholder text are interpolated verbatim (callers must quote or validate them for the target backend); values stay separated as bind parameters.

`FilterError`: `Expected { expected: String, got: String }`, `TypeError(String)` (cannot compile to the backend's filter type), `MissingField(String)`, `Must(String, String)`, `Serialization(String)` (a string, because `serde_json::Error` is not `Clone`).

## In-memory store & LSH

`InMemoryVectorStore<D: Serialize>` is a `HashMap<String, (D, Vec<Embedding>)>` keyed by document id, plus an `IndexStrategy` and an optional `LSHIndex`; it derives `Clone` and defaults to an empty brute-force store. Documents need no `Eq`/`Ord`.

| Item (`vector_store::in_memory_store`) | Signature |
|----------------------------------------|-----------|
| `from_documents(documents)` | `impl IntoIterator<Item = (D, Vec<Embedding>)> -> Self` (consumes the iterator); ids are `"doc{n}"`, starting at the current count and skipping occupied ids |
| `from_documents_with_ids(documents)` | `impl IntoIterator<Item = (impl ToString, D, Vec<Embedding>)> -> Self`; duplicates replace |
| `from_documents_with_id_f(documents, f)` | `f: impl FnMut(&D) -> String`, called once per document in input order |
| `builder()` | `InMemoryVectorStoreBuilder<D>`, brute force unless configured |
| `add_documents` / `add_documents_with_ids` / `add_documents_with_id_f` | the same three shapes on `&mut self` |
| `index(self, model: impl Into<DynModel<operation::Embedding>>)` | `InMemoryVectorIndex<D>` |
| `iter()`, `len()`, `is_empty()` | iteration over `(&String, &(D, Vec<Embedding>))`, count, emptiness |

`InMemoryVectorStoreBuilder<D>` (declared in `vector_store::builder`, re-exported from `in_memory_store`): `new()`, `index_strategy(IndexStrategy)`, `documents(docs)`, `documents_with_ids(docs)`, `build()`. There is **no** `documents_with_id_f` on the builder — use a store constructor or `add_documents_with_id_f`.

```rust
use rig_core::vector_store::{IndexStrategy, builder::InMemoryVectorStoreBuilder};

let store = InMemoryVectorStoreBuilder::<String>::new()
    .index_strategy(IndexStrategy::LSH { num_tables: 5, num_hyperplanes: 10 })
    .documents([("a searchable document".to_owned(), Vec::new())])
    .build();

let index = store.index(embed_model); // InMemoryVectorIndex<String>: VectorStoreIndex
```

`InMemoryVectorIndex<D: Serialize>` pairs a store with the query embedding model: `new(model, store)`, `pub store`, `iter`/`len`/`is_empty`, and the `VectorStoreIndex` impl `where D: Serialize + WasmCompatSend + WasmCompatSync` with `type Filter = Filter<serde_json::Value>`. Stored vectors must use the same embedding space and dimensions as query vectors; the filter and threshold are applied *during* the scan, before the top-`n` cut, so a filtered query does not return an unfiltered top-`n`.

`IndexStrategy` (`vector_store::IndexStrategy`, `Default = BruteForce`):

| Variant | Meaning |
|---------|---------|
| `BruteForce` | scores every stored document |
| `LSH { num_tables: usize, num_hyperplanes: usize }` | LSH proposes candidates, exact cosine distances rank them |

With the LSH strategy the store builds an `LSHIndex` at construction and inserts every later embedding into it. If the index could not be built (empty store, or zero-width embeddings) a search logs a warning and falls back to brute force.

`vector_store::lsh` (`Clone + Default`):

| Item | Signature |
|------|-----------|
| `LSH::new(dim, num_tables, num_hyperplanes)` | random normalized hyperplanes; use at most 64 hyperplanes per table and keep `num_tables * num_hyperplanes` inside `usize` |
| `LSH::hash(&self, vector: &[f64], table_idx: usize) -> u64` | f32 sign bits; lengths are not checked |
| `LSHIndex::new(dim, num_tables, num_hyperplanes)` | one `HashMap<u64, Vec<String>>` per table |
| `LSHIndex::insert(&mut self, id: &str, embedding: &[f64])` | into every table |
| `LSHIndex::query(&self, embedding: &[f64]) -> Vec<String>` | unique ids sharing a bucket in any table, unspecified order |
| `LSHIndex::clear()` | empties every table |

Candidates are approximate; the distance is always exact.

## Tool RAG

Modern agents can carry large tool lists, which wastes context and degrades output. Tool RAG stores tool definitions in a vector store and retrieves only the relevant ones at request time — saving context budget and token cost.

Tools that should be retrievable implement `ToolEmbedding` (in addition to `Tool`) and are registered as retrievable tools in a `ToolSet`:

```rust
use rig_agent::tool::{ToolSet, ToolEmbedding};
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

let mut toolset = ToolSet::default();
let _ = toolset.add_retrieved_tool(Adder);
let embeddings = EmbeddingsBuilder::new(embed_model.clone())
    .documents(toolset.schemas()?) // Vec<ToolSchema>
    .build()
    .await?;

let vector_store =
    InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
let index = vector_store.index(embed_model);

let agent = rig_agent::AgentBuilder::new(completion_model)
    .preamble("You are a calculator. Use the tools provided.")
    .retrieved_tools(2, index, toolset)
    .build();
```

`retrieved_tools(sample, index, toolset)` takes the max tools to retrieve, the index, and the toolset. Each request dispatches `RetrieveQuery::TopNIds` on every retrieval-tools key and passes the returned ids to the tool snapshot (found ids first, then the always-exposed registrations); called tools are executed from the toolset.

| Item | Shape |
|------|-------|
| `ToolEmbedding: Tool` | `type InitError: Error + WasmCompatSend + WasmCompatSync`, `type Context: DeserializeOwned + Serialize`, `type State: WasmCompatSend`; `embedding_docs(&self) -> Vec<String>`, `context(&self) -> Self::Context`, `init(state, context) -> Result<Self, Self::InitError>` |
| `PortableToolEmbedding: PortableTool` | the context-free twin (`rig_core::tool`, not re-exported from `rig_agent::tool`) |
| `ToolSchema { name: String, context: serde_json::Value, embedding_docs: Vec<String> }` | `Embed`, so `EmbeddingsBuilder::documents(toolset.schemas()?)` works; `ToolSchema::try_from(&impl PortableToolEmbedding)` |
| `ToolSet::add_retrieved_tool<T: ToolEmbedding>(&mut self, tool) -> Result<String, serde_json::Error>` | registers the tool plus its embedding descriptor; a context that does not serialize registers nothing |
| `ToolSet::schemas(&self) -> Result<Vec<ToolSchema>, EmbedError>` | the embeddable forms of the retrievable registrations |
| `ToolSet::documents(&self) -> Vec<Document>` | every definition as a `Document` for a text index instead of `ToolSchema` |

`dynamic_tool` / `dynamic_tools` attach `DynamicTool` values directly (no index), and `retrieved_tools_handler` swaps in a custom retrieval-family handler (see `tools.md` for `Tool`, `PortableTool` and `DynamicTool`).

### Vector stores are tools

Any `VectorStoreIndex` whose `Filter` is a `SearchFilter<Value = serde_json::Value>` that is also `DeserializeOwned + WasmCompatSend + WasmCompatSync` blanket-implements `rig_core::tool::PortableTool`:

| `PortableTool` item | Value |
|---------------------|-------|
| `NAME` | `"search_vector_store"` |
| `Args` | `VectorSearchRequest<F>` |
| `Output` | `Vec<VectorSearchResult<serde_json::Value>>` |
| `Error` | `VectorStoreError` |
| `description()` | `"Retrieves the most relevant documents from a vector store based on a query."` |
| `parameters()` | object with `query: string`, `samples: integer` (default `5`, minimum `1`) and `threshold: number`; `required: ["query", "samples"]` |
| `call(args)` | `self.top_n(args)` |

So an index can be handed to a model as a tool without a wrapper: `AgentBuilder::new(model).tool(index)`. Three details matter:

- **`filter` is not advertised**: the schema lists only `query` / `samples` / `threshold`, yet `Args` is the full request, so a caller (not the model) may still pass a filter — the `DeserializeOwned` bound on `F` exists for exactly that.
- **Every index shares one tool name** (`search_vector_store`), so two indexes in one tool set collide with a `replacing an existing tool registration` warning; expose one, or re-key a registration (`RegisteredTool::with_key`).
- **Changed in 0.44 (breaking, #2688):** `Output` was `Vec<VectorStoreOutput { score, id, document: Value }>` in 0.43; that struct is gone and the tool returns the named results directly. The serialized shape is unchanged: `[{"score": 0.9, "id": "doc-1", "document": {"answer": 42}}]`.

## Modern RAG patterns

### Re-ranking

Re-rank initial search results for better relevance. Dedicated re-ranking models score results more deeply than vector search alone, and rig models them as an operation (`rig_core::rerank`): `RerankRequest { query, documents }` → `RerankResponse { results: Vec<RerankResult { index, document, relevance_score }>, model, usage, provider, response_id, provider_request_id, raw }` on a `DynModel<operation::Rerank>`. An OpenAI-shaped client builds one with `client.rerank(model)`, and a model handler serves the family under `rerank:<label>`. Scores are response-local (the range is provider-specific; do not read them as probabilities or compare across responses).

### Hybrid search

Semantic search can miss results containing a target term but not ranked as "relevant". Hybrid search combines full-text + semantic: store docs in a regular DB and a vector store, query both, merge with [Reciprocal Rank Fusion](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/reciprocal-rank-fusion) or weighted scoring. SQL backends can lean on `SqlCondition` to render the metadata half of the query.

### RAG as memory

RAG is a useful basis for agentic memory — store conversation summaries, user/company-specific facts, chunked documents, and retrieve by relevance. See `memory.md`.

## Limitations

- **Split context** — relevant info can span multiple chunks. Use overlapping chunks (10–20%) or parent-child chunking (retrieve small chunks, pass the larger parent to the LLM).
- **Contradictory data** — filter by metadata, apply recency-based weighting, weight by source authority.
- **Stale data** — track `created_at`/`last_updated`, use versioning + TTL, monitor source data to trigger re-embedding.

## Stores

| Module | Store | 0.44 status |
|--------|-------|-------------|
| `rig_core::vector_store::in_memory_store` | `InMemoryVectorStore` / `InMemoryVectorIndex` | ships in core, no external deps |
| `rig_core::vector_store::lsh` | `LSH` / `LSHIndex` approximate candidates | ships in core |
| `rig_core::vector_store::builder` | `InMemoryVectorStoreBuilder` | ships in core |
| companions (`rig-lancedb`, `rig-qdrant`, `rig-mongodb`, `rig-neo4j`, `rig-postgres`, `rig-sqlite`, `rig-milvus`, `rig-scylladb`, `rig-s3vectors`, `rig-vectorize`, `rig-helixdb`) | LanceDB, Qdrant, MongoDB, Neo4j, PostgreSQL (pgvector), SQLite, Milvus, ScyllaDB, S3 Vectors, Cloudflare Vectorize, HelixDB | **republished for 0.44** (unlike 0.43), feature-gated on the `rig` facade; they are not vendored here, so take each crate's concrete API from its own docs — their contracts (`VectorStoreIndex` / `InsertDocuments` / `SearchFilter` / `DynamicSearchFilter`) live in `rig-core` |
| `rig-fastembed` | fastembed text/image embedding models (embeddings, not a store) | republished for 0.44 |

Setup per store: https://rig.rs/docs/integrations/vector_stores
