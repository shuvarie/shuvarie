# Embeddings

An embedding is a vector representation of data — usually text — where semantically similar items map to nearby points. Rig's embeddings system turns your data into vectors so you can power semantic search, similarity comparison, and RAG.

Official docs: https://rig.rs/docs/concepts/embeddings · API: https://docs.rs/rig-core/latest/rig_core/embeddings/index.html

An `Embedding` is an input identifier plus its vector: `document: String` and `vec: Vec<f64>` (still `f64` — not `f32`). Its `PartialEq` compares **only** `document`, so two vectors compare equal whenever they name the same input.

## Minimal example

An embedding model is a **`Model<Embeddings>`** — a client builds one with `.embedding(id, ndims)`. `Embeddings` is the OpenAI-shaped wire; behind it the driver runs the operation `rig_core::operation::Embedding`. Embed text (`String`, which implements `Embed`) values with `EmbeddingsBuilder`:

```rust
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::OpenAI;

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

`EmbeddingsBuilder::new(model)` takes `impl Into<DynModel<op::Embedding>>`: pass the `Model<Embeddings>` directly (`Model<W, T>: Into<DynModel<W::Op>>`, and `Embeddings::Op = Embedding`), or `model.erase()` when you want the erased handle itself (reusable across the builder and a vector index).

There is **no `EmbeddingModel` trait**: a provider's embedding endpoint is a wire (`Wire<Op = Embedding>`) whose request type is plain `Vec<String>`.

| Call | Returns |
|------|---------|
| `Model<Embeddings>::call(vec![text])` | `Result<EmbeddingResponse, ProviderError>` |
| `DynModel<op::Embedding>::call(vec![text])` | `Result<EmbeddingResponse, ProviderError>` |
| `…::embed_text("a document")` | `Result<Embedding, ProviderError>` — the last vector of a one-text batch, or `ProviderError::Response` when the reply carried none |

**Widths.** A model's width resolves caller → the dialect's own `ModelWidth` table (Mistral, Doubleword, …) → the shared identifier table → `0`, rig's "unknown" sentinel. `Some(ndims)` reaches the request only when the dialect has a width field (`dimensions`; Mistral's `output_dimension`) and the value differs from the model's documented native width; a width the dialect cannot honour, and `encoding_format: base64`, are refused before anything is sent. A declared width is what enables the reply check — `Capabilities::declaring(Some(ndims))` makes the first vector whose length disagrees report `ProviderError::MismatchedDimensions { provider, requested, returned }`.

## The `Embed` trait

A type implements `Embed` to be embedded. Derive it (the `derive` feature, default) or implement it manually; either way a value contributes **text fragments in append order**, and extraction failures are reported as `EmbedError`.

**Derive macro** — mark each field to embed with the bare `#[embed]` attribute:

```rust
use rig_core::Embed;

#[derive(Embed)]
struct Foo {
    id: i32,
    #[embed]
    name: String,
}
```

- Structs only, and at least one annotated field — otherwise the derive fails to compile. A field cannot combine `#[embed]` with `#[embed(embed_with = "…")]`, nor carry two `embed_with` attributes; unknown attribute keys and non-string values are compile errors too.
- Fields append in declaration order, **all `#[embed]` fields first, then all `#[embed(embed_with = "…")]` fields** (not interleaved by declaration).
- A generic field gets a `where FieldTy: Embed` bound added for you.
- `#[embed(embed_with = "path::to::fn")]` hands the field to your own extractor, `fn(&mut TextEmbedder, FieldTy) -> Result<(), EmbedError>`, which appends whatever it wants.

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

`TextEmbedder::embed(&mut self, text: String)` appends without filtering or deduplication; `to_texts(item) -> Result<Vec<String>, EmbedError>` runs any `Embed` and returns its fragments in append order. Built-in impls:

| Implementor | Fragment |
|-------------|----------|
| `String`, `&str`, `i8`–`i128`, `f32`, `f64`, `bool`, `char` | its `to_string()` — the integer impls are **signed only** |
| `serde_json::Value` | the compact JSON string, or `EmbedError::new(..)` if serialization fails |
| `&T`, `Vec<T>` | forwards to `T` / each item in order |

## Embedding many documents

`.documents(iter)` batches `Embed` values — the builder chunks by the model's `capabilities().max_documents` and runs up to `1024 / max_documents` batches concurrently, then re-assembles slots so both document order and each document's text order survive out-of-order completion:

```rust
use rig_core::embeddings::EmbeddingsBuilder;
use rig_core::providers::openai::OpenAI;

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

`.build()` returns `Vec<(T, Vec<Embedding>)>` and is `ProviderError`-fallible. A document produces one or many embeddings depending on how its `Embed` impl used `TextEmbedder`; a document that produced **no** text (an `#[embed]` field holding an empty collection embeds nothing) is an error naming the document, as is a batch that returned fewer embeddings than texts sent. Surplus embeddings are ignored, and the builder is `#[must_use]`. `build()` discards the accumulated `Usage` — the token-accounting entry point (`build_with_usage`) is crate-private, so take embedding token counts from provider telemetry instead.

