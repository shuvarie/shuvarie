# Vector Stores & RAG

Retrieval-Augmented Generation (RAG) retrieves relevant documents from a store based on a query and includes them in an LLM prompt, grounding responses in factual information. It reduces hallucinations and lets a model use data not in its training set. Rig provides RAG building blocks — embeddings, vector stores, and RAG-enabled agents — out of the box.

Official docs: https://rig.rs/docs/concepts/rag · API: https://docs.rs/rig/latest/rig/vector_store/index.html · Guide: https://rig.rs/docs/guides/rag/rag_system

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
- **`VectorStoreIndex`** — search a store for documents relevant to a query.
- **`InsertDocuments`** — insert embedded documents into a store.

Rig primarily uses cosine similarity. An in-memory store ships by default (dev + small apps, no external deps); durable stores (LanceDB, MongoDB, Neo4j, PostgreSQL, Qdrant, SurrealDB, Milvus, ScyllaDB, SQLite, S3 Vectors, Cloudflare Vectorize, HelixDB) are available via companion features.

## Minimal RAG agent

```rust
use rig::client::{CompletionClient, EmbeddingsClient, ProviderClient};
use rig::completion::Prompt;
use rig::embeddings::EmbeddingsBuilder;
use rig::providers::openai::Client;
use rig::vector_store::in_memory_store::InMemoryVectorStore;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai_client = Client::from_env()?;
    let embed_model = openai_client.embedding_model("text-embedding-3-small");

    let embeddings = EmbeddingsBuilder::new(embed_model.clone())
        .documents(vec![
            "Rig is a Rust library for building LLM-powered applications.",
            "RAG combines retrieval and generation for better accuracy.",
            "Vector stores enable semantic search over documents.",
        ])?
        .build()
        .await?;

    let mut vector_store = InMemoryVectorStore::default();
    vector_store.add_documents(embeddings);
    let index = vector_store.index(embed_model);

    let agent = openai_client
        .agent("gpt-5.5")
        .preamble("You answer questions using the provided context.")
        .dynamic_context(2, index) // top 2 relevant docs per query
        .build();

    let response = agent
        .prompt("What is Rig and how does it help with LLM applications?")
        .await?;
    println!("{response}");
    Ok(())
}
```

`dynamic_context(n, index)` retrieves `n` relevant documents per query and injects them into the model's context.

## Retrieving documents directly

Query the index with a `VectorSearchRequest` when you want the docs yourself rather than letting an agent inject them:

```rust
use rig::vector_store::{VectorSearchRequest, VectorStoreIndex};

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
use rig::tool::{ToolSet, ToolEmbedding};

let mut toolset = ToolSet::default();
toolset.add_retrieved_tool(Adder);
let embeddings = EmbeddingsBuilder::new(embed_model.clone())
    .documents(toolset.schemas()?)?
    .build()
    .await?;

let vector_store =
    InMemoryVectorStore::from_documents_with_id_f(embeddings, |tool| tool.name.clone());
let index = vector_store.index(embed_model);

let agent = openai_client
    .agent("gpt-5.5")
    .preamble("You are a calculator. Use the tools provided.")
    .retrieved_tools(2, index, toolset)
    .build();
```

`retrieved_tools(sample, index, toolset)` (the 0.42 name) takes max tools to retrieve, the index, and the toolset. At context-assembly time the agent uses RAG to fetch relevant tool definitions to send to the model; called tools are executed from the toolset. See `tools.md` for `ToolEmbedding`. (The old `dynamic_tools(n, index, toolset)` was renamed; `dynamic_tool`/`dynamic_tools` now attach `DynamicTool` values directly.)

## Modern RAG patterns

### Re-ranking

Re-rank initial search results for better relevance. Dedicated re-ranking models score results more deeply than vector search alone. The `fastembed` crate provides a `TextRerank` type (enable the `fastembed` feature).

### Hybrid search

Semantic search can miss results containing a target term but not ranked as "relevant." Hybrid search combines full-text + semantic: store docs in a regular DB and a vector store, query both, merge with [Reciprocal Rank Fusion](https://www.elastic.co/docs/reference/elasticsearch/rest-apis/reciprocal-rank-fusion) or weighted scoring.

### RAG as memory

RAG is a useful basis for agentic memory — store conversation summaries, user/company-specific facts, chunked documents, and retrieve by relevance. See `memory.md`.

## Limitations

- **Split context** — relevant info can span multiple chunks. Use overlapping chunks (10–20%) or parent-child chunking (retrieve small chunks, pass the larger parent to the LLM).
- **Contradictory data** — filter by metadata, apply recency-based weighting, weight by source authority.
- **Stale data** — track `created_at`/`last_updated`, use versioning + TTL, monitor source data to trigger re-embedding.

## Supported stores (companion features)

| Feature | Module | Store |
|---------|--------|-------|
| (default) | `rig::vector_store::in_memory_store` | `InMemoryVectorStore` |
| `lancedb` | `rig::lancedb` | LanceDB |
| `mongodb` | `rig::mongodb` | MongoDB |
| `neo4j` | `rig::neo4j` | Neo4j |
| `postgres` | `rig::postgres` | PostgreSQL (pgvector) |
| `qdrant` | `rig::qdrant` | Qdrant |
| `surrealdb` | `rig::surrealdb` | SurrealDB |
| `sqlite` | `rig::sqlite` | SQLite |
| `milvus` | `rig::milvus` | Milvus |
| `scylladb` | `rig::scylladb` | ScyllaDB |
| `s3vectors` | `rig::s3vectors` | AWS S3 Vectors |
| `vectorize` | `rig::vectorize` | Cloudflare Vectorize |
| `helixdb` | `rig::helixdb` | HelixDB |

Setup per store: https://rig.rs/docs/integrations/vector_stores