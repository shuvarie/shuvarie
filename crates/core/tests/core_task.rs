use std::path::PathBuf;

use shuvarie_core::{
    Active, Command, Config, Connections, Event, ProviderConfig, Session, StartupSession,
    TrustGrants, run,
};
use shuvarie_db::Store;

fn empty_config() -> Config {
    Config::default()
}

fn empty_connections() -> Connections {
    Connections::default()
}

/// A connection whose endpoint refuses connections: the stream fails with a
/// retryable `Connection failed`, so the busy state (and the retry wait) is
/// deterministic on any machine.
fn dead_endpoint_connections() -> Connections {
    let mut connections = empty_connections();
    connections.providers.insert(
        "dead".into(),
        ProviderConfig::new(
            "dead",
            "ollama",
            None,
            Some("http://127.0.0.1:9".to_string()),
        ),
    );
    connections.active = Some(Active {
        provider: "dead".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    connections
}

fn temp_connections_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shuvarie-test-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir.join("shuvarie").join("connections.kdl")
}

async fn recv_skills_loaded(event_rx: &mut tokio::sync::mpsc::Receiver<Event>) {
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::SkillsLoaded { .. }),
        "expected SkillsLoaded as the first startup event, got {ev:?}"
    );
}

fn permissions_for_tests() -> std::sync::Arc<shuvarie_core::permissions::Permissions> {
    std::sync::Arc::new(
        shuvarie_core::permissions::Permissions::build(
            &empty_config().permissions,
            &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        )
        .unwrap(),
    )
}

