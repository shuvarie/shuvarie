# Structured Output (Extractors)

An `Extractor` turns unstructured text into a strongly-typed Rust value. Give it a target type and Rig drives an LLM to parse text into that type with type-safe deserialization and minimal boilerplate — useful for pulling entities, fields, or records out of free-form input.

Official docs: https://rig.rs/docs/concepts/extractors · API: https://docs.rs/rig-agent/latest/rig_agent/extractor/index.html (0.43: `rig-agent-0.43.0/src/extractor.rs`)

## Minimal example

Target type must derive `serde::Deserialize`, `serde::Serialize`, and `schemars::JsonSchema`:

```rust
use rig_agent::extractor::ExtractorBuilder;
use rig_core::providers::openai::{self, OpenAI};

#[derive(serde::Deserialize, serde::Serialize, rig_core::schemars::JsonSchema)]
struct Person {
    name: Option<String>,
    age: Option<u8>,
    profession: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let openai = OpenAI::from_env()?;
    let extractor = ExtractorBuilder::<Person>::new(openai.completion(openai::GPT_5_2)).build();
    let person = extractor
        .extract("John Doe is a 30 year old doctor.")
        .await?
        .output;    // (the response carries usage as well)
    println!("{} is a {}",
        person.name.unwrap_or_default(),
        person.profession.unwrap_or_default());
    Ok(())
}
```

`ExtractorBuilder::new(model)` takes `impl Into<DynModel<Completion>>`. Swap defaults later with `Extractor::with_model(model)` / `with_model_label(label)`.

Use `Option<T>` for fields that may be absent so the model can leave them out cleanly. Keep target structs small and focused for the most reliable extraction.

## How it works

An `Extractor` is an `Agent` in **output-tool mode**: every `extract(..)` is a one-call `TypedRun` with a synthetic "submit" output tool whose schema is your type's (generated via `schemars`), `.retries(n)` bounding response retries. Native structured-output providers constrain via `output_schema`; the submit-tool flow is the fallback (and the shape hooks observe). Because the schema is derived at compile time, you get compile-time type checking and automatic schema generation for free.

## Adding context and instructions

```rust
let extractor = ExtractorBuilder::<Person>::new(model)
    .preamble("Extract person details with high precision.")
    .context("Ages are in years; ignore honorifics like 'Dr.'")
    .retries(2)
    .build();
```

## Error handling

`extract` returns `StructureError`-shaped failures via `StructuredOutputError` (`rig_agent::completion`), which distinguishes:

- `PromptError(..)` — the underlying run failed (wraps `PromptError` — `CompletionError(ProviderError)`, `MaxTurnsError`, `PromptCancelled`, `UnknownToolCall`, …).
- `DeserializationError(..)` — submitted JSON didn't match your type.
- `EmptyResponse` — model accepted a response with no extractable content.

```rust
use rig_agent::completion::StructuredOutputError;

match extractor.extract("...").await {
    Ok(person) => { /* person.output / person.usage */ }
    Err(e) => return Err(e.into()),
}
```

> Empty responses usually mean the model was too weak to call the submit tool reliably. Prefer a more capable model for extraction-heavy workloads.

## Batch processing

Extractors are cheap to reuse across many inputs — build once, extract in a loop:

```rust
use rig_agent::extractor::Extractor;

async fn process_documents(
    extractor: &Extractor<Person>,
    docs: Vec<String>,
) -> Vec<Result<Person, Box<dyn std::error::Error>>> {
    let mut results = Vec::new();
    for doc in docs {
        results.push(match extractor.extract(&doc).await {
            Ok(response) => Ok(response.output),
            Err(e) => Err(e.into()),
        });
    }
    results
}
```

Feed extractors from document loaders (`rig_core::loaders`):

```rust
use rig_core::loaders::FileLoader;

let docs = FileLoader::with_glob("*.txt")?.read().ignore_errors();
for doc in docs {
    let structured = extractor.extract(&doc).await?;
    // process structured
}
```

## Extractor vs. `TypedRun` / `prompt_typed`

- **`Extractor`** wraps an agent + a submit output tool specifically for parsing text into a type, with a retry budget. Reach for it when structured extraction is the whole job.
- **`TypedRun`** (`agent.prompt_typed::<T>("...").await?.output`) gives the same output-mode behavior directly on an `Agent` with per-run configuration. Reach for it when structured output is one step in a broader agent workflow.
- Both can set the schema raw (`AgentBuilder::output_schema_raw(schema)`) when you control schema generation yourself; `Agent::prompt_typed::<T>(..)` derives it from `T` behind the scenes.

See `agents.md` for agent construction and `tools.md` for how output tools work.