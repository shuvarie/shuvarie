use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::AbortHandle;

use shuvarie_db::Store;
use shuvarie_llm::ProviderClient;

use crate::command::Command;
use crate::config::Config;
use crate::connections::{Connections, ProviderConfig};
use crate::embeddings::{self, EmbeddingSetup};
use crate::event::Event;
use crate::session::Session;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    config: Config,
    mut connections: Connections,
    mut store: Store,
    load_current: bool,
    config_path: Option<PathBuf>,
    connections_path: Option<PathBuf>,
    mut cmd_rx: Receiver<Command>,
    event_tx: Sender<Event>,
) {
    let mut clients: HashMap<String, ProviderClient> = HashMap::new();
    let mut session: Option<Arc<Mutex<Session>>> = None;
    let mut active_stream: Option<AbortHandle> = None;
    let mut semantic_search: Option<AbortHandle> = None;

    let embedding_setup = embeddings::setup(&config, &connections, &mut clients);
    if let Some(setup) = embedding_setup.clone() {
        let store_backfill = store.clone();
        tokio::spawn(async move {
            embeddings::backfill(&mut store_backfill.clone(), &setup).await;
        });
    }

    if load_current {
        load_most_recent_session(&mut store, &mut session, &event_tx).await;
    }

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Ping => {
                let _ = event_tx.send(Event::Pong).await;
            }
            Command::ListModels { provider_name } => {
                let client = match client_for(&mut clients, &mut connections, &provider_name) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::ModelsError {
                                provider_name,
                                error: e,
                            })
                            .await;
                        continue;
                    }
                };
                match client.list_models().await {
                    Ok(mut models) => {
                        let provider = client.kind();
                        for model in &mut models {
                            shuvarie_catalog::enrich(provider, model);
                        }
                        let _ = event_tx
                            .send(Event::ModelsLoaded {
                                provider_name,
                                models,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::ModelsError {
                                provider_name,
                                error: e.to_string(),
                            })
                            .await;
                    }
                }
            }
            Command::AddProvider { name, config: pc } => {
                connections.providers.insert(name.clone(), pc);
                clients.remove(&name);
                persist(
                    &config,
                    &connections,
                    config_path.as_deref(),
                    connections_path.as_deref(),
                    &event_tx,
                )
                .await;
            }
            Command::RemoveProvider { name } => {
                connections.providers.remove(&name);
                clients.remove(&name);
                if connections.active_provider.as_deref() == Some(name.as_str()) {
                    connections.active_provider = None;
                    connections.active_model = None;
                }
                persist(
                    &config,
                    &connections,
                    config_path.as_deref(),
                    connections_path.as_deref(),
                    &event_tx,
                )
                .await;
            }
            Command::SetActiveProvider { name } => {
                if connections.providers.contains_key(&name) {
                    connections.active_provider = Some(name.clone());
                    if !clients.contains_key(&name)
                        && let Some(pc) = connections.providers.get(&name)
                        && let Ok(client) = build_client(pc)
                    {
                        clients.insert(name.clone(), client);
                    }
                    persist(
                        &config,
                        &connections,
                        config_path.as_deref(),
                        connections_path.as_deref(),
                        &event_tx,
                    )
                    .await;
                }
            }
            Command::SetActiveModel { model } => {
                connections.active_model = Some(model);
                persist(
                    &config,
                    &connections,
                    config_path.as_deref(),
                    connections_path.as_deref(),
                    &event_tx,
                )
                .await;
            }
            Command::SaveConfig => {
                persist(
                    &config,
                    &connections,
                    config_path.as_deref(),
                    connections_path.as_deref(),
                    &event_tx,
                )
                .await;
            }
            Command::StartSession => {
                session = Some(Arc::new(Mutex::new(Session::new())));
                let _ = event_tx.send(Event::SessionStarted).await;
            }
            Command::NewSession => {
                if stream_busy(&active_stream, &event_tx).await {
                    continue;
                }
                session = Some(Arc::new(Mutex::new(Session::new())));
                let _ = event_tx.send(Event::SessionStarted).await;
            }
            Command::SendMessage { content } => {
                if active_stream.as_ref().is_some_and(|h| !h.is_finished()) {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: "a reply is already streaming".into(),
                        })
                        .await;
                    continue;
                }
                active_stream = None;
                if session.is_none() {
                    session = Some(Arc::new(Mutex::new(Session::new())));
                    let _ = event_tx.send(Event::SessionStarted).await;
                }
                let s = session.as_ref().unwrap();
                s.lock().await.push_user(content.clone());

                {
                    let mut guard = s.lock().await;
                    if guard.id.is_none() {
                        let title = title_for(&content);
                        match store
                            .create_session(
                                &title,
                                connections.active_provider.as_deref(),
                                connections.active_model.as_deref(),
                            )
                            .await
                        {
                            Ok(id) => {
                                guard.id = Some(id);
                                guard.title = Some(title.clone());
                                let _ = event_tx.send(Event::SessionCreated { id, title }).await;
                            }
                            Err(e) => {
                                let _ = event_tx
                                    .send(Event::StreamError {
                                        error: format!("failed to create session: {e}"),
                                    })
                                    .await;
                                continue;
                            }
                        }
                    }
                    let id = guard.id.unwrap();
                    let seq = guard.messages.len() - 1;
                    let msg = store
                        .append_message(id, guard.messages.last().unwrap().role, &content)
                        .await;
                    match msg {
                        Ok(msg) => {
                            if let Some(setup) = &embedding_setup {
                                let store_idx = store.clone();
                                let setup_idx = setup.clone();
                                let content_idx = msg.content.clone();
                                tokio::spawn(async move {
                                    let _ = embeddings::index_message(
                                        &mut store_idx.clone(),
                                        &setup_idx,
                                        msg.id,
                                        id,
                                        seq as u64,
                                        &content_idx,
                                    )
                                    .await;
                                });
                            }
                        }
                        Err(e) => {
                            let _ = event_tx
                                .send(Event::StreamError {
                                    error: format!("failed to persist message: {e}"),
                                })
                                .await;
                            continue;
                        }
                    }
                }

                let Some(provider_name) = connections.active_provider.clone() else {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: "no active provider".into(),
                        })
                        .await;
                    continue;
                };
                let Some(model) = connections.active_model.clone() else {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: "no active model".into(),
                        })
                        .await;
                    continue;
                };

                let client = match client_for(&mut clients, &mut connections, &provider_name) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = event_tx.send(Event::StreamError { error: e }).await;
                        continue;
                    }
                };

                let prior: Vec<shuvarie_llm::ChatMsg> = {
                    let guard = s.lock().await;
                    guard.messages[..guard.messages.len().saturating_sub(1)].to_vec()
                };
                let loaded_context = crate::context::load_from_cwd();
                if !loaded_context.is_empty() {
                    let _ = event_tx
                        .send(Event::ContextLoaded {
                            paths: loaded_context.files.clone(),
                        })
                        .await;
                }
                let preamble = crate::context::build_preamble(AGENT_PREAMBLE, &loaded_context);
                let tools = crate::tools::all_tools();
                let mut worker_set = crate::agents::build_workers(client.clone(), &model);
                let stream = client
                    .stream(
                        &model,
                        Some(&preamble),
                        &content,
                        &prior,
                        &tools,
                        &mut worker_set.workers,
                    )
                    .await;
                let tx = event_tx.clone();
                let session_shared = s.clone();
                let client_shared = client.clone();
                let store_shared = store.clone();
                let model_shared = model.clone();
                let worker_usage = worker_set.usage;
                let embedding_shared = embedding_setup.clone();
                active_stream = Some(
                    tokio::spawn(async move {
                        stream_stream_to_events(
                            stream,
                            session_shared,
                            client_shared,
                            store_shared,
                            model_shared,
                            worker_usage,
                            embedding_shared,
                            tx,
                        )
                        .await;
                    })
                    .abort_handle(),
                );
            }
            Command::CancelStream => {
                if let Some(handle) = active_stream.take()
                    && !handle.is_finished()
                {
                    handle.abort();
                    let _ = event_tx.send(Event::StreamCancelled).await;
                }
            }
            Command::ListSessions => match store.list_sessions().await {
                Ok(sessions) => {
                    let _ = event_tx.send(Event::SessionsLoaded { sessions }).await;
                }
                Err(e) => {
                    let _ = event_tx
                        .send(Event::SessionError {
                            error: e.to_string(),
                        })
                        .await;
                }
            },
            Command::LoadSession { id } => {
                if stream_busy(&active_stream, &event_tx).await {
                    continue;
                }
                match store.load_session(id).await {
                    Ok(stored) => {
                        let loaded = Session::from_stored(stored);
                        session = Some(Arc::new(Mutex::new(loaded.clone())));
                        let _ = event_tx
                            .send(Event::SessionLoaded {
                                id,
                                title: loaded.title.clone().unwrap_or_default(),
                                session: loaded,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::SessionError {
                                error: e.to_string(),
                            })
                            .await;
                    }
                }
            }
            Command::DeleteSession { id } => {
                if stream_busy(&active_stream, &event_tx).await {
                    continue;
                }
                match store.delete_session(id).await {
                    Ok(()) => {
                        if let Some(s) = &session
                            && s.lock().await.id == Some(id)
                        {
                            *s.lock().await = Session::new();
                        }
                        let _ = event_tx.send(Event::SessionDeleted { id }).await;
                    }
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::SessionError {
                                error: e.to_string(),
                            })
                            .await;
                    }
                }
            }
            Command::SearchHistory { query } => {
                let query = query.trim().to_string();
                if query.is_empty() {
                    let _ = event_tx.send(Event::SearchResults { hits: vec![] }).await;
                    continue;
                }
                let mut fts_hits = match store.search_messages(&query, SEARCH_LIMIT).await {
                    Ok(hits) => hits,
                    Err(e) => {
                        let _ = event_tx
                            .send(Event::SearchError {
                                error: e.to_string(),
                            })
                            .await;
                        continue;
                    }
                };
                if let Some(setup) = &embedding_setup {
                    if let Some(handle) = semantic_search.take() {
                        handle.abort();
                    }
                    let store_sem = store.clone();
                    let setup_sem = setup.clone();
                    let tx_sem = event_tx.clone();
                    let fts_sem = std::mem::take(&mut fts_hits);
                    let query_sem = query.clone();
                    semantic_search = Some(
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                            let texts = vec![query_sem];
                            let vecs = setup_sem
                                .client
                                .embed(&setup_sem.model, setup_sem.dims, &texts)
                                .await;
                            let Ok(mut vecs) = vecs else { return };
                            let Some(vec) = vecs.pop() else { return };
                            let Ok(semantic_hits) =
                                store_sem.clone().semantic_search(vec, SEARCH_LIMIT).await
                            else {
                                return;
                            };
                            let merged = embeddings::rrf_merge(
                                fts_sem,
                                semantic_hits,
                                60,
                                SEARCH_LIMIT as usize,
                            );
                            let _ = tx_sem.send(Event::SearchResults { hits: merged }).await;
                        })
                        .abort_handle(),
                    );
                }
                let _ = event_tx.send(Event::SearchResults { hits: fts_hits }).await;
            }
        }
    }
}

