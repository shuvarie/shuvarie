use futures_util::StreamExt;
use selune::ProviderType;
use serde_json::Value;
use std::sync::Arc;

use crate::message::ChatMsg;
use crate::stream::{StreamItem, StreamStream};
use crate::tool::{DynamicTool, FileChangeHook};
use crate::{LlmError, Result};

#[derive(Debug, Clone)]
pub struct ProviderClient {
    kind: ProviderType,
    base_url: Option<String>,
    list: ListImpl,
}

fn openai_builder(key: &str, base_url: Option<&str>) -> rig::providers::openai::ClientBuilder {
    let builder = rig::providers::openai::Client::builder().api_key(key);
    if let Some(url) = base_url {
        builder.base_url(url)
    } else {
        builder
    }
}

#[derive(Debug, Clone)]
enum ListImpl {
    OpenAi(rig::providers::openai::Client),
    OpenRouter(rig::providers::openrouter::Client),
    Anthropic(rig::providers::anthropic::Client),
    Gemini(rig::providers::gemini::Client),
    Ollama(rig::providers::ollama::Client),
}

impl ProviderClient {
    pub fn build(
        kind: ProviderType,
        api_key: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Self> {
        let base_url = base_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(str::to_string);
        let list = match kind {
            ProviderType::Openai | ProviderType::OpenaiCompat | ProviderType::Vercel => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = openai_builder(key, base_url.as_deref())
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
            ProviderType::Openrouter => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::openrouter::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::OpenRouter(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Anthropic => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::anthropic::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Anthropic(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Google => {
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = rig::providers::gemini::Client::builder().api_key(key);
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Gemini(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Ollama => {
                let client =
                    rig::providers::ollama::Client::builder().api_key(api_key.unwrap_or(""));
                let client = if let Some(url) = base_url.as_deref() {
                    client.base_url(url)
                } else {
                    client
                };
                ListImpl::Ollama(
                    client
                        .build()
                        .map_err(|e| LlmError::Provider(e.to_string()))?,
                )
            }
            ProviderType::Azure | ProviderType::Bedrock | ProviderType::GoogleVertex => {
                // These providers are not yet wired to a dedicated rig client;
                // fall back to an OpenAI-compatible client at the configured URL.
                let key = api_key.ok_or(LlmError::Provider("API key required".into()))?;
                let client = openai_builder(key, base_url.as_deref())
                    .build()
                    .map_err(|e| LlmError::Provider(e.to_string()))?;
                ListImpl::OpenAi(client)
            }
        };
        Ok(Self {
            kind,
            base_url,
            list,
        })
    }

    pub fn kind(&self) -> ProviderType {
        self.kind
    }

    pub fn base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    pub fn supports_embeddings(&self) -> bool {
        matches!(
            self.kind,
            ProviderType::Openai
                | ProviderType::OpenaiCompat
                | ProviderType::Openrouter
                | ProviderType::Google
                | ProviderType::Ollama
                | ProviderType::Vercel
        )
    }

    pub async fn embed(&self, model: &str, dims: usize, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        use rig::client::EmbeddingsClient;
        use rig::embeddings::EmbeddingsBuilder;

        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let embeddings = match &self.list {
            ListImpl::OpenAi(c) => {
                let model = c.embedding_model(model);
                EmbeddingsBuilder::new(model)
                    .documents(texts.to_vec())
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
            }
            ListImpl::OpenRouter(c) => {
                let model = c.embedding_model(model);
                EmbeddingsBuilder::new(model)
                    .documents(texts.to_vec())
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
            }
            ListImpl::Gemini(c) => {
                let model = c.embedding_model(model);
                EmbeddingsBuilder::new(model)
                    .documents(texts.to_vec())
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
            }
            ListImpl::Ollama(c) => {
                let model = c.embedding_model_with_ndims(model, dims);
                EmbeddingsBuilder::new(model)
                    .documents(texts.to_vec())
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
                    .build()
                    .await
                    .map_err(|e| LlmError::Embedding(e.to_string()))?
            }
            _ => {
                return Err(LlmError::Embedding(
                    "provider does not support embeddings".into(),
                ));
            }
        };

        let mut out = Vec::with_capacity(embeddings.len());
        for (_, emb) in embeddings {
            for embedding in emb.iter() {
                out.push(embedding.vec.iter().map(|&v| v as f32).collect());
            }
        }
        Ok(out)
    }

    pub async fn list_models(&self) -> Result<Vec<crate::Model>> {
        use rig::client::ModelListingClient;

        let models = match &self.list {
            ListImpl::OpenAi(c) => c.list_models().await,
            ListImpl::OpenRouter(c) => c.list_models().await,
            ListImpl::Anthropic(c) => c.list_models().await,
            ListImpl::Gemini(c) => c.list_models().await,
            ListImpl::Ollama(c) => c.list_models().await,
        }
        .map_err(|e| LlmError::Model(e.to_string()))?;

        Ok(models.data)
    }

    pub async fn run_worker(
        &self,
        req: &crate::agent::WorkerRequest,
    ) -> std::result::Result<String, String> {
        let user_msg = rig::message::Message::user(req.task.clone());
        let activity_tx = req.activity_tx.clone();
        let usage = Arc::clone(&req.usage);
        let tracker = crate::context_hook::UsageTracker::new();
        let budget = req.context_budget.clone();
        let file_hook = FileChangeHook::new();
        match &self.list {
            ListImpl::OpenAi(c) => {
                let agent = agent_with_tools(
                    c,
                    &req.model,
                    Some(&req.preamble),
                    req.tools.clone(),
                    budget,
                    tracker,
                    file_hook.clone(),
                );
                run_worker_agent(
                    agent,
                    &req.name,
                    user_msg,
                    activity_tx,
                    usage,
                    req.max_turns,
                    file_hook,
                )
                .await
            }
            ListImpl::OpenRouter(c) => {
                let agent = agent_with_tools(
                    c,
                    &req.model,
                    Some(&req.preamble),
                    req.tools.clone(),
                    budget,
                    tracker,
                    file_hook.clone(),
                );
                run_worker_agent(
                    agent,
                    &req.name,
                    user_msg,
                    activity_tx,
                    usage,
                    req.max_turns,
                    file_hook,
                )
                .await
            }
            ListImpl::Anthropic(c) => {
                let agent = agent_with_tools(
                    c,
                    &req.model,
                    Some(&req.preamble),
                    req.tools.clone(),
                    budget,
                    tracker,
                    file_hook.clone(),
                );
                run_worker_agent(
                    agent,
                    &req.name,
                    user_msg,
                    activity_tx,
                    usage,
                    req.max_turns,
                    file_hook,
                )
                .await
            }
            ListImpl::Gemini(c) => {
                let agent = agent_with_tools(
                    c,
                    &req.model,
                    Some(&req.preamble),
                    req.tools.clone(),
                    budget,
                    tracker,
                    file_hook.clone(),
                );
                run_worker_agent(
                    agent,
                    &req.name,
                    user_msg,
                    activity_tx,
                    usage,
                    req.max_turns,
                    file_hook,
                )
                .await
            }
            ListImpl::Ollama(c) => {
                let agent = agent_with_tools(
                    c,
                    &req.model,
                    Some(&req.preamble),
                    req.tools.clone(),
                    budget,
                    tracker,
                    file_hook.clone(),
                );
                run_worker_agent(
                    agent,
                    &req.name,
                    user_msg,
                    activity_tx,
                    usage,
                    req.max_turns,
                    file_hook,
                )
                .await
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn stream(
        &self,
        model: &str,
        preamble: Option<&str>,
        prompt: &str,
        history: &[ChatMsg],
        tools: Vec<DynamicTool>,
        workers: &mut [crate::agent::WorkerAgent],
        max_turns: usize,
        context_budget: Option<crate::context_hook::ContextBudget>,
    ) -> StreamStream {
        use rig::streaming::StreamingChat;

        let tracker = crate::context_hook::UsageTracker::new();
        let tracker_for_hook = tracker.clone();
        let user_msg = rig::message::Message::user(prompt.to_string());
        let rig_history: Vec<rig::message::Message> = history
            .iter()
            .cloned()
            .map(rig::message::Message::from)
            .collect();
        let mut dynamic = tools;
        for worker in workers.iter() {
            dynamic.push(crate::tool::into_dynamic(worker.name(), worker.clone()));
        }
        let worker_names: std::collections::HashSet<String> =
            workers.iter().map(|w| w.name().to_string()).collect();
        let receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>> = workers
            .iter_mut()
            .filter_map(crate::agent::WorkerAgent::take_activity_receiver)
            .collect();
        let file_hook = FileChangeHook::new();

        async fn build(
            agent: rig::agent::Agent,
            prompt: rig::message::Message,
            history: Vec<rig::message::Message>,
            receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>>,
            worker_names: std::collections::HashSet<String>,
            file_hook: FileChangeHook,
            max_turns: usize,
            tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
        ) -> StreamStream {
            let stream = agent
                .stream_chat(prompt, history)
                .max_turns(max_turns)
                .await;
            let mut tool_called = false;
            let mut accumulated = String::new();
            let mut tool_names: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            let mut pending_workers: std::collections::VecDeque<String> =
                std::collections::VecDeque::new();
            let tracker_clone = tracker.clone();
            let main = stream.map(move |item| match item {
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::Text(t),
                )) => {
                    accumulated.push_str(&t.text);
                    StreamItem::Delta { text: t.text }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::Reasoning { reasoning, .. },
                )) => {
                    let text = reasoning.display_text();
                    if text.is_empty() {
                        StreamItem::Delta {
                            text: String::new(),
                        }
                    } else {
                        StreamItem::Reasoning { text }
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::ReasoningDelta { reasoning, .. },
                )) => {
                    if reasoning.is_empty() {
                        StreamItem::Delta {
                            text: String::new(),
                        }
                    } else {
                        StreamItem::Reasoning { text: reasoning }
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig::streaming::StreamedAssistantContent::ToolCall {
                        tool_call,
                        internal_call_id,
                    },
                )) => {
                    tool_called = true;
                    let name = tool_call.function.name.clone();
                    if worker_names.contains(&name) {
                        pending_workers.push_back(name.clone());
                        StreamItem::WorkerStart {
                            name,
                            args: tool_call.function.arguments,
                        }
                    } else {
                        tool_names.insert(internal_call_id, name.clone());
                        StreamItem::ToolStart {
                            name,
                            args: tool_call.function.arguments,
                            worker: None,
                        }
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(
                    rig::streaming::StreamedUserContent::ToolResult {
                        tool_result,
                        internal_call_id,
                    },
                )) => {
                    let mut output = String::new();
                    let mut ok = true;
                    for content in tool_result.content.iter() {
                        if let Some(text) = content.as_text() {
                            if !output.is_empty() {
                                output.push('\n');
                            }
                            output.push_str(text);
                        }
                    }
                    if output.is_empty() {
                        ok = false;
                        output = String::from("(no output)");
                    } else if output.starts_with("{\"error\":") {
                        ok = false;
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&output)
                            && let Some(message) = value.get("error").and_then(Value::as_str)
                        {
                            output = message.to_string();
                        }
                    }
                    let name = tool_names.remove(&internal_call_id);
                    let captured = file_hook.take(&internal_call_id);
                    match name {
                        Some(name) => StreamItem::ToolResult {
                            name,
                            output,
                            ok,
                            worker: None,
                            file_change: captured.file_change,
                            streams: captured.shell,
                        },
                        None => StreamItem::WorkerResult {
                            name: pending_workers.pop_front().unwrap_or_default(),
                            output,
                            ok,
                        },
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::FinalResponse(resp)) => {
                    let text = if accumulated.is_empty() && tool_called {
                        String::new()
                    } else {
                        std::mem::take(&mut accumulated)
                    };
                    StreamItem::Done {
                        text,
                        usage: resp.usage,
                    }
                }
                Ok(rig::agent::MultiTurnStreamItem::CompletionCall(call)) => {
                    tracker_clone.record(call.usage);
                    StreamItem::Delta {
                        text: String::new(),
                    }
                }
                Ok(_) => StreamItem::Delta {
                    text: String::new(),
                },
                Err(e) => {
                    let message = e.to_string();
                    if message.contains(crate::context_hook::OVERFLOW_REASON) {
                        StreamItem::Overflow
                    } else if let Some(failure) = crate::retry::classify_connection_error(&e) {
                        StreamItem::ConnectionError {
                            message,
                            reason: failure.reason,
                        }
                    } else {
                        StreamItem::Error { message }
                    }
                }
            });
            Box::pin(merge_streams(main, receivers))
        }

        match &self.list {
            ListImpl::OpenAi(c) => {
                build(
                    agent_with_tools(
                        c,
                        model,
                        preamble,
                        dynamic,
                        context_budget,
                        tracker_for_hook,
                        file_hook.clone(),
                    ),
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    file_hook,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::OpenRouter(c) => {
                build(
                    agent_with_tools(
                        c,
                        model,
                        preamble,
                        dynamic,
                        context_budget,
                        tracker_for_hook,
                        file_hook.clone(),
                    ),
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    file_hook,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Anthropic(c) => {
                build(
                    agent_with_tools(
                        c,
                        model,
                        preamble,
                        dynamic,
                        context_budget,
                        tracker_for_hook,
                        file_hook.clone(),
                    ),
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    file_hook,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Gemini(c) => {
                build(
                    agent_with_tools(
                        c,
                        model,
                        preamble,
                        dynamic,
                        context_budget,
                        tracker_for_hook,
                        file_hook.clone(),
                    ),
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    file_hook,
                    max_turns,
                    tracker,
                )
                .await
            }
            ListImpl::Ollama(c) => {
                build(
                    agent_with_tools(
                        c,
                        model,
                        preamble,
                        dynamic,
                        context_budget,
                        tracker_for_hook,
                        file_hook.clone(),
                    ),
                    user_msg,
                    rig_history,
                    receivers,
                    worker_names,
                    file_hook,
                    max_turns,
                    tracker,
                )
                .await
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_with_tools<C>(
    client: &C,
    model: &str,
    preamble: Option<&str>,
    dynamic: Vec<DynamicTool>,
    context_budget: Option<crate::context_hook::ContextBudget>,
    tracker: std::sync::Arc<crate::context_hook::UsageTracker>,
    file_hook: FileChangeHook,
) -> rig::agent::Agent
where
    C: rig::client::CompletionClient + rig::prelude::AgentClientExt,
    C::CompletionModel: 'static,
{
    let builder = match preamble {
        Some(p) => client.agent(model).preamble(p),
        None => client.agent(model).without_preamble(),
    };
    if let Some(budget) = context_budget {
        builder
            .dynamic_tools(dynamic)
            .add_hook(crate::context_hook::ContextHook::new(budget, tracker))
            .add_hook(file_hook)
            .build()
    } else {
        builder.dynamic_tools(dynamic).add_hook(file_hook).build()
    }
}

async fn run_worker_agent(
    agent: rig::agent::Agent,
    name: &str,
    prompt: rig::message::Message,
    activity_tx: tokio::sync::mpsc::Sender<StreamItem>,
    usage: std::sync::Arc<std::sync::Mutex<crate::TokenUsage>>,
    max_turns: usize,
    file_hook: FileChangeHook,
) -> std::result::Result<String, String> {
    use rig::streaming::StreamingChat;

    let mut stream = agent
        .stream_chat(prompt, Vec::<rig::message::Message>::new())
        .max_turns(max_turns)
        .await;

    let mut text = String::new();
    let mut usage_aggregate = crate::TokenUsage::default();
    let mut tool_names: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    while let Some(item) = stream.next().await {
        match item {
            Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig::streaming::StreamedAssistantContent::Text(t),
            )) => {
                text.push_str(&t.text);
            }
            Ok(rig::agent::MultiTurnStreamItem::StreamAssistantItem(
                rig::streaming::StreamedAssistantContent::ToolCall {
                    tool_call,
                    internal_call_id,
                },
            )) => {
                tool_names.insert(internal_call_id, tool_call.function.name.clone());
                let _ = activity_tx
                    .send(StreamItem::ToolStart {
                        name: tool_call.function.name,
                        args: tool_call.function.arguments,
                        worker: Some(name.to_string()),
                    })
                    .await;
            }
            Ok(rig::agent::MultiTurnStreamItem::StreamUserItem(
                rig::streaming::StreamedUserContent::ToolResult {
                    tool_result,
                    internal_call_id,
                },
            )) => {
                let tool_name = tool_names.remove(&internal_call_id).unwrap_or_default();
                let captured = file_hook.take(&internal_call_id);
                let mut output = String::new();
                let mut ok = true;
                for content in tool_result.content.iter() {
                    if let Some(text) = content.as_text() {
                        if !output.is_empty() {
                            output.push('\n');
                        }
                        output.push_str(text);
                    }
                }
                if output.is_empty() {
                    ok = false;
                    output = String::from("(no output)");
                } else if output.starts_with("{\"error\":") {
                    ok = false;
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&output)
                        && let Some(message) = value.get("error").and_then(Value::as_str)
                    {
                        output = message.to_string();
                    }
                }
                let _ = activity_tx
                    .send(StreamItem::ToolResult {
                        name: tool_name,
                        output,
                        ok,
                        worker: Some(name.to_string()),
                        file_change: captured.file_change,
                        streams: captured.shell,
                    })
                    .await;
            }
            Ok(rig::agent::MultiTurnStreamItem::FinalResponse(resp)) => {
                usage_aggregate = resp.usage;
            }
            Ok(_) => {}
            Err(e) => {
                let message = format!("worker '{name}' failed: {e}");
                let _ = activity_tx
                    .send(StreamItem::Error {
                        message: message.clone(),
                    })
                    .await;
                return Err(message);
            }
        }
    }
    {
        let mut guard = usage.lock().unwrap();
        guard.input_tokens += usage_aggregate.input_tokens;
        guard.output_tokens += usage_aggregate.output_tokens;
        guard.total_tokens += usage_aggregate.total_tokens;
        guard.cached_input_tokens += usage_aggregate.cached_input_tokens;
        guard.reasoning_tokens += usage_aggregate.reasoning_tokens;
    }
    Ok(text)
}

fn merge_streams(
    main: impl futures_core::Stream<Item = StreamItem> + Send + 'static,
    receivers: Vec<tokio::sync::mpsc::Receiver<StreamItem>>,
) -> StreamStream {
    use futures_util::stream::select_all;
    use std::pin::Pin;

    let mut streams: Vec<Pin<Box<dyn futures_core::Stream<Item = StreamItem> + Send>>> = Vec::new();
    streams.push(Box::pin(main));
    for rx in receivers {
        streams.push(Box::pin(tokio_rx_stream(rx)));
    }
    Box::pin(select_all(streams))
}

fn tokio_rx_stream(
    rx: tokio::sync::mpsc::Receiver<StreamItem>,
) -> impl futures_core::Stream<Item = StreamItem> {
    use futures_util::stream::unfold;
    unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::Tool;
    use futures_util::StreamExt;
    use serde_json::json;

    fn pending_stream() -> impl futures_core::Stream<Item = StreamItem> + Send + 'static {
        futures_util::stream::pending::<StreamItem>()
    }

    #[tokio::test]
    async fn merge_streams_yields_main_and_worker_items() {
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let main_stream = futures_util::stream::iter(vec![StreamItem::Delta {
            text: "main".into(),
        }]);
        let mut merged = merge_streams(main_stream, vec![worker_rx]);
        let worker_items = vec![
            StreamItem::ToolStart {
                name: "read_file".into(),
                args: json!({ "path": "x.rs" }),
                worker: Some("explore_workspace".into()),
            },
            StreamItem::ToolResult {
                name: "read_file".into(),
                output: "ok".into(),
                ok: true,
                worker: Some("explore_workspace".into()),
                file_change: None,
                streams: None,
            },
        ];
        let _ = worker_tx.send(worker_items[0].clone()).await;
        let _ = worker_tx.send(worker_items[1].clone()).await;
        drop(worker_tx);
        let mut collected = Vec::new();
        while let Some(item) = merged.next().await {
            collected.push(item);
        }
        assert_eq!(collected.len(), 3, "main delta + two worker items");
        assert!(collected.contains(&worker_items[0]));
        assert!(collected.contains(&worker_items[1]));
        assert!(collected.contains(&StreamItem::Delta {
            text: "main".into()
        }));
    }

    #[tokio::test]
    async fn merge_streams_drains_workers_after_main_done() {
        // `select_all` is round-robin but polls the main stream first, so a
        // worker item queued at the moment `Done` arrives is yielded after
        // it. The merged stream must keep yielding until the receiver
        // drains — the core only sends `Event::StreamDone` once it returns
        // `None`.
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let done = StreamItem::Done {
            text: "final".into(),
            usage: crate::TokenUsage::default(),
        };
        let worker_item = StreamItem::ToolResult {
            name: "grep".into(),
            output: "found".into(),
            ok: true,
            worker: Some("explore_workspace".into()),
            file_change: None,
            streams: None,
        };
        let _ = worker_tx.send(worker_item.clone()).await;
        drop(worker_tx);
        let mut merged = merge_streams(
            futures_util::stream::iter(vec![done.clone()]),
            vec![worker_rx],
        );
        let mut collected = Vec::new();
        while let Some(item) = merged.next().await {
            collected.push(item);
        }
        assert_eq!(
            collected,
            vec![done, worker_item],
            "worker items must trail Done, never be dropped"
        );
    }

    #[tokio::test]
    async fn merge_streams_emits_worker_items_even_when_main_pending() {
        let (worker_tx, worker_rx) = tokio::sync::mpsc::channel(8);
        let mut merged = merge_streams(pending_stream(), vec![worker_rx]);
        let item = StreamItem::WorkerStart {
            name: "run_tests".into(),
            args: json!({ "task": "run tests" }),
        };
        let _ = worker_tx.send(item.clone()).await;
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), merged.next())
            .await
            .expect("worker item should arrive")
            .expect("stream should not end");
        assert_eq!(got, item);
    }

    #[tokio::test]
    async fn worker_missing_task_returns_error() {
        let client = ProviderClient::build(selune::ProviderType::Ollama, None, None).unwrap();
        let (_activity_tx, _activity_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
        let usage = Arc::new(std::sync::Mutex::new(crate::TokenUsage::default()));
        let worker = crate::agent::WorkerAgent::new(
            "test_worker",
            "a test worker",
            "you are a test worker",
            client,
            "test-model",
            Vec::new(),
            usage,
            10,
            None,
        );
        let err = worker
            .call(&mut crate::tool::ToolContext::new(), json!({}))
            .await
            .expect_err("missing task should fail");
        assert!(
            err.to_string().contains("task"),
            "error should mention task: {err}"
        );
    }

    #[test]
    fn supports_embeddings_by_provider() {
        use selune::ProviderType::*;
        let ok = [Openai, OpenaiCompat, Openrouter, Google, Ollama, Vercel];
        let no = [Anthropic, Azure, Bedrock, GoogleVertex];
        for p in ok {
            let client = ProviderClient::build(p, Some("k"), None).unwrap();
            assert!(
                client.supports_embeddings(),
                "{p:?} should support embeddings"
            );
        }
        for p in no {
            let client = ProviderClient::build(p, Some("k"), None).unwrap();
            assert!(!client.supports_embeddings(), "{p:?} should not");
        }
    }

    #[tokio::test]
    async fn embed_unsupported_provider_errors() {
        let client =
            ProviderClient::build(selune::ProviderType::Anthropic, Some("k"), None).unwrap();
        let err = client
            .embed("some-model", 768, &["hello".to_string()])
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Embedding(_)));
    }

    #[tokio::test]
    async fn embed_empty_input_returns_empty() {
        let client = ProviderClient::build(selune::ProviderType::Ollama, None, None).unwrap();
        let out = client.embed("m", 384, &[]).await.unwrap();
        assert!(out.is_empty());
    }
}
