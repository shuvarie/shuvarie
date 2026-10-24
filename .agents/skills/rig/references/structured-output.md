# Structured Output (Extractors)

An `Extractor` turns unstructured text into a strongly-typed Rust value. Give it a target type and Rig drives an LLM to parse text into that type with type-safe deserialization and minimal boilerplate — useful for pulling entities, fields, or records out of free-form input.

Official docs: https://rig.rs/docs/concepts/extractors · API: https://docs.rs/rig-agent/latest/rig_agent/extractor/index.html (authoritative reference: `rig-agent-0.45.0/src/extractor.rs`, `rig-agent-0.45.0/src/agent/typed.rs`, `rig-core-0.45.0/src/structured_output.rs`).

> **Changed in 0.44** — the structured-output *policy* is shared by the agent runtimes in `rig_core::structured_output` (output-tool name and description, instructions, reprompts, required-field check). A typed run is `AgentRunner<TypedOutput<T>>` and **shares every `AgentRunner` setter** (0.43 forwarded them one by one). `PromptResponse::output()` is a **method derived from `content`** (no `output: String` field), and 0.44 tightened the extractor to match: `StructuredOutputError` is `Prompt` / `Deserialization { output, error }` / `EmptyResponse`, and `ExtractorBuilder::retries` takes `usize`.

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
        .output;    // (the response carries usage, messages and completion calls as well)
    println!("{} is a {}",
        person.name.unwrap_or_default(),
        person.profession.unwrap_or_default());
    Ok(())
}
```

`ExtractorBuilder::new(model)` takes `impl Into<DynModel<Completion>>`. Repoint an extractor later with `Extractor::with_model(model)` (registers it on the extractor's bus) or `with_model_label(label)`.

Use `Option<T>` for fields that may be absent so the model can leave them out cleanly. Keep target structs small and focused for the most reliable extraction.

## How it works

An `Extractor<T>` is an `Agent` in **Tool output mode** with a synthetic output tool, plus a retry budget. `ExtractorBuilder::from_agent_builder` configures the inner agent with a fixed extraction preamble, `.output_schema::<T>()`, `.tool_choice(ToolChoice::Required)` and `.output_mode(OutputMode::Tool)`; every `extract(text)` is then a one-call run (`max_turns(1)`) that names that tool `submit`:

```rust
// what Extractor::extract does, crate-internally
let runner = self.agent.prompt(text).max_turns(1).output_tool(
    "submit",
    "Submit the structured data you extracted from the provided text.",
    /* augment_preamble */ false,
);
TypedRun::output_tool(runner).retries(self.retries)
```

- The synthetic tool's `parameters` are `T`'s schema, generated at compile time via `schemars` — so you get compile-time type checking and automatic schema generation for free.
- The tool is advertised **last** in the turn's tool list, is in the turn's *allowed* names, and is **never executable**: a call to it is the answer, not work. The run finalizes straight away with the call's arguments (serialized JSON) as the response content, and no tool result is ever produced for it.
- Native structured-output providers can constrain the reply via `output_schema` instead; the submit-tool flow is the shape hooks observe. A model that *rejects* a forced tool choice (`ProviderCapabilities::rejects_forced_tool_choice`, checked only for Rig's own output-tool flow) has the force dropped: the tool choice is cleared and a preamble line tells the model to reply with **only** the JSON object when the wire composes native output with tools (the mode switches to `Native`), or to call the tool instead of replying in text otherwise.
- `retries(n)` on the extractor is the **attempt** budget of each `TypedRun<T>`; separately, the run itself has an in-run **output-reprompt** budget (see below).

## Structured-output policy (`rig_core::structured_output`)

The model-facing policy shared by the agent runtimes — one implementation, so no runtime can drift on how an output tool is named, asked for, checked and re-prompted:

| Item | What it does |
|------|--------------|
| `OUTPUT_TOOL_NAME` / `OUTPUT_TOOL_DESCRIPTION` / `AUGMENTATION_SEPARATOR` | `"final_result"`, the synthetic tool's default description, and `"\n\n"` (the join between a preamble and an augmentation) |
| `output_tool_name(is_taken: impl Fn(&str) -> bool) -> String` | reserves the run's output-tool name: `final_result`, then `final_result_1`, … while `is_taken` reports a collision with a real tool |
| `output_tool_augmentation(name) -> String` | the preamble line for Tool mode: call `name` exactly once with the structured result as its arguments, never as plain text |
| `prompted_augmentation(schema: &str) -> String` | the Prompted-mode instruction: one JSON object only, no prose or fences, followed by the schema's canonical rendering |
| `output_tool_callable(choice: Option<&ToolChoice>, name) -> bool` | whether the active tool choice permits calling `name`: no choice, `Auto`, `Required` and a `Specific` set naming it permit it; `None` forbids it |
| `missing_required_fields(schema, arguments) -> Vec<String>` | the schema's top-level `required` names `arguments` lacks, in schema order (a non-object argument lacks every one) |
| `text_satisfies_schema(schema: Option<&Value>, text) -> bool` | whether `text` parses as JSON with every top-level required field (types and other constraints are **not** checked); without a schema, any JSON passes |
| `reprompt_text_answer(name) -> String` | the corrective message when the model answered as text instead of calling the output tool |
| `reprompt_missing_fields(name, missing) -> String` | the corrective message when the output tool was called without required fields |

A runtime needs all of it because the flow is name → advertise → ask → validate → re-prompt: the tool has to be named without colliding with the turn's real tools, advertised only when the tool choice allows calling it, requested in the preamble, checked on return (required fields, or schema-valid text), and re-prompted with identical wording when it is missing.

## Output modes and schema resolution

`rig_agent::run::OutputMode` (re-exported by `agent`):

| Mode | What the request carries |
|------|--------------------------|
| `Auto` *(default)* | resolved per turn to Tool or Native (below) |
| `Tool` | the schema as a synthetic final-answer **tool** appended last to the advertised tools (allowed, never executable); best-effort — callers must validate |
| `Native` | a provider-native structured-output constraint on every turn (`output_schema` on the request); enforcement and composition with tool calls depend on provider capabilities |
| `Prompted` | the schema in the system prompt, final text returned verbatim; callers must extract and validate JSON (prose and markdown may be present) |

`prepare_request` resolves the mode (the `resolve_output_mode` helper is private) and never leaves `PreparedRequest::output_mode` at `Auto`:

1. no schema → `Native` (nothing is constrained);
2. a schema **and** a committed output-tool name → pins `Tool` for the whole run, so a shifting tool set cannot flip the mode mid-run;
3. explicit `Native`/`Prompted` stay as asked; explicit `Tool` falls back to `Native` when `output_tool_callable` is false;
4. `Auto` → `Tool` only with a schema, advertised executable tools, a callable output tool, and a provider that **cannot** compose native output with tools (`ProviderCapabilities::composes_native_output_with_tools`), else `Native`.

In Tool mode the preamble is augmented with `output_tool_augmentation(name)` when `augment_output_preamble` is set, and in Prompted mode with `prompted_augmentation(schema)`; the augmentation is joined to the preamble with `AUGMENTATION_SEPARATOR`. The output tool is added to the request's allowed names (so a call to it is not an invalid tool call) but never to its executable names; a real tool colliding with a pinned output name is a `PrepareError::Request`.

| Where the schema is set | Surface |
|------------------------|---------|
| `RunSpec` (`rig_agent::run`) | `output_schema: Option<serde_json::Value>`, `output_mode: OutputMode`, `output_tool_name: Option<String>`, `output_tool_description: Option<String>`, `augment_output_preamble: bool` (default `true`), plus `DEFAULT_OUTPUT_RETRIES: usize = 1` for the in-run output reprompt budget |
| `AgentBuilder` | `output_schema::<T>()` (the schema for `T`), `output_schema_raw(schemars::Schema)`, `output_mode(OutputMode)` — agent-wide defaults for every run |
| `AgentRunner` | inherits the agent's schema/mode (`Agent::run_spec()`); no public schema/mode setter. `unhandled_invalid_tool_call(UnhandledInvalidToolCall::{Fail, Ignore})` is the public output-related setter; the synthetic output tool is configured crate-internally by `Agent::prompt_typed` and the extractor |
| `AgentRun` (sans-IO) | `with_output_validation(schema, max_output_retries)`, `with_output_tool_name(name)` / `commit_output_tool_name(name)` / `output_tool_name()`, `with_tool_choice(choice)` |

## Adding context and instructions

```rust
let extractor = ExtractorBuilder::<Person>::new(model)
    .append_preamble("Extract person details with high precision.")
    .context("Ages are in years; ignore honorifics like 'Dr.'")
    .retries(2)
    .build();
