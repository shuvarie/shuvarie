use std::path::PathBuf;

use shuvarie_core::{
    Active, Command, Config, Connections, Event, ProviderConfig, Session, StartupSession, run,
};
use shuvarie_db::Store;

fn empty_config() -> Config {
    Config::default()
}

fn empty_connections() -> Connections {
    Connections::default()
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
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::Ping).await.unwrap();
    let ev = event_rx.recv().await.expect("event");
    assert!(matches!(ev, Event::Pong));
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn start_session_loads_skills_before_prompt() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::StartSession).await.unwrap();

    let mut saw_skills = false;
    let mut saw_started = false;
    for _ in 0..4 {
        match event_rx.recv().await {
            Some(Event::SkillsLoaded { .. }) if !saw_started => saw_skills = true,
            Some(Event::SessionStarted) => {
                saw_started = true;
                break;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(
        saw_skills && saw_started,
        "SkillsLoaded must be emitted when the session starts, before the first prompt"
    );

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
        cmd_rx,
        event_tx,
    ));
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

    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
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
    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        store_clone,
        StartupSession::None,
        None,
        None,
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
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

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
        .append_message(sid, shuvarie_llm::Role::User, "hello")
        .await
        .unwrap();
    let assistant = store
        .append_message(sid, shuvarie_llm::Role::Assistant, "text")
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
            true,
            None,
            "",
            None,
            None,
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
async fn reload_after_redo_maps_tools_to_new_dense_indices() {
    let mut store = Store::open_in_memory().await.unwrap();
    let sid = store
        .create_session("t", Some("ollama"), Some("model"))
        .await
        .unwrap();

    let _user = store
        .append_message(sid, shuvarie_llm::Role::User, "hello")
        .await
        .unwrap();
    let first = store
        .append_message(sid, shuvarie_llm::Role::Assistant, "old")
        .await
        .unwrap();
    store
        .append_tool_call(
            sid, first.id, 0, "grep", "{}", "out", true, None, "", None, None,
        )
        .await
        .unwrap();

    // Simulate redo: delete the assistant turn (tool calls + message), then
    // re-append a fresh assistant row with a higher seq and its own tool call.
    store.delete_tool_calls_for_message(first.id).await.unwrap();
    store.delete_message(first.id).await.unwrap();
    let second = store
        .append_message(sid, shuvarie_llm::Role::Assistant, "new")
        .await
        .unwrap();
    store
        .append_tool_call(
            sid,
            second.id,
            0,
            "read_file",
            "{}",
            "out",
            true,
            None,
            "",
            None,
            None,
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
