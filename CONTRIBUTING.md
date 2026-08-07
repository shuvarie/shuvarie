# Contribute to Shuvarie

## Usage of Large Language Models (LLMs) and AI tools

As an AI tool project, we encourage our contributors to use AI **constructively**, **ethically**, and **educatively**.

### Disclosure of AI usage

If you have used AI to write the code during your contribution **no matter entirely or partly**, please add the following text at the top of the commit description (next line of the commit summary):

```
Assisted-By: <model vendor>/<model name>[, <other models>...] via <tool name>[, <other models and tools>...]
```

For example,

```
Assisted-By: anthropic/Claude-Fable-5 via OpenCode
```

If multiple models are involved within the same tool:

```
Assisted-By: moonshotai/Kimi-K2.7-Code, zai-org/GLM-5.2 via OpenCode
```

If multiple tools are involved:

```
Assisted-By: zai-org/GLM-5.2 via OpenCode, zai-org/GLM-5.2 via Crush
```

This is **not** a hard-line standard (but better to be uniformed), check the org and model IDs on Hugging Face as a reference.
The purpose of this rule is to inform other contributors how you have achieved the result.

Disclosure of the model provider (i.e., OpenRouter, Ollama Cloud, OpenCode Zen, Crush Hyper, etc.) is **not required**.

#### Penalty

If you didn't ...

### Usage of automation