## Normalized replies

Every embedding wire — text or image — decodes into the *same* type, `rig_core::embeddings::EmbeddingResponse`:

| Field | Type | Notes |
|-------|------|-------|
| `embeddings` | `Vec<Embedding>` | one per input, in input order |
| `usage` | `Usage` | every counter `None` when the provider reported none |
| `provider` | `String` | stable descriptor name (`"openai"`); always populated |
| `model` | `Option<String>` | provider-reported id; dropped when reported empty |
| `response_id` | `Option<String>` | provider-assigned, when reported |
| `provider_request_id` | `Option<String>` | transport request id from the reply headers |
| `raw` | `serde_json::Value` | provider payload; `Null` when none was attached |

How a reply gets there:

1. The decoder builds the response through the crate-private `EmbeddingResponse::from_vectors(impl IntoIterator<Item = Vec<f64>>)`, which names each vector's `document` itself — a reply is never trusted to echo its inputs back. (`EmbeddingResponse::new(Vec<Embedding>)` is the public constructor, for hand-built responses.)
2. The driver **stamps** what it learned: `provider`, `provider_request_id` from the reply headers, and `raw`; it also drops an empty `model`/`response_id`. Decoders set what the provider itself reported.
3. The fold pairs each vector positionally with the document the request asked for, rejecting a reply whose vector count disagrees and whose widths contradict a declared `ndims` (both `ProviderError`). Providers must return embeddings in input order within a batch; that is not checked.

> **Changed in 0.44** — one reply type for both modalities. 0.43 had a separate `ImageEmbeddingResponse` for the `ImageEmbedding` operation; 0.44 collapsed it into `EmbeddingResponse`, so text and image embeddings now share one decoder shape, one stamping pass and one error path. `Embedding` itself is unchanged (`vec: Vec<f64>`).

## Storing embeddings

`build()` hands back exactly what a store's `InsertDocuments` wants — `Vec<(Doc, Vec<Embedding>)>`:

```rust
use rig_core::vector_store::InsertDocuments;

qdrant.insert_documents(embeddings).await?;
```

`InsertDocuments::insert_documents<Doc: Serialize + Embed + WasmCompatSend>(Vec<(Doc, Vec<Embedding>)>) -> Result<(), VectorStoreError>`; `vector_store::flatten_embedded` is the shared shape of most implementations (one backend record per embedding, carrying the owning document's serialized form). See `rag-and-vector-stores.md` for how embeddings power retrieval and the supported stores.

## Tool RAG: `ToolSchema`

`rig_core::embeddings::ToolSchema` is what makes a tool retrievable by embedding: `{ name: String, context: serde_json::Value, embedding_docs: Vec<String> }`, built with `ToolSchema::try_from(tool)` from any `&T where T: PortableToolEmbedding` (which supplies `embedding_docs()`, `context()` and `init()`). `ToolSchema` implements `Embed` by appending every `embedding_docs` entry, so a `Vec<ToolSchema>` embeds straight through `EmbeddingsBuilder`; the serialized `context` is what reconstructs the tool afterwards. A tool handler's descriptor carries the same data as `FamilyDescriptor::Tool { embedding: Some(ToolEmbeddingDescriptor { context, embedding_docs }) }`, which is what an agent's own retrieval query is answered from. See `tools.md` for tool authoring/dispatch and `rag-and-vector-stores.md` for the index side.

## Best practices

- **Prepare documents** — clean/normalize text before embedding; chunk large documents into focused pieces (512–1000 tokens or semantic boundaries).
- **Match models** — query embeddings must use the **exact same model id** as the stored documents; vectors from different models aren't comparable.
- **Handle errors** — `Embed::embed` returns `Result<(), EmbedError>`, so extraction is fallible (`EmbedError::new(e)` wraps anything); provider and transport failures surface as `ProviderError`.
- **Batch** — prefer `.documents(...)` for multiple items so Rig batches requests efficiently.

## OpenAI embedding constants

| Constant | Dimensions | Notes |
|----------|-----------:|-------|
| `TEXT_EMBEDDING_3_LARGE` | 3072 | |
| `TEXT_EMBEDDING_3_SMALL` | 1536 | |
| `TEXT_EMBEDDING_ADA_002` | 1536 | legacy |

The dimension matters when provisioning a vector store index — it must match the model you embed with. `model.capabilities().ndims` reports the resolved width (the caller's `ndims`, else the dialect's width table, else these three ids' known widths, else `0` for unknown), which is what the reply check is driven from.
