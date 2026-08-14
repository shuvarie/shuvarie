use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::task::AbortHandle;

use shuvarie_llm::ProviderClient;

use crate::command::Command;
use crate::config::{Config, ProviderConfig};
use crate::event::Event;
use crate::session::Session;

pub async fn run(mut config: Config, mut cmd_rx: Receiver<Command>, event_tx: Sender<Event>) {
    let mut clients: HashMap<String, ProviderClient> = HashMap::new();
    let mut session: Option<Arc<Mutex<Session>>> = None;
    let mut active_stream: Option<AbortHandle> = None;

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
                active_stream = Some(
                    tokio::spawn(async move {
                        stream_stream_to_events(stream, session_shared, client_shared, tx).await;
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
        }
    }
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
                drop(guard);
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
    use crate::config::UiPrefs;
    use shuvarie_llm::Provider;

    fn empty_config() -> Config {
        Config {
            providers: HashMap::new(),
            active_provider: None,
            active_model: None,
            ui: UiPrefs::default(),
        }
    }

    #[tokio::test]
    async fn ping_pong() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let handle = tokio::spawn(run(empty_config(), cmd_rx, event_tx));
        cmd_tx.send(Command::Ping).await.unwrap();
        let ev = event_rx.recv().await.expect("event");
        assert!(matches!(ev, Event::Pong));
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn add_provider_emits_saved() {
        // The core task persists to Config::config_path() (user config dir). We only
        // assert the event is emitted here; disk round-trip is covered by config tests.
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let config = empty_config();
        let handle = tokio::spawn(run(config, cmd_rx, event_tx));
        cmd_tx
            .send(Command::AddProvider {
                name: "shuvarie-test-add".into(),
                config: ProviderConfig::new(Provider::Ollama, None, None),
            })
            .await
            .unwrap();

        let mut saw_saved = false;
        for _ in 0..5 {
            match event_rx.recv().await {
                Some(Event::ConfigSaved) => {
                    saw_saved = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_saved, "expected ConfigSaved event");
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn remove_provider_clears_active() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let mut config = empty_config();
        config.providers.insert(
            "p1".into(),
            ProviderConfig::new(Provider::Ollama, None, None),
        );
        config.active_provider = Some("p1".into());

        let handle = tokio::spawn(run(config, cmd_rx, event_tx));
        cmd_tx
            .send(Command::RemoveProvider { name: "p1".into() })
            .await
            .unwrap();

        let mut saved = false;
        for _ in 0..5 {
            if matches!(event_rx.recv().await, Some(Event::ConfigSaved)) {
                saved = true;
                break;
            }
        }
        assert!(saved);
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn send_message_without_active_provider_emits_error() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let handle = tokio::spawn(run(empty_config(), cmd_rx, event_tx));
        cmd_tx
            .send(Command::SendMessage {
                content: "hello".into(),
            })
            .await
            .unwrap();

        let mut saw_error = false;
        for _ in 0..5 {
            match event_rx.recv().await {
                Some(Event::StreamError { .. }) => {
                    saw_error = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(
            saw_error,
            "expected StreamError for missing active provider"
        );
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn cancel_with_no_active_stream_keeps_task_alive() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let handle = tokio::spawn(run(empty_config(), cmd_rx, event_tx));
        cmd_tx.send(Command::CancelStream).await.unwrap();
        cmd_tx.send(Command::Ping).await.unwrap();

        let ev = event_rx.recv().await.expect("event");
        assert!(
            matches!(ev, Event::Pong),
            "task must survive a no-op cancel"
        );
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn double_send_while_streaming_is_rejected() {
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
        let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

        let mut config = empty_config();
        config.providers.insert(
            "ollama".into(),
            ProviderConfig::new(Provider::Ollama, None, None),
        );
        config.active_provider = Some("ollama".into());
        config.active_model = Some("test-model".into());

        let handle = tokio::spawn(run(config, cmd_rx, event_tx));
        cmd_tx
            .send(Command::SendMessage {
                content: "first".into(),
            })
            .await
            .unwrap();
        cmd_tx
            .send(Command::SendMessage {
                content: "second".into(),
            })
            .await
            .unwrap();

        let mut saw_rejection = false;
        for _ in 0..6 {
            match event_rx.recv().await {
                Some(Event::StreamError { error }) if error.contains("already streaming") => {
                    saw_rejection = true;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_rejection, "expected second send to be rejected");
        cmd_tx.send(Command::CancelStream).await.unwrap();
        drop(cmd_tx);
        let _ = handle.await;
    }

    #[tokio::test]
    async fn stream_events_forward_and_accumulate_usage() {
        use shuvarie_llm::{StreamItem, TokenUsage};

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
        tokio::spawn(async move {
            stream_stream_to_events(stream, session_shared, client, event_tx).await;
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
        use shuvarie_llm::StreamItem;

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
        tokio::spawn(async move {
            stream_stream_to_events(stream, session_shared, client, event_tx).await;
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
