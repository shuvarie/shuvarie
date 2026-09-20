# Contribute to Shuvarie

Thank you for considering contributing to Shuvarie.

Shuvarie is a terminal-first AI tool (or so-called "harness") that prioritizes an efficient and enjoyable AI workflow experience.

## Project Structure

### Branches

- `main`: the development/trunk branch.
- `rel/<version>`: release branch, for patching, tagging, and releasing a specific minor version. `rel` branches are created only for patching without leaking `main` features in.
- `feat/*`: feature branch, for new feature development or bugfixes that require a longer period of time.

When you submit a pull request (PR), it should go to `main`. The commits (especially bugfixes) might get cherrypicked into specific `rel` branch.

### Crates

- `.`: Workspace + TUI crate
- `crates/*`: Module crates
- `vendor/`/`vendor-debug/`: Dependencies as Git submodules

## Conventions

### Commit Summaries

In a feature branch, you can name them whatever you want.

However, in `main` or a `rel` branch, or a pull request, a commit summary is expected to follow the [Conventional Commit style](https://www.conventionalcommits.org/en/v1.0.0/).

### Language

Always use English for description, comments, and UI text, unless for special needs.

Usage of other languages will probably lead to immediate close/reject, even if the maintainer understands the language.

## Develop with an LLM

As an AI tool project, we encourage our contributors to use AI **constructively**, **pragmatically**, and **educatively**.

TLDR: **You must know what you're doing.**

### Disclosure of AI usage

If you have used AI to write the code during your contribution **no matter entirely or partly**,
please add the following text at the top of the commit description (next line of the commit summary) and your pull request:

```
Assisted-By: <model vendor>/<model name>[, <other models>...] via <tool name>[, <other models and tools>...]
```

For example,

```
Assisted-By: anthropic/Claude-Fable-5 via OpenCode
```

If multiple models are involved within the same tool:

```
Assisted-By: moonshotai/Kimi-K3, zai-org/GLM-5.3-Flash via Shuvarie
```

If multiple tools are involved:

```
Assisted-By: zai-org/GLM-5.3 via OpenCode, zai-org/GLM-5.3 via Crush
```

This is **not** a hard-line standard (but better to be uniformed), check the org and model IDs on Hugging Face as a reference.
The purpose of this rule is to inform other contributors how you have achieved the result.

Disclosure of the model provider (i.e., OpenRouter, Ollama Cloud, OpenCode Zen, Crush Hyper, etc.) is **not required**.

### Automated Issue/PR Submission

Issues/PRs submitted by an LLM without human review or supervision is **prohibited**.

In addition, pull requests about refactoring the codebase will **always** get rejected without any review process.

## Response to Rule Violation

If you ignore this document twice, or if you spam the tracker with agent-generated issues, your GitHub account will be permanently blocked.
