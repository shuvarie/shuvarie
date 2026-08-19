use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::AbortHandle;

use shuvarie_db::Store;
use shuvarie_llm::ProviderClient;

use crate::command::Command;
use crate::config::{Config, ProviderConfig};
use crate::event::Event;
use crate::session::Session;

pub async fn run(
    mut config: Config,
    mut store: Store,
    mut cmd_rx: Receiver<Command>,
    event_tx: Sender<Event>,
) {
    let mut clients: HashMap<String, ProviderClient> = HashMap::new();
    let mut session: Option<Arc<Mutex<Session>>> = None;
    let mut active_stream: Option<AbortHandle> = None;

    load_most_recent_session(&mut store, &mut session, &event_tx).await;

    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            Command::Ping => {
                let _ = event_tx.send(Event::Pong).await;
            }
            Command::ListModels { provider_name } => {
                let client = match client_for(&mut clients, &mut config, &provider_name) {
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
                    Ok(models) => {
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
                config.providers.insert(name.clone(), pc);
                clients.remove(&name);
                persist(&config, &event_tx).await;
            }
            Command::RemoveProvider { name } => {
                config.providers.remove(&name);
                clients.remove(&name);
                if config.active_provider.as_deref() == Some(name.as_str()) {
                    config.active_provider = None;
                    config.active_model = None;
                }
                persist(&config, &event_tx).await;
            }
            Command::SetActiveProvider { name } => {
                if config.providers.contains_key(&name) {
                    config.active_provider = Some(name.clone());
                    if !clients.contains_key(&name)
                        && let Some(pc) = config.providers.get(&name)
                        && let Ok(client) = build_client(pc)
                    {
                        clients.insert(name.clone(), client);
                    }
                    persist(&config, &event_tx).await;
                }
            }
            Command::SetActiveModel { model } => {
                config.active_model = Some(model);
                persist(&config, &event_tx).await;
            }
            Command::SaveConfig => {
                persist(&config, &event_tx).await;
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
                                config.active_provider.as_deref(),
                                config.active_model.as_deref(),
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
                    if let Err(e) = store
                        .append_message(id, guard.messages.last().unwrap().role, &content)
                        .await
                    {
                        let _ = event_tx
                            .send(Event::StreamError {
                                error: format!("failed to persist message: {e}"),
                            })
                            .await;
                        continue;
                    }
                }

                let Some(provider_name) = config.active_provider.clone() else {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: "no active provider".into(),
                        })
                        .await;
                    continue;
                };
                let Some(model) = config.active_model.clone() else {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: "no active model".into(),
                        })
                        .await;
                    continue;
                };

                let client = match client_for(&mut clients, &mut config, &provider_name) {
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
                let stream = client.stream(&model, &content, &prior).await;
                let tx = event_tx.clone();
                let session_shared = s.clone();
                let client_shared = client.clone();
                let store_shared = store.clone();
                active_stream = Some(
                    tokio::spawn(async move {
                        stream_stream_to_events(
                            stream,
                            session_shared,
                            client_shared,
                            store_shared,
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
    config: &'a mut Config,
    name: &str,
) -> Result<&'a ProviderClient, String> {
    if !clients.contains_key(name) {
        let pc = config
            .providers
            .get(name)
            .ok_or_else(|| format!("provider '{name}' not found"))?;
        let client = build_client(pc)?;
        clients.insert(name.to_string(), client);
    }
    Ok(clients.get(name).unwrap())
}

async fn stream_stream_to_events(
    mut stream: shuvarie_llm::StreamStream,
    session: Arc<Mutex<Session>>,
    client: ProviderClient,
    mut store: Store,
    event_tx: Sender<Event>,
) {
    use futures_util::StreamExt;

    while let Some(item) = stream.next().await {
        match item {
            shuvarie_llm::StreamItem::Delta { text } if !text.is_empty() => {
                let _ = event_tx.send(Event::TokenReceived { content: text }).await;
            }
            shuvarie_llm::StreamItem::Delta { .. } => {}
            shuvarie_llm::StreamItem::Done { text, usage } => {
                let mut guard = session.lock().await;
                guard.push_assistant(text.clone());
                let cost = client.estimate_cost(&usage);
                guard.add_usage(usage, cost);
                let id = guard.id;
                drop(guard);
                if let Some(id) = id
                    && let Err(e) = store.append_assistant_message(id, &text, usage, cost).await
                {
                    let _ = event_tx
                        .send(Event::StreamError {
                            error: format!("failed to persist message: {e}"),
                        })
                        .await;
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

async fn persist(config: &Config, event_tx: &Sender<Event>) {
    match config.save() {
        Ok(()) => {
            let _ = event_tx.send(Event::ConfigSaved).await;
        }
        Err(e) => {
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
    use shuvarie_llm::{Provider, StreamItem, TokenUsage};

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
        tokio::spawn(async move {
            stream_stream_to_events(stream, session_shared, client, store, event_tx).await;
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
        tokio::spawn(async move {
            stream_stream_to_events(stream, session_shared, client, store, event_tx).await;
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
}
