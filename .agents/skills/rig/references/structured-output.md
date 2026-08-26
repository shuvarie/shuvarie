# Structured Output (Extractors)

An `Extractor` turns unstructured text into a strongly-typed Rust value. Give it a target type and Rig drives an LLM to parse text into that type with type-safe deserialization and minimal boilerplate — useful for pulling entities, fields, or records out of free-form input.

Official docs: https://rig.rs/docs/concepts/extractors · API: https://docs.rs/rig/latest/rig/extractor/index.html

## Minimal example

Target type must derive `serde::Deserialize`, `serde::Serialize`, and `schemars::JsonSchema`:

```rust
use rig::client::ProviderClient;
use rig::providers::openai;

#[derive(serde::Deserialize, serde::Serialize, rig::schemars::JsonSchema)]
struct Person {
    name: Option<String>,
    age: Option<u8>,
    profession: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = openai::Client::from_env()?;
    let extractor = openai.extractor::<Person>("gpt-5.5").build();
    let person = extractor
        .extract("John Doe is a 30 year old doctor.")
        .await?;
    println!("{} is a {}",
        person.name.unwrap_or_default(),
        person.profession.unwrap_or_default());
    Ok(())
}
```

Use `Option<T>` for fields that may be absent so the model can leave them out cleanly. Keep target structs small and focused for the most reliable extraction.

## How it works

Under the hood an `Extractor` combines an `Agent` with a private "submit" `Tool` whose arguments are your target type. Rig generates a JSON schema from your struct (via `schemars`), the model calls the submit tool with data matching that schema, and Rig deserializes the tool arguments back into your type. Because the schema is derived at compile time, you get compile-time type checking and automatic schema generation for free.

## Adding context and instructions

```rust
let extractor = openai
    .extractor::<Person>("gpt-5.5")
    .preamble("Extract person details with high precision.")
    .context("Ages are in years; ignore honorifics like 'Dr.'")
    .build();
```

## Error handling

`extract` returns `ExtractionError`, which distinguishes:

- `NoData` — model never called the submit tool; nothing was extracted.
- `DeserializationError` — submitted JSON didn't match your type.
- `CompletionError` — the underlying completion request failed.
- `PromptError` — a prompt-level failure (`MaxTurnsError`, `PromptCancelled`, `UnknownToolCall`, …).

```rust
use rig::extractor::ExtractionError;

match extractor.extract("...").await {
    Ok(person) => { /* use person */ }
    Err(ExtractionError::NoData) => eprintln!("Model produced no structured data"),
    Err(err) => return Err(err.into()),
}
```

> `NoData` usually means the model was too weak to reliably call the submit tool. Prefer a more capable model for extraction-heavy workloads.

Extraction also tracks usage: use `extract_with_usage(...)` / `extract_with_chat_history_with_usage(...)` to get an `ExtractionResponse { data, usage }` (usage accumulates across retry attempts). In 0.42 `Extractor`/`ExtractorBuilder` are model-agnostic (no `<M>` type parameter) — `.extract(...)` takes `impl Into<Message>`, and chat history is passed as `Vec<Message>`.

## Batch processing

Extractors are cheap to reuse across many inputs — build once, extract in a loop:

```rust
use rig::completion::CompletionModel;
use rig::extractor::{Extractor, ExtractionError};

async fn process_documents(
    extractor: &Extractor<Person>,
    docs: Vec<String>,
) -> Vec<Result<Person, ExtractionError>> {
    let mut results = Vec::new();
    for doc in docs {
        results.push(extractor.extract(&doc).await);
    }
    results
}
```

Feed extractors from document loaders:

```rust
use rig::loaders::FileLoader;

let docs = FileLoader::with_glob("*.txt")?.read().ignore_errors();
let extractor = openai.extractor::<Person>("gpt-5.5").build();
for doc in docs {
    let structured = extractor.extract(&doc).await?;
    // process structured
}
```

## Extractor vs. `TypedPrompt`

- **`Extractor`** wraps an agent + a submit tool specifically for parsing text into a type. Reach for it when structured extraction is the whole job.
- **`TypedPrompt`** (`agent.prompt_typed("...").await?`) gives the same typed-output behavior directly on an existing `Agent`. Reach for it when structured output is one step in a broader agent workflow.

See `completions.md` for `TypedPrompt` and `tools.md` for how the submit tool underneath extractors works.