#[tokio::test]
async fn ping_pong() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::Ping).await.unwrap();
    recv_skills_loaded(&mut event_rx).await;
    let ev = event_rx.recv().await.expect("event");
    assert!(matches!(ev, Event::Pong));
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn add_provider_emits_saved() {
    // The core task persists to the given connections path; we assert the event
    // is emitted and the provider lands in the temp file, not the user config.
    let connections_path = temp_connections_path("add-provider");
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        Some(connections_path.clone()),
        permissions_for_tests(),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::AddProvider {
            id: "shuvarie-test-add".into(),
            config: ProviderConfig::new("shuvarie-test-add", "ollama", None, None),
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

    let loaded = Connections::load_from(&connections_path).expect("load persisted connections");
    assert!(
        loaded.providers.contains_key("shuvarie-test-add"),
        "provider persisted to the temp connections path"
    );
    drop(cmd_tx);
    let _ = handle.await;
    let _ = std::fs::remove_file(connections_path);
}

#[tokio::test]
async fn remove_provider_clears_active() {
    let connections_path = temp_connections_path("remove-provider");
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let mut connections = empty_connections();
    connections
        .providers
        .insert("p1".into(), ProviderConfig::new("p1", "ollama", None, None));
    connections.active = Some(Active {
        provider: "p1".into(),
        model: None,
        variant: None,
    });

    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        Some(connections_path.clone()),
        permissions_for_tests(),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    let loaded = Connections::load_from(&connections_path).expect("load persisted connections");
    assert!(
        !loaded.providers.contains_key("p1") && loaded.active.is_none(),
        "removal persisted to the temp connections path"
    );
    drop(cmd_tx);
    let _ = handle.await;
    let _ = std::fs::remove_file(connections_path);
}

#[tokio::test]
async fn send_message_without_active_provider_emits_error() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::CancelStream).await.unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();

    recv_skills_loaded(&mut event_rx).await;
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::Pong),
        "task must survive a no-op cancel"
    );
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn send_while_streaming_is_steered() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        dead_endpoint_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    let mut saw_steered = false;
    for _ in 0..8 {
        match event_rx.recv().await {
            Some(Event::PromptSteered { content }) => {
                assert_eq!(content, "second");
                saw_steered = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_steered, "expected the second send to be steered");
    cmd_tx.send(Command::CancelStream).await.unwrap();
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn steered_recall_round_trip() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);

    let handle = tokio::spawn(run(
        empty_config(),
        dead_endpoint_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    let mut steered = 0;
    for _ in 0..10 {
        match event_rx.recv().await {
            Some(Event::PromptSteered { content }) => {
                assert_eq!(content, "second");
                steered += 1;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert_eq!(steered, 1, "expected the second send to be steered");

    cmd_tx
        .send(Command::RecallSteered { stacked: true })
        .await
        .unwrap();
    let mut recalled = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SteeredRecalled { stacked, content }) => {
                assert!(stacked);
                assert_eq!(content.as_deref(), Some("second"));
                recalled = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(recalled, "expected the steered prompt to be recalled");

    cmd_tx
        .send(Command::RecallSteered { stacked: false })
        .await
        .unwrap();
    let mut empty_recall = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SteeredRecalled { content, .. }) => {
                assert!(content.is_none(), "queue was drained by the first recall");
                empty_recall = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(empty_recall, "expected an empty-queue recall reply");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn cancel_dispatches_first_steered_prompt() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);

    let handle = tokio::spawn(run(
        empty_config(),
        dead_endpoint_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    let mut steered = false;
    for _ in 0..10 {
        match event_rx.recv().await {
            Some(Event::PromptSteered { content }) => {
                assert_eq!(content, "second");
                steered = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(steered, "expected the second send to be steered");

    // Wait for the retry wait to be scheduled: the stream task is gone by
    // then, so the cancel lands on a stable busy state and reliably takes the
    // retry-cancel path (cancelling between the task ending and its outcome
    // being processed would leave the queue to the next window instead).
    let mut retrying = false;
    for _ in 0..10 {
        match event_rx.recv().await {
            Some(Event::RetryScheduled { .. }) => {
                retrying = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(retrying, "expected a scheduled retry for the dead endpoint");

    cmd_tx.send(Command::CancelStream).await.unwrap();
    let mut dispatched = false;
    for _ in 0..10 {
        match event_rx.recv().await {
            Some(Event::TurnStarted { content, steered }) => {
                assert!(steered, "dispatch must be flagged as steered");
                assert_eq!(content, "second");
                dispatched = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(dispatched, "cancel must dispatch the first steered prompt");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn new_session_clears_steered_queue() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);

    let handle = tokio::spawn(run(
        empty_config(),
        dead_endpoint_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));
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

    // Wait until the failed stream scheduled its retry: the stream task is
    // gone by then, so the busy gate no longer rejects `NewSession`.
    let mut retrying = false;
    for _ in 0..10 {
        match event_rx.recv().await {
            Some(Event::RetryScheduled { .. }) => {
                retrying = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(retrying, "expected a scheduled retry for the dead ollama");

    cmd_tx.send(Command::NewSession).await.unwrap();
    let mut cleared = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SteeredCleared) => {
                cleared = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(cleared, "a session-level transition must wipe the queue");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn send_message_persists_session_and_messages() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let mut connections = empty_connections();
    connections.providers.insert(
        "ollama".into(),
        ProviderConfig::new("ollama", "ollama", None, None),
    );
    connections.active = Some(Active {
        provider: "ollama".into(),
        model: Some("test-model".into()),
        variant: None,
    });

    let store = Store::open_in_memory().await.unwrap();
    let store_clone = store.clone();
    // Disable connection retries: the missing ollama must surface as an
    // immediate `StreamError` instead of a `RetryScheduled` wait.
    let mut config = empty_config();
    config.retry.max_retries = 0;
    let handle = tokio::spawn(run(
        config,
        connections,
        store_clone,
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
        })
        .await
        .unwrap();
    // Wait for the stream error (no real ollama running) after the session row is created.
    let mut saw_error = false;
    for _ in 0..8 {
        match event_rx.recv().await {
            Some(Event::StreamError { error }) if error.contains("failed to") => {
                saw_error = true;
                break;
            }
            Some(Event::SessionStarted) => {}
            Some(Event::StreamError { .. }) => {
                saw_error = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_error, "expected a stream error for missing ollama");

    let mut store = store;
    let sessions = store.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1, "session row created on first message");
    assert_eq!(sessions[0].title, "hello world");
    assert_eq!(sessions[0].message_count, 1, "user message persisted");
    let loaded = store.load_session(sessions[0].id).await.unwrap();
    assert_eq!(loaded.messages.len(), 1);
    assert_eq!(loaded.messages[0].content, "hello world");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn load_current_emits_session_loaded_on_startup() {
    let mut store = Store::open_in_memory().await.unwrap();
    store
        .create_session("existing", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        store.clone(),
        StartupSession::MostRecent,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    let mut saw_loaded = false;
    for _ in 0..3 {
        match event_rx.recv().await {
            Some(Event::SessionLoaded { title, .. }) => {
                assert_eq!(title, "existing");
                saw_loaded = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_loaded, "expected SessionLoaded for --current startup");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn load_session_by_uuid_on_startup() {
    let mut store = Store::open_in_memory().await.unwrap();
    store
        .create_session("other", Some("ollama"), Some("model"))
        .await
        .unwrap();
    let target = store
        .create_session("target", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        store.clone(),
        StartupSession::Session(target),
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    let mut saw_loaded = false;
    for _ in 0..3 {
        match event_rx.recv().await {
            Some(Event::SessionLoaded { id, title, .. }) => {
                assert_eq!(id, target);
                assert_eq!(title, "target");
                saw_loaded = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_loaded, "expected SessionLoaded for --session startup");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn load_missing_session_uuid_on_startup_errors() {
    let mut store = Store::open_in_memory().await.unwrap();
    store
        .create_session("existing", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        store.clone(),
        StartupSession::Session(uuid::Uuid::nil()),
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    let mut saw_error = false;
    for _ in 0..3 {
        match event_rx.recv().await {
            Some(Event::SessionError { error }) => {
                assert!(error.contains("not found"), "unexpected error: {error}");
                saw_error = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_error, "expected SessionError for unknown -s UUID");

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn no_load_current_skips_session_loaded_on_startup() {
    let mut store = Store::open_in_memory().await.unwrap();
    store
        .create_session("existing", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        store.clone(),
        StartupSession::None,
        None,
        None,
        std::sync::Arc::new(
            shuvarie_core::permissions::Permissions::build(
                &empty_config().permissions,
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .unwrap(),
        ),
        TrustGrants::all(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    recv_skills_loaded(&mut event_rx).await;
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::Pong),
        "expected Pong, got {ev:?}; no SessionLoaded without --current"
    );

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn reload_reconstructs_tool_records_at_dense_message_indices() {
    let mut store = Store::open_in_memory().await.unwrap();
    let sid = store
        .create_session("t", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let _user = store
        .append_message(sid, None, shuvarie_llm::Role::User, "hello")
        .await
        .unwrap();
    let user_id = store.load_session(sid).await.unwrap().messages[0].id;
    let assistant = store
        .append_message(sid, Some(user_id), shuvarie_llm::Role::Assistant, "text")
        .await
        .unwrap();
    // `ToolCall.seq` is a per-message tool ordinal (0), not the message seq (1).
    store
        .append_tool_call(
            sid,
            assistant.id,
            0,
            "read_file",
            "{}",
            "out",
            "",
            true,
            false,
            None,
            "",
            None,
            None,
            0,
        )
        .await
        .unwrap();

    let stored = store.load_session(sid).await.unwrap();
    let session = Session::from_stored(stored);

    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.tool_records.len(), 1);
    // The tool must be attributed to the assistant message's dense index, not
    // the raw tool ordinal.
    assert_eq!(session.tool_records[0].message_id, assistant.id);
    assert_eq!(session.tool_records[0].message_seq, 1);
}

#[tokio::test]
async fn reload_after_resume_maps_tools_to_new_dense_indices() {
    let mut store = Store::open_in_memory().await.unwrap();
    let sid = store
        .create_session("t", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let _user = store
        .append_message(sid, None, shuvarie_llm::Role::User, "hello")
        .await
        .unwrap();
    let user_id = store.load_session(sid).await.unwrap().messages[0].id;
    let first = store
        .append_message(sid, Some(user_id), shuvarie_llm::Role::Assistant, "old")
        .await
        .unwrap();
    store
        .append_tool_call(
            sid, first.id, 0, "grep", "{}", "out", "", true, false, None, "", None, None, 0,
        )
        .await
        .unwrap();

    // Simulate an interrupted resume: delete the assistant turn (tool calls
    // + message), walk the leaf back to the user prompt, then re-append a
    // fresh assistant row with a higher seq and its own tool call.
    store.delete_tool_calls_for_message(first.id).await.unwrap();
    store.delete_message(first.id).await.unwrap();
    store.set_active_leaf(sid, Some(user_id)).await.unwrap();
    let second = store
        .append_message(sid, Some(user_id), shuvarie_llm::Role::Assistant, "new")
        .await
        .unwrap();
    store.set_active_leaf(sid, Some(second.id)).await.unwrap();
    store
        .append_tool_call(
            sid,
            second.id,
            0,
            "read_file",
            "{}",
            "out",
            "",
            true,
            false,
            None,
            "",
            None,
            None,
            0,
        )
        .await
        .unwrap();

    let stored = store.load_session(sid).await.unwrap();
    let session = Session::from_stored(stored);

    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.tool_records.len(), 1);
    assert_eq!(session.tool_records[0].message_id, second.id);
    // The tool must be mapped to the assistant's dense index (1), regardless of
    // the re-append bumping the DB seq above the row count.
    assert_eq!(session.tool_records[0].message_seq, 1);
}