```

The extraction preamble itself is **fixed** (it tells the model to call the `submit` tool), so `ExtractorBuilder` has `append_preamble(&str)` — **not** `preamble(..)`, which would replace it.

| `ExtractorBuilder<T>` | What it does |
|-----------------------|--------------|
| `new(model)` | an extractor of `T` over `impl Into<DynModel<Completion>>` (builds an `AgentBuilder` internally) |
| `from_agent_builder(builder)` | reuse an existing builder — replaces its preamble, output schema, tool choice (always `ToolChoice::Required`) and output mode (`Tool`) |
| `append_preamble(&str)` | append to the fixed extraction preamble (never replaces it) |
| `context(&str)` | a static context document |
| `dynamic_context(samples, index)` | retrieve up to `samples` documents per call through the agent's dynamic-context hook |
| `additional_params(serde_json::Value)` / `max_tokens(u64)` / `add_hook(H: AgentHook)` | forwarded to the inner `AgentBuilder` |
| `retries(usize)` | the retry budget every `extract` carries (default `0`) |
| `build() -> Extractor<T>` | |

| `Extractor<T>` | Returns |
|----------------|---------|
| `extract(text: impl Into<Message>)` | `TypedRun<T>` — a one-call Tool-mode run over `submit` with this extractor's retry budget |
| `with_model(model)` / `with_model_label(label)` | `Self`, re-pointing this extractor's default model |
| `agent()` | `&Agent` — the agent behind the extractor (history, hooks, model registration) |

## Error handling

A typed run fails with `StructuredOutputError` (`rig_agent::completion`):

| Variant | Meaning |
|---------|---------|
| `Prompt(PromptError)` | the underlying run failed — any `PromptError` (`Provider`, `Report`, `Memory`, `MaxTurns`, `Cancelled`, `UnknownToolCall`) |
| `Deserialization { output: String, error: serde_json::Error }` | the accepted output did not deserialize into the requested type |
| `EmptyResponse` | the model returned no accepted content (for an extractor: it never called `submit`) |

`StructuredOutputError` is the thin wrapper: `#[from] PromptError` makes the run's own failure surface unchanged (and it forwards `provider_response_body()` / `provider_response_json()` / `provider_request_id()` / `provider_response_status()` from it). It is *not* `PromptError` itself — a streamed or awaited run yields `PromptError` directly.

