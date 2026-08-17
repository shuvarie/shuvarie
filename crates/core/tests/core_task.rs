use std::collections::HashMap;

use shuvarie_core::{Command, Config, Event, ProviderConfig, run};
use shuvarie_llm::Provider;

fn empty_config() -> Config {
    Config {
        providers: HashMap::new(),
        active_provider: None,
        active_model: None,
        ui: Default::default(),
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