fn title_for(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        "Untitled session".to_string()
    } else {
        trimmed.chars().take(48).collect()
    }
}

const SEARCH_LIMIT: u64 = 50;

const AGENT_PREAMBLE: &str = "\
You are Shuvarie, an agentic coding assistant running in a terminal inside the user's project. \
You can read, write, and edit files, list directories, grep for text, and run commands. \
Prefer using tools to inspect the workspace and verify your work (for example, run the test \
suite after editing code) instead of guessing. When a tool reports an error, fix the cause and \
retry rather than stopping. After finishing the work, summarize what you did and any results in \
a short reply. Keep the reply concise.

You can also delegate work to three specialist worker agents, exposed as tools:
- explore_workspace: locates, reads, and summarizes existing code (list/read/grep). Use it for \
  research and understanding before changes.
- run_tests: runs the project's build, test, and lint commands and iterates on failures. Use it \
  to verify changes or diagnose failing commands.
- edit_files: implements changes by reading, writing, and editing files.

Delegate a task to a worker when it is long, multi-step, or self-contained — the worker runs its \
own agent loop and returns a summary. Keep doing your own work for quick, single tool calls. \
You remain responsible for the final answer: synthesize worker results and verify the overall \
outcome (for example, delegate to run_tests after edit_files).";

