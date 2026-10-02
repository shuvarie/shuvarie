# Vector Stores & RAG

Retrieval-Augmented Generation (RAG) retrieves relevant documents from a store based on a query and includes them in an LLM prompt, grounding responses in factual information. It reduces hallucinations and lets a model use data not in its training set. Rig provides RAG building blocks — embeddings, vector stores, and RAG-enabled agents — out of the box.

Official docs: https://rig.rs/docs/concepts/rag · API: https://docs.rs/rig-core/latest/rig_core/vector_store/index.html · Guide: https://rig.rs/docs/guides/rag/rag_system (0.43: `rig-core-0.43.0/src/vector_store/`)

## How RAG works

- **Embeddings** — numerical vectors carrying semantic meaning (see `embeddings.md`).
- **Cosine similarity** — the default metric; higher score = more similar. Cheap and effective, hence the default for RAG/recommender/hybrid-search.

Two phases:

1. **Ingestion** — split documents into chunks (fixed token sizes 512–1000, or semantic boundaries like paragraphs), embed each chunk, insert embeddings + metadata into a vector store.
2. **Retrieval** — embed the query with the **same model** used for the documents, run a vector search, include the top results + metadata in the LLM prompt.

### Do I need RAG?

Essential for support bots / chatbots grounded in docs you own. Probably unnecessary for simple classification where categories are already well-known.

## RAG in Rig

Two traits:
- **`VectorStoreIndex`** — search a store for documents relevant to a query: `top_n::<T>(req)` (scored `(f64, String, T)` tuples) / `top_n_ids(req)` (`(f64, String)`); each index carries an associated `Filter: SearchFilter` type (in-memory: `Filter` over serde values — `.filter(json!({...}))` on the request).
- **`InsertDocuments`** — insert computed embeddings: `insert_documents::<Doc>(documents: Vec<(Doc, Vec<Embedding>)>)`.

Rig primarily uses cosine similarity. An in-memory store ships by default (dev + small apps, no external deps), plus an approximate LSH index (`vector_store::lsh`). Durable stores (LanceDB, MongoDB, Neo4j, PostgreSQL, Qdrant, SurrealDB, Milvus, SQLite, …) are companion crates — **not republished for 0.43 at the time of writing**; their contracts (`VectorStoreIndex`/`InsertDocuments`/`SearchFilter`) live in `rig-core`, so implement the traits over your own store when needed.

## Minimal RAG agent

```rust
use rig_agent::AgentBuilder;
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::{self, OpenAI};
use rig_core::vector_store::in_memory_store::InMemoryVectorStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let embed_model = openai.embedding("text-embedding-3-small", None); // Model<Embeddings>

    let embeddings = EmbeddingsBuilder::new(embed_model.clone())
        .documents(vec![
            "Rig is a Rust library for building LLM-powered applications.",
            "RAG combines retrieval and generation for better accuracy.",
            "Vector stores enable semantic search over documents.",
        ])?
        .build()
        .await?;

    let vector_store = InMemoryVectorStore::from_documents(embeddings); // Vec<(doc, Vec<Embedding>)>
    let index = vector_store.index(embed_model.clone());                // needs DynModel<Embedding>

    let agent = AgentBuilder::new(openai.completion(openai::GPT_5_2))
        .preamble("You answer questions using the provided context.")
        .dynamic_context(2, index) // top 2 relevant docs per query
        .build();

    let response = agent
        .prompt("What is Rig and how does it help with LLM applications?")
        .await?;
    println!("{}", response.output);
    Ok(())
}
```

`dynamic_context(n, index)` retrieves `n` relevant documents per query and injects them into the model's context (implemented as a generated retrieval handler on the agent's bus + a completion-call hook — see `hooks.md` for the boundary it runs at). `AgentBuilder::dynamic_context_handler(n, handler)` swaps in a custom retrieval-family handler (e.g. a replay adapter).

## Retrieving documents directly

Query the index with a `VectorSearchRequest` when you want the docs yourself rather than letting an agent inject them:

```rust
use rig_core::vector_store::request::VectorSearchRequest;
use rig_core::vector_store::VectorStoreIndex;

let req = VectorSearchRequest::builder()
    .query("What is Rig?")
    .samples(2)
    .build();

let results = index.top_n::<String>(req).await?;
for (score, id, doc) in results {
    println!("score={score} id={id} doc={doc}");
}
```

Each result is a `(score, id, document)` tuple. Feed the docs into a completion request from here.

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
    .documents(toolset.schemas()?)?
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

`retrieved_tools(sample, index, toolset)` takes the max tools to retrieve, the index, and the toolset. At context-assembly time the agent uses RAG to fetch relevant tool definitions to send to the model; called tools are executed from the toolset. See `tools.md` for `ToolEmbedding` and `ToolSchema` (`ToolSet::schemas()` embeddable forms). `dynamic_tool`/`dynamic_tools` attach `DynamicTool` values directly (no index).

## Modern RAG patterns

### Re-ranking

Re-rank initial search results for better relevance. Dedicated re-ranking models score results more deeply than vector search alone (e.g. Cohere's `Rerank` operation, VoyageAI's `rerank` model — `client.rerank(id)` wires in 0.43; rerank is an `Operation` with its own request folding).

### Hybrid search

Semantic search can miss results containing a target term but not ranked as "relevant." Hybrid search combines full-text + semantic: store docs in a regular DB and a vector store, query both, merge with [Reciprocal Rank Fusion](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/reciprocal-rank-fusion) or weighted scoring.

### RAG as memory

RAG is a useful basis for agentic memory — store conversation summaries, user/company-specific facts, chunked documents, and retrieve by relevance. See `memory.md`.

## Limitations

- **Split context** — relevant info can span multiple chunks. Use overlapping chunks (10–20%) or parent-child chunking (retrieve small chunks, pass the larger parent to the LLM).
- **Contradictory data** — filter by metadata, apply recency-based weighting, weight by source authority.
- **Stale data** — track `created_at`/`last_updated`, use versioning + TTL, monitor source data to trigger re-embedding.

## Stores

| Module | Store | 0.43 status |
|--------|-------|-------------|
| `rig_core::vector_store::in_memory_store` | `InMemoryVectorStore` | ships in core |
| `rig_core::vector_store::lsh` | LSH approximate index | ships in core |
| companions (`rig-lancedb`, `rig-qdrant`, `rig-mongodb`, `rig-neo4j`, `rig-postgres`, `rig-surrealdb`, `rig-sqlite`, `rig-milvus`, `rig-scylladb`, `rig-s3vectors`, `rig-vectorize`, `rig-helixdb`) | LanceDB, Qdrant, MongoDB, Neo4j, PostgreSQL (pgvector), SurrealDB, SQLite, Milvus, ScyllaDB, S3 Vectors, Cloudflare Vectorize, HelixDB | not published for 0.43 yet — implement `VectorStoreIndex`/`InsertDocuments` over your own store meanwhile |

Setup per store: https://rig.rs/docs/integrations/vector_stores