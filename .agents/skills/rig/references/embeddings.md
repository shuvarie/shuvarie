# Embeddings

An embedding is a vector representation of data — usually text — where semantically similar items map to nearby points. Rig's embeddings system turns your data into vectors so you can power semantic search, similarity comparison, and RAG.

Official docs: https://rig.rs/docs/concepts/embeddings · API: https://docs.rs/rig/latest/rig/embeddings/index.html

An `Embedding` carries the original document text alongside its vector (`Vec<f64>`).

## Minimal example

Create an embedding model from a provider client and embed strings with `EmbeddingsBuilder`:

```rust
use rig::client::{EmbeddingsClient, ProviderClient};
use rig::embeddings::EmbeddingsBuilder;
use rig::providers::openai;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai_client = openai::Client::from_env()?;
    let model = openai_client.embedding_model("text-embedding-3-small");

    let embeddings = EmbeddingsBuilder::new(model)
        .document("Some text")?
        .document("More text")?
        .build()
        .await?;

    println!("Generated {} embeddings", embeddings.len());
    Ok(())
}
```

## The `Embed` trait

A type must implement `Embed` to be embedded. Derive it (with the `derive` feature) or implement manually.

**Derive macro** — mark field(s) to embed with `#[embed]`:

```rust
use rig::Embed;

#[derive(Embed)]
struct Foo {
    id: i32,
    #[embed]
    name: String,
}
```

**Manual impl** — push each text fragment through a `TextEmbedder`:

```rust
use rig::embeddings::{Embed, EmbedError, TextEmbedder};

struct WordDefinition { id: i32, word: String, definition: String }

impl Embed for WordDefinition {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        embedder.embed(self.definition.to_owned()); // only the definition
        Ok(())
    }
}
```

## Embedding many documents

`.documents(vec)` batches `Embed` values — the builder respects the provider's max batch size and handles concurrency:

```rust
use rig::client::EmbeddingsClient;
use rig::embeddings::EmbeddingsBuilder;
use rig::providers::openai;

let documents = vec![
    Foo { id: 1, name: "Rig".to_string() },
    Foo { id: 2, name: "Playgrounds".to_string() },
];

let model = openai::Client::from_env()?.embedding_model("text-embedding-3-small");

let embeddings = EmbeddingsBuilder::new(model)
    .documents(documents)?
    .build()
    .await?;
```

Returns an iterator over `(T, OneOrMany<Embedding>)` — collect for use elsewhere or insert straight into a vector store.

## Storing embeddings

Insert into any store implementing `InsertDocuments`:

```rust
use rig::vector_store::InsertDocuments;

qdrant.insert_documents(embeddings).await?;
```

See `rag-and-vector-stores.md` for how embeddings power retrieval and the supported stores.

## Best practices

- **Prepare documents** — clean/normalize text before embedding; chunk large documents into focused pieces (512–1000 tokens or semantic boundaries).
- **Match models** — query embeddings must use the **exact same model id** as the stored documents; vectors from different models aren't comparable.
- **Handle errors** — validate non-empty input; handle provider API errors gracefully.
- **Batch** — prefer `.documents(...)` for multiple items so Rig batches requests efficiently.

## OpenAI embedding constants

| Constant | Dimensions | Notes |
|----------|-----------:|-------|
| `TEXT_EMBEDDING_3_LARGE` | 3072 | |
| `TEXT_EMBEDDING_3_SMALL` | 1536 | |
| `TEXT_EMBEDDING_ADA_002` | 1536 | legacy |

The dimension matters when provisioning a vector store index — it must match the model you embed with.