```rust
use rig_agent::completion::StructuredOutputError;

match extractor.extract("...").await {
    Ok(person) => { /* person.output, person.usage, person.messages */ }
    Err(StructuredOutputError::Prompt(err)) => eprintln!("run failed: {err}"),
    Err(StructuredOutputError::Deserialization { output, error }) => {
        eprintln!("{output:?} is not the requested type: {error}")
    }
    Err(StructuredOutputError::EmptyResponse) => eprintln!("the model never called `submit`"),
}
```

Two independent retry budgets:

- **Attempts** — `TypedRun::retries(n)` (or `ExtractorBuilder::retries(n)` for every run it produces). An attempt that errors, produces no output, or produces output that does not parse as `T` is retried from scratch; usage accumulates across attempts, including attempts that were billed but failed to parse (an attempt whose completion call itself errored contributes no usage).
- **Output reprompts (inside one run)** — `RunSpec::DEFAULT_OUTPUT_RETRIES` (`1`) via `AgentRun::with_output_validation(schema, max_output_retries)`. In Tool mode the run re-prompts when the model finalizes without calling the output tool (and its text is not schema-valid JSON), when the output call's arguments are not a JSON object, or when top-level required fields are missing. Each reprompt appends the assistant turn plus the corrective feedback from `reprompt_text_answer` / `reprompt_missing_fields`, consumes a model call, and leaves the run to finalize best-effort once either budget runs out — except arguments that are not a JSON object, which fail the run.

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

## `TypedRun<T>` and `TypedPromptResponse<T>`

```rust
pub type TypedRun<T> = AgentRunner<TypedOutput<T>>;   // TypedOutput<T>: the typed-output marker
```

A `TypedRun<T>` **is** an `AgentRunner`, parameterised by the `TypedOutput<T>` marker: it takes every `AgentRunner` setter (`.history(..)`, `.max_turns(..)`, `.using_model(..)`, `.tool_context(..)`, `.add_hook(..)`, …) plus `retries(usize)`, and `.await`ing it yields `Result<TypedPromptResponse<T>, StructuredOutputError>`. It shares the runner's span semantics: the run belongs to the span it was started in.

| `TypedPromptResponse<T>` | |
|--------------------------|---|
| `output: T` | the parsed value |
| `usage: Usage` | aggregate usage across every attempt |
| `completion_calls: Vec<CompletionCall>` / `completion_calls()` / `requests()` | per-request usage/identity of the accepted attempt |
| `messages: Vec<Message>` | the accepted attempt's transcript (append it to caller-owned history) |
| `memory_append: Option<MemoryAppend>` | **removed in 0.45** — a run backed by a session store persists through its `Writer`; `TypedPromptResponse` carries `output`, `usage`, `completion_calls` and `messages` |
| `new(output, usage)` / `with_completion_calls(calls)` | constructors |

`Agent::prompt_typed::<T>(prompt) -> TypedRun<T>` is the native-mode entry point: the schema for `T` becomes the run's `output_schema`, the mode is `Native`, and the model's final text is parsed as `T` (tolerating prose or fences around the JSON — a direct parse is tried first, then one value starting at the first `{` or `[`).

## Extractor vs. `TypedRun` / `prompt_typed`

- **`Extractor`** wraps an agent + the `submit` output tool specifically for parsing text into a type, with a retry budget and a fixed preamble. Reach for it when structured extraction is the whole job.
- **`TypedRun`** (`agent.prompt_typed::<T>("...").await?.output`) gives the same output-mode machinery directly on an `Agent`, with per-run configuration and every `AgentRunner` setter. Reach for it when structured output is one step in a broader agent workflow.
- Both derive the schema from `T`; set one raw at build time with `AgentBuilder::output_schema_raw(schema)` (or per run through `AgentRun::with_output_validation`) when you control schema generation yourself.

See `agents.md` for agent construction and `tools.md` for how output tools work.
