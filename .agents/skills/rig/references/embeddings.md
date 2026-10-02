# Embeddings

An embedding is a vector representation of data — usually text — where semantically similar items map to nearby points. Rig's embeddings system turns your data into vectors so you can power semantic search, similarity comparison, and RAG.

Official docs: https://rig.rs/docs/concepts/embeddings · API: https://docs.rs/rig-core/latest/rig_core/embeddings/index.html (0.43: `rig-core-0.43.0/src/embeddings/`)

An `Embedding` carries the vector (`vec: Vec<f64>`).

## Minimal example

An embedding model is a **`Model<Embeddings>`** — a client builds one with `.embedding(id, ndims)`. Embed text (`String`, which implements `Embed`) values with `EmbeddingsBuilder`:

```rust
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::{self, OpenAI};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let model = openai.embedding("text-embedding-3-small", None); // Model<Embeddings>

    let embeddings = EmbeddingsBuilder::new(model)
        .document("Some text")?
        .document("More text")?
        .build()
        .await?;

    println!("Generated {} embeddings", embeddings.len());
    Ok(())
}
```

`EmbeddingsBuilder::new(model)` takes `impl Into<DynModel<op::Embedding>>` — pass the `Model<Embeddings>` directly (or `model.erase()`). Widths: most models self-declare; `Some(ndims)` targets providers whose dimension is chosen per request (e.g. Ollama) — and mismatches surface as `ProviderError::MismatchedDimensions`.

## The `Embed` trait

A type must implement `Embed` to be embedded. Derive it (the `derive` feature, default) or implement manually.

**Derive macro** — mark field(s) to embed with `#[embed]`:

```rust
use rig_core::Embed;

#[derive(Embed)]
struct Foo {
    id: i32,
    #[embed]
    name: String,
}
```

**Manual impl** — push each text fragment through a `TextEmbedder`:

```rust
use rig_core::embeddings::{Embed, EmbedError, TextEmbedder};

struct WordDefinition { id: i32, word: String, definition: String }

impl Embed for WordDefinition {
    fn embed(&self, embedder: &mut TextEmbedder) -> Result<(), EmbedError> {
        embedder.embed(self.definition.to_owned()); // only the definition
        Ok(())
    }
}
```

## Embedding many documents

`.documents(iter)` batches `Embed` values — the builder respects the provider's max batch size (from the wire's `capabilities()`) and handles batching/concurrency:

```rust
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::{self, OpenAI};

let documents = vec![
    Foo { id: 1, name: "Rig".to_string() },
    Foo { id: 2, name: "Playgrounds".to_string() },
];

let model = OpenAI::from_env()?.embedding("text-embedding-3-small", None);

let embeddings = EmbeddingsBuilder::new(model)
    .documents(documents)?
    .build()
    .await?;
```

`.build()` returns a `Vec<(T, Vec<Embedding>)>` (ordered by input, not completion), or `build_with_usage()` internally for token accounting. A document may produce one or many embeddings depending on how its `Embed` impl uses `TextEmbedder`; a document producing no text is an error naming the document. Provider errors surface as `ProviderError`.

## Storing embeddings

Insert into any store implementing `InsertDocuments`:

```rust
use rig_core::vector_store::InsertDocuments;

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

The dimension matters when provisioning a vector store index — it must match the model you embed with (provider-reported widths also let `Some(ndims)`-free embedding lookups rely on model-known widths, e.g. voyageai/gemini model width tables).