async fn load_most_recent_session(
    store: &mut Store,
    session: &mut Option<Arc<Mutex<Session>>>,
    event_tx: &Sender<Event>,
) {
    match store.most_recent_session().await {
        Ok(Some(stored)) => {
            let loaded = Session::from_stored(stored);
            *session = Some(Arc::new(Mutex::new(loaded.clone())));
            let _ = event_tx
                .send(Event::SessionLoaded {
                    id: loaded.id.unwrap_or_default(),
                    title: loaded.title.clone().unwrap_or_default(),
                    session: loaded,
                })
                .await;
        }
        Ok(None) => {}
        Err(e) => {
            let _ = event_tx
                .send(Event::SessionError {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

async fn stream_busy(active_stream: &Option<AbortHandle>, event_tx: &Sender<Event>) -> bool {
    if active_stream.as_ref().is_some_and(|h| !h.is_finished()) {
        let _ = event_tx
            .send(Event::StreamError {
                error: "a reply is already streaming".into(),
            })
            .await;
        return true;
    }
    false
}

fn client_for<'a>(
    clients: &'a mut HashMap<String, ProviderClient>,
    connections: &'a mut Connections,
    name: &str,
) -> Result<&'a ProviderClient, String> {
    if !clients.contains_key(name) {
        let pc = connections
            .providers
            .get(name)
            .ok_or_else(|| format!("provider '{name}' not found"))?;
        let client = build_client(pc)?;
        clients.insert(name.to_string(), client);
    }
    Ok(clients.get(name).unwrap())
}

#[allow(clippy::too_many_arguments)]
async fn stream_stream_to_events(
    mut stream: shuvarie_llm::StreamStream,
    session: Arc<Mutex<Session>>,
    client: ProviderClient,
    mut store: Store,
    model: String,
    worker_usage: Arc<std::sync::Mutex<shuvarie_catalog::TokenUsage>>,
    embedding_setup: Option<EmbeddingSetup>,
    event_tx: Sender<Event>,
) {
    use futures_util::StreamExt;

    while let Some(item) = stream.next().await {
        match item {
            shuvarie_llm::StreamItem::Delta { text } if !text.is_empty() => {
                let _ = event_tx.send(Event::TokenReceived { content: text }).await;
            }
            shuvarie_llm::StreamItem::Delta { .. } => {}
            shuvarie_llm::StreamItem::ToolStart { name, args, worker } => {
                let _ = event_tx
                    .send(Event::ToolStarted { name, args, worker })
                    .await;
            }
            shuvarie_llm::StreamItem::ToolResult {
                name,
                output,
                ok,
                worker,
            } => {
                let _ = event_tx
                    .send(Event::ToolFinished {
                        name,
                        ok,
                        output,
                        worker,
                    })
                    .await;
            }
            shuvarie_llm::StreamItem::WorkerStart { name, args } => {
                let _ = event_tx.send(Event::WorkerStarted { name, args }).await;
            }
            shuvarie_llm::StreamItem::WorkerResult { name, output, ok } => {
                let _ = event_tx
                    .send(Event::WorkerFinished { name, ok, output })
                    .await;
            }
            shuvarie_llm::StreamItem::Done { text, usage } => {
                let mut guard = session.lock().await;
                let combined = {
                    let worker_usage = worker_usage.lock().unwrap();
                    shuvarie_catalog::TokenUsage {
                        input_tokens: usage.input_tokens + worker_usage.input_tokens,
                        output_tokens: usage.output_tokens + worker_usage.output_tokens,
                        total_tokens: usage.total_tokens + worker_usage.total_tokens,
                        cached_input_tokens: usage.cached_input_tokens
                            + worker_usage.cached_input_tokens,
                        reasoning_tokens: usage.reasoning_tokens + worker_usage.reasoning_tokens,
                    }
                };
                guard.push_assistant(text.clone());
                let cost = shuvarie_catalog::estimate_cost(client.kind(), &model, &combined);
                guard.add_usage(combined, cost);
                let id = guard.id;
                let seq = guard.messages.len() - 1;
                drop(guard);
                if let Some(id) = id {
                    match store
                        .append_assistant_message(id, &text, combined, cost)
                        .await
                    {
                        Ok(msg) => {
                            if let Some(setup) = &embedding_setup {
                                let store_idx = store.clone();
                                let setup_idx = setup.clone();
                                let content_idx = msg.content.clone();
                                tokio::spawn(async move {
                                    let _ = embeddings::index_message(
                                        &mut store_idx.clone(),
                                        &setup_idx,
                                        msg.id,
                                        id,
                                        seq as u64,
                                        &content_idx,
                                    )
                                    .await;
                                });
                            }
                        }
                        Err(e) => {
                            let _ = event_tx
                                .send(Event::StreamError {
                                    error: format!("failed to persist message: {e}"),
                                })
                                .await;
                        }
                    }
                }
                let _ = event_tx.send(Event::StreamDone { text, usage }).await;
                let _ = event_tx.send(Event::UsageUpdate { usage, cost }).await;
                break;
            }
            shuvarie_llm::StreamItem::Error { message } => {
                let _ = event_tx.send(Event::StreamError { error: message }).await;
                break;
            }
        }
    }
}

fn build_client(pc: &ProviderConfig) -> Result<ProviderClient, String> {
    ProviderClient::build(pc.kind, pc.api_key.as_deref(), pc.base_url.as_deref())
        .map_err(|e| e.to_string())
}

async fn persist(
    config: &Config,
    connections: &Connections,
    config_path: Option<&Path>,
    connections_path: Option<&Path>,
    event_tx: &Sender<Event>,
) {
    let config_result = match config_path {
        Some(path) => config.save_to(path),
        None => config.save(),
    };
    let connections_result = match connections_path {
        Some(path) => connections.save_to(path),
        None => connections.save(),
    };
    match (config_result, connections_result) {
        (Ok(()), Ok(())) => {
            let _ = event_tx.send(Event::ConfigSaved).await;
        }
        (Err(e), _) | (_, Err(e)) => {
            let _ = event_tx
                .send(Event::ConfigError {
                    error: e.to_string(),
                })
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shuvarie_catalog::{Provider, TokenUsage};
    use shuvarie_llm::StreamItem;

    #[tokio::test]
    async fn stream_events_forward_and_accumulate_usage() {
        let client = ProviderClient::build(Provider::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::Delta {
                text: "hello ".into(),
            },
            StreamItem::Delta {
                text: "world".into(),
            },
            StreamItem::Done {
                text: "hello world".into(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                store,
                "ollama-model".into(),
                worker_usage,
                None,
                event_tx,
            )
            .await;
        });

        let mut deltas = String::new();
        let mut saw_done = false;
        let mut saw_usage = false;
        for _ in 0..6 {
            match event_rx.recv().await {
                Some(Event::TokenReceived { content }) => deltas.push_str(&content),
                Some(Event::StreamDone { .. }) => saw_done = true,
                Some(Event::UsageUpdate { .. }) => saw_usage = true,
                Some(_) => {}
                None => break,
            }
            if saw_done && saw_usage {
                break;
            }
        }
        assert_eq!(deltas, "hello world");
        assert!(saw_done && saw_usage);
        let guard = session.lock().await;
        assert_eq!(guard.messages.len(), 1);
        assert_eq!(guard.messages[0].content, "hello world");
        assert_eq!(guard.tokens, 30);
        assert_eq!(guard.input_tokens, 10);
        assert_eq!(guard.output_tokens, 20);
    }

    #[tokio::test]
    async fn stream_error_forwards_and_leaves_session_clean() {
        let client = ProviderClient::build(Provider::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::Delta {
                text: "partial".into(),
            },
            StreamItem::Error {
                message: "boom".into(),
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                store,
                "ollama-model".into(),
                worker_usage,
                None,
                event_tx,
            )
            .await;
        });

        let mut saw_error = false;
        for _ in 0..4 {
            match event_rx.recv().await {
                Some(Event::StreamError { error }) if error == "boom" => {
                    saw_error = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_error, "expected StreamError");
        let guard = session.lock().await;
        assert!(guard.messages.is_empty(), "no assistant message on error");
    }

    #[tokio::test]
    async fn worker_events_forward_and_usage_accumulates() {
        let client = ProviderClient::build(Provider::Ollama, None, None).unwrap();
        let session = Arc::new(Mutex::new(Session::new()));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

        let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
            StreamItem::WorkerStart {
                name: "explore_workspace".into(),
                args: serde_json::json!({ "task": "find the bug" }),
            },
            StreamItem::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({ "pattern": "bug" }),
                worker: Some("explore_workspace".into()),
            },
            StreamItem::ToolResult {
                name: "grep".into(),
                output: "found".into(),
                ok: true,
                worker: Some("explore_workspace".into()),
            },
            StreamItem::WorkerResult {
                name: "explore_workspace".into(),
                output: "summary".into(),
                ok: true,
            },
            StreamItem::Done {
                text: "done".into(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                    ..TokenUsage::default()
                },
            },
        ]));

        let session_shared = session.clone();
        let store = Store::open_in_memory().await.unwrap();
        let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            total_tokens: 12,
            ..TokenUsage::default()
        }));
        tokio::spawn(async move {
            stream_stream_to_events(
                stream,
                session_shared,
                client,
                store,
                "ollama-model".into(),
                worker_usage,
                None,
                event_tx,
            )
            .await;
        });

        let mut saw_worker_start = false;
        let mut saw_worker_tool = false;
        let mut saw_worker_finish = false;
        loop {
            match event_rx.recv().await {
                Some(Event::WorkerStarted { name, .. }) if name == "explore_workspace" => {
                    saw_worker_start = true;
                }
                Some(Event::ToolStarted { worker, .. })
                    if worker.as_deref() == Some("explore_workspace") =>
                {
                    saw_worker_tool = true;
                }
                Some(Event::WorkerFinished { name, ok, .. }) if name == "explore_workspace" => {
                    saw_worker_finish = ok;
                }
                Some(Event::UsageUpdate { usage, .. }) => {
                    assert_eq!(usage.input_tokens, 15, "manager + worker input");
                    assert_eq!(usage.output_tokens, 27, "manager + worker output");
                    assert_eq!(usage.total_tokens, 42, "manager + worker total");
                    saw_worker_finish = true;
                }
                Some(Event::StreamDone { .. }) => break,
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_worker_start, "expected WorkerStarted");
        assert!(saw_worker_tool, "expected nested tool event");
        assert!(saw_worker_finish, "expected WorkerFinished or UsageUpdate");
        let guard = session.lock().await;
        assert_eq!(guard.tokens, 42, "session accumulates combined usage");
        assert_eq!(guard.input_tokens, 15);
        assert_eq!(guard.output_tokens, 27);
    }
}
