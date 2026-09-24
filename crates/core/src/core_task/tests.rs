use crate::core_task::steer::SteerSignal;

use super::*;
use futures_util::StreamExt as _;
use selune::ProviderType;
use shuvarie_llm::StreamItem;
use shuvarie_llm::TokenUsage;

fn provider(id: &str, kind: &str) -> ProviderConfig {
    ProviderConfig::new(id, kind, Some("tok".into()), None)
}

fn connections_with(providers: &[(&str, &str)]) -> Connections {
    let mut connections = Connections::default();
    for (id, kind) in providers {
        connections
            .providers
            .insert(id.to_string(), provider(id, kind));
    }
    connections
}

#[test]
fn model_override_resolves_the_matching_default_provider() {
    let connections = connections_with(&[
        ("openai-a", "openai"),
        ("openai-b", "openai"),
        ("anthropic-c", "anthropic"),
    ]);
    let defaults = shuvarie_config::DefaultProvidersConfig {
        use_ids: vec!["openai-b".to_string()],
    };
    assert_eq!(
        resolve_model_override(&connections, &defaults, "openai/gpt-6").unwrap(),
        ("openai-b".to_string(), "gpt-6".to_string())
    );
    // No default entry for anthropic: the first configured provider of
    // the type (lowest id) is used.
    assert_eq!(
        resolve_model_override(&connections, &defaults, "anthropic/claude-x").unwrap(),
        ("anthropic-c".to_string(), "claude-x".to_string())
    );
}

#[test]
fn model_override_last_matching_use_entry_wins() {
    let connections = connections_with(&[("a", "openai"), ("b", "openai"), ("c", "openai")]);
    let defaults = shuvarie_config::DefaultProvidersConfig {
        use_ids: vec!["a".to_string(), "b".to_string()],
    };
    assert_eq!(
        resolve_model_override(&connections, &defaults, "openai/m").unwrap(),
        ("b".to_string(), "m".to_string())
    );
}

#[test]
fn model_override_skips_entries_whose_type_no_longer_matches() {
    // `c` points at an anthropic connection; it must not serve an openai
    // model — the first openai connection wins instead.
    let connections = connections_with(&[("a", "openai"), ("c", "anthropic")]);
    let defaults = shuvarie_config::DefaultProvidersConfig {
        use_ids: vec!["c".to_string()],
    };
    assert_eq!(
        resolve_model_override(&connections, &defaults, "openai/m").unwrap(),
        ("a".to_string(), "m".to_string())
    );
}

#[test]
fn model_override_skips_entries_whose_connection_vanished() {
    let connections = connections_with(&[("a", "openai")]);
    let defaults = shuvarie_config::DefaultProvidersConfig {
        use_ids: vec!["ghost".to_string()],
    };
    assert_eq!(
        resolve_model_override(&connections, &defaults, "openai/m").unwrap(),
        ("a".to_string(), "m".to_string())
    );
}

#[test]
fn model_override_errors() {
    let connections = connections_with(&[("a", "openai")]);
    let defaults = shuvarie_config::DefaultProvidersConfig::default();
    let error = resolve_model_override(&connections, &defaults, "anthropic/m").unwrap_err();
    assert!(
        error.contains("no provider connection of type `anthropic`"),
        "{error}"
    );
    let error = resolve_model_override(&connections, &defaults, "nosuchtype/m").unwrap_err();
    assert!(error.contains("unknown provider type"), "{error}");
    let error = resolve_model_override(&connections, &defaults, "nomodel").unwrap_err();
    assert!(error.contains("expected `<provider>/<model>`"), "{error}");
}

#[test]
fn device_code_handler_forwards_prompts_for_oauth_backed_kinds_only() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
    let handler = device_code_handler(ProviderType::Chatgpt, "ChatGPT".into(), &tx)
        .expect("chatgpt gets a handler");
    handler(shuvarie_llm::DeviceCodePrompt {
        verification_uri: "https://auth.openai.com/codex/device".into(),
        user_code: "ABCD-1234".into(),
    });
    match rx.try_recv().expect("prompt forwarded") {
        Event::AuthPrompt {
            provider,
            verification_uri,
            user_code,
        } => {
            assert_eq!(provider, "ChatGPT");
            assert_eq!(verification_uri, "https://auth.openai.com/codex/device");
            assert_eq!(user_code, "ABCD-1234");
        }
        other => panic!("unexpected event: {other:?}"),
    }
    assert!(
        device_code_handler(ProviderType::Copilot, "copilot".into(), &tx).is_some(),
        "copilot gets a handler"
    );
    assert!(
        device_code_handler(ProviderType::Openai, "openai".into(), &tx).is_none(),
        "api-key transports need no device-code handler"
    );
}

#[tokio::test]
async fn auth_provider_login_reports_unknown_provider() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
    handle_auth_provider_login("ghost".into(), &BTreeMap::new(), &HashMap::new(), &tx).await;
    match rx.try_recv().expect("failure reported") {
        Event::AuthFailed { provider, error } => {
            assert_eq!(provider, "ghost");
            assert!(error.contains("unknown provider"), "{error}");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn auth_provider_login_with_a_static_key_succeeds_off_loop() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
    let mut providers = BTreeMap::new();
    providers.insert(
        "ChatGPT".to_string(),
        ProviderConfig::new("ChatGPT", "chatgpt", Some("tok".into()), None),
    );
    handle_auth_provider_login("ChatGPT".into(), &providers, &HashMap::new(), &tx).await;
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("event within timeout")
        .expect("channel open");
    match event {
        Event::AuthSuccess { provider } => assert_eq!(provider, "ChatGPT"),
        other => panic!("unexpected event: {other:?}"),
    }
}

#[tokio::test]
async fn auth_provider_login_on_api_key_transport_reports_failure() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Event>(4);
    let mut providers = BTreeMap::new();
    providers.insert(
        "OpenAI".to_string(),
        ProviderConfig::new("OpenAI", "openai", Some("sk-x".into()), None),
    );
    handle_auth_provider_login("OpenAI".into(), &providers, &HashMap::new(), &tx).await;
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("event within timeout")
        .expect("channel open");
    match event {
        Event::AuthFailed { provider, error } => {
            assert_eq!(provider, "OpenAI");
            assert!(error.contains("OAuth sign-in"), "{error}");
        }
        other => panic!("unexpected event: {other:?}"),
    }
}

#[test]
fn retry_schedule_escalates_and_caps() {
    assert_eq!(RetrySchedule::delay_secs(1), 3);
    assert_eq!(RetrySchedule::delay_secs(2), 5);
    assert_eq!(RetrySchedule::delay_secs(3), 10);
    assert_eq!(RetrySchedule::delay_secs(4), 20);
    assert_eq!(RetrySchedule::delay_secs(5), 30);
    assert_eq!(RetrySchedule::delay_secs(6), 60);
    assert_eq!(RetrySchedule::delay_secs(10), 60);
}

#[test]
fn next_connection_retry_caps_at_max() {
    assert_eq!(next_turn_retry(0, 10), Some((1, 3)));
    assert_eq!(next_turn_retry(1, 10), Some((2, 5)));
    assert_eq!(next_turn_retry(4, 10), Some((5, 30)));
    assert_eq!(next_turn_retry(5, 10), Some((6, 60)));
    assert_eq!(next_turn_retry(9, 10), Some((10, 60)));
    assert_eq!(next_turn_retry(10, 10), None);
    assert_eq!(next_turn_retry(11, 10), None);
    assert_eq!(next_turn_retry(0, 0), None, "0 = no retry");
    assert_eq!(next_turn_retry(0, 1), Some((1, 3)));
    assert_eq!(next_turn_retry(1, 1), None);
}

#[tokio::test]
async fn stream_events_forward_and_accumulate_usage() {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
        StreamItem::Delta {
            text: "hello ".into(),
        },
        StreamItem::Usage {
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                ..TokenUsage::default()
            },
            worker: None,
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
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });

    let mut deltas = String::new();
    let mut saw_done = false;
    let mut saw_live_usage = false;
    let mut snapshot = None;
    for _ in 0..8 {
        match event_rx.recv().await {
            Some(Event::TokenReceived { content }) => deltas.push_str(&content),
            Some(Event::StreamDone { .. }) => saw_done = true,
            Some(Event::UsageUpdate { usage, .. }) => {
                assert_eq!(usage.total_tokens, 30, "per-request usage passes through");
                saw_live_usage = true;
            }
            Some(Event::UsageSnapshot { usage, cost }) => {
                snapshot = Some((usage, cost));
            }
            Some(_) => {}
            None => break,
        }
        if saw_done && snapshot.is_some() {
            break;
        }
    }
    assert_eq!(deltas, "hello world");
    assert!(
        saw_done && saw_live_usage,
        "live usage and snapshot both emitted"
    );
    let (snapshot_usage, snapshot_cost) = snapshot.expect("UsageSnapshot after the turn");
    assert_eq!(snapshot_usage.total_tokens, 30, "snapshot is session-total");
    assert_eq!(snapshot_usage.input_tokens, 10);
    assert_eq!(snapshot_usage.output_tokens, 20);
    assert_eq!(snapshot_cost, 0.0, "no catalog provider, zero cost");
    let guard = session.lock().await;
    assert_eq!(guard.messages.len(), 1);
    assert_eq!(guard.messages[0].content, "hello world");
    assert_eq!(guard.tokens, 30);
    assert_eq!(guard.input_tokens, 10);
    assert_eq!(guard.output_tokens, 20);
}

#[tokio::test]
async fn stream_error_schedules_turn_retry_and_leaves_session_clean() {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
        StreamItem::Delta {
            text: "partial".into(),
        },
        StreamItem::Error {
            message: "boom".into(),
            reason: "Turn error".into(),
        },
    ]));

    let session_shared = session.clone();
    let store = Store::open_in_memory().await.unwrap();
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });

    // A turn error no longer surfaces as `StreamError`: it schedules the
    // timeout retry, so the run loop resumes the turn after a backoff.
    let outcome = stream_done_rx.recv().await.expect("outcome reported");
    assert_eq!(
        outcome,
        StreamOutcome::RetryableFailure {
            reason: "Turn error".into(),
            message: "boom".into(),
        },
    );
    // No `StreamError` either: only the run loop emits one, after
    // `[retry].max-retries` is exhausted.
    while let Some(event) = event_rx.recv().await {
        assert!(
            !matches!(event, Event::StreamError { .. }),
            "a turn error must not surface as StreamError: {event:?}"
        );
    }
    let guard = session.lock().await;
    assert!(guard.messages.is_empty(), "no assistant message on error");
}

#[tokio::test]
async fn stream_done_waits_for_queued_worker_items_to_drain() {
    // Mirrors the production merge shape (`select_all`, main stream
    // first): a worker's `ToolResult` queued behind an immediately-ready
    // main stream must still be delivered before `Event::StreamDone`, so
    // the TUI finishes every tool block before committing the turn.
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

    let done = StreamItem::Done {
        text: "final".into(),
        usage: TokenUsage {
            input_tokens: 10,
            output_tokens: 20,
            total_tokens: 30,
            ..TokenUsage::default()
        },
    };
    let worker_result = StreamItem::ToolResult {
        name: "grep".into(),
        output: "found".into(),
        ok: true,
        worker: Some("explore_workspace".into()),
        file_change: None,
        streams: None,
        call_id: "w1".into(),
    };
    let worker_request_usage = StreamItem::Usage {
        usage: TokenUsage {
            input_tokens: 5,
            output_tokens: 7,
            total_tokens: 12,
            ..TokenUsage::default()
        },
        worker: Some("explore_workspace".into()),
    };
    let (worker_tx, worker_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
    let _ = worker_tx.send(worker_request_usage).await;
    let _ = worker_tx.send(worker_result).await;
    drop(worker_tx);
    let streams: Vec<
        std::pin::Pin<Box<dyn futures_util::stream::Stream<Item = StreamItem> + Send>>,
    > = vec![
        Box::pin(futures_util::stream::iter(vec![done])),
        Box::pin(futures_util::stream::unfold(
            worker_rx,
            |mut rx| async move { rx.recv().await.map(|item| (item, rx)) },
        )),
    ];
    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::select_all(streams));

    let session_shared = session.clone();
    let store = Store::open_in_memory().await.unwrap();
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage {
        input_tokens: 5,
        output_tokens: 7,
        total_tokens: 12,
        ..TokenUsage::default()
    }));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });

    let mut done_seen = false;
    let mut saw_tool_result_before_done = false;
    let mut saw_live_usage = false;
    let mut saw_snapshot = false;
    loop {
        match event_rx.recv().await {
            Some(Event::ToolFinished { name, .. }) if name == "grep" => {
                assert!(!done_seen, "ToolFinished must precede StreamDone");
                saw_tool_result_before_done = true;
            }
            Some(Event::StreamDone { .. }) => done_seen = true,
            Some(Event::UsageUpdate { usage, .. }) => {
                assert_eq!(
                    usage.total_tokens, 12,
                    "worker request usage passes through"
                );
                saw_live_usage = true;
            }
            Some(Event::UsageSnapshot { usage, .. }) => {
                assert_eq!(usage.input_tokens, 15, "snapshot combines manager + worker");
                saw_snapshot = true;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_tool_result_before_done);
    assert!(done_seen, "StreamDone still emitted after the drain");
    assert!(saw_live_usage, "worker request emitted live usage");
    assert!(saw_snapshot, "turn end emitted UsageSnapshot");
    assert!(stream_done_rx.recv().await.is_some(), "outcome still sent");
    let guard = session.lock().await;
    assert_eq!(guard.tokens, 42, "session accumulates combined usage");
}

#[tokio::test]
async fn worker_events_forward_and_usage_accumulates() {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
        StreamItem::WorkerStart {
            name: "explore_workspace".into(),
            args: serde_json::json!({ "task": "find the bug" }),
            call_id: "w1".into(),
        },
        StreamItem::ToolStart {
            name: "grep".into(),
            args: serde_json::json!({ "pattern": "bug" }),
            worker: Some("explore_workspace".into()),
            call_id: "t1".into(),
        },
        StreamItem::ToolResult {
            name: "grep".into(),
            output: "found".into(),
            ok: true,
            worker: Some("explore_workspace".into()),
            file_change: None,
            streams: None,
            call_id: "t1".into(),
        },
        StreamItem::WorkerResult {
            name: "explore_workspace".into(),
            output: "summary".into(),
            ok: true,
            call_id: "w1".into(),
        },
        StreamItem::Usage {
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
                ..TokenUsage::default()
            },
            worker: None,
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
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });

    let mut saw_worker_start = false;
    let mut saw_worker_tool = false;
    let mut saw_worker_finish = false;
    let mut saw_live_usage = false;
    let mut saw_snapshot = false;
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
            Some(Event::UsageUpdate {
                usage,
                context_tokens,
                ..
            }) => {
                assert_eq!(
                    usage.total_tokens, 30,
                    "manager request usage passes through"
                );
                assert_eq!(
                    context_tokens,
                    Some(30),
                    "main-stream usage carries its context footprint"
                );
                saw_live_usage = true;
            }
            Some(Event::UsageSnapshot { usage, .. }) => {
                assert_eq!(usage.input_tokens, 15, "manager + worker input");
                assert_eq!(usage.output_tokens, 27, "manager + worker output");
                assert_eq!(usage.total_tokens, 42, "manager + worker total");
                saw_snapshot = true;
            }
            Some(Event::StreamDone { .. }) => {}
            Some(_) => {}
            None => break,
        }
    }
    assert!(saw_worker_start, "expected WorkerStarted");
    assert!(saw_worker_tool, "expected nested tool event");
    assert!(saw_worker_finish, "expected WorkerFinished");
    assert!(saw_live_usage, "expected live UsageUpdate");
    assert!(saw_snapshot, "expected turn-end UsageSnapshot");
    let guard = session.lock().await;
    assert_eq!(guard.tokens, 42, "session accumulates combined usage");
    assert_eq!(guard.input_tokens, 15);
    assert_eq!(guard.output_tokens, 27);
}

#[tokio::test]
async fn overflow_without_plan_reports_error_without_compaction_events() {
    // A session with no persisted id has nothing compactable: the overflow
    // path must still emit the budget StreamError and the Overflowed
    // outcome, but no CompactionStarted/Finished pair (there is no
    // summarizer call to bracket).
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(16);

    let stream: shuvarie_llm::StreamStream =
        Box::pin(futures_util::stream::iter(vec![StreamItem::Overflow]));

    let session_shared = session.clone();
    let store = Store::open_in_memory().await.unwrap();
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });

    let mut budget_error = false;
    let mut unexpected = false;
    loop {
        match event_rx.recv().await {
            Some(Event::StreamError { error }) => {
                assert!(error.contains("context budget exceeded"));
                budget_error = true;
            }
            Some(Event::CompactionStarted) | Some(Event::CompactionFinished) => {
                unexpected = true;
            }
            Some(_) => {}
            None => break,
        }
    }
    assert!(budget_error, "overflow must report the budget error");
    assert!(
        !unexpected,
        "no compaction events without a compaction plan"
    );
    assert_eq!(
        stream_done_rx.recv().await,
        Some(StreamOutcome::Overflowed { compacted: false }),
        "uncompacted overflow outcome"
    );
}

/// Spawn the stream task over a fixed item list with a store-backed
/// session row, mirroring production where the session row exists before
/// the stream starts.
async fn spawn_preempt_stream(
    items: Vec<StreamItem>,
    steer: SteerSignal,
    deny_cut: crate::permissions::DenyCut,
) -> (
    Arc<Mutex<Session>>,
    tokio::sync::mpsc::Receiver<Event>,
    tokio::sync::mpsc::Receiver<StreamOutcome>,
) {
    spawn_stream_core(Box::pin(futures_util::stream::iter(items)), steer, deny_cut).await
}

async fn spawn_stream_core(
    stream: shuvarie_llm::StreamStream,
    steer: SteerSignal,
    deny_cut: crate::permissions::DenyCut,
) -> (
    Arc<Mutex<Session>>,
    tokio::sync::mpsc::Receiver<Event>,
    tokio::sync::mpsc::Receiver<StreamOutcome>,
) {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("preempt", None, None, None)
        .await
        .unwrap();
    session.lock().await.id = Some(id);
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, stream_done_rx) = tokio::sync::mpsc::channel(1);
    let session_shared = session.clone();
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            steer,
            deny_cut,
        )
        .await;
    });
    (session, event_rx, stream_done_rx)
}

fn main_tool_start(name: &str, call_id: &str) -> StreamItem {
    StreamItem::ToolStart {
        name: name.into(),
        args: serde_json::json!({}),
        worker: None,
        call_id: call_id.into(),
    }
}

fn main_tool_result(name: &str, call_id: &str) -> StreamItem {
    StreamItem::ToolResult {
        name: name.into(),
        output: "ok".into(),
        ok: true,
        worker: None,
        file_change: None,
        streams: None,
        call_id: call_id.into(),
    }
}

fn worker_tool_result(call_id: &str) -> StreamItem {
    StreamItem::ToolResult {
        name: "grep".into(),
        output: "found".into(),
        ok: true,
        worker: Some("explore_workspace".into()),
        file_change: None,
        streams: None,
        call_id: call_id.into(),
    }
}
#[tokio::test]
async fn steered_prompt_cuts_after_text_segment_before_tools_run() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Delta {
                text: "plan".into(),
            },
            main_tool_start("read_file", "c1"),
            main_tool_result("read_file", "c1"),
            StreamItem::Done {
                text: "plan".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_cancel = false;
    let mut saw_tool = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::ToolStarted { .. } => saw_tool = true,
            Event::StreamCancelled => {
                saw_cancel = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_cancel, "cut emitted StreamCancelled");
    assert!(
        !saw_tool,
        "the committed tool call must never execute once steered"
    );
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    let guard = session.lock().await;
    assert_eq!(
        guard.messages.last().map(|m| m.content.as_str()),
        Some("plan"),
        "partial text persisted as the interrupted assistant message"
    );
    assert!(guard.tool_records.is_empty(), "no tool ran");
}

#[tokio::test]
async fn steered_prompt_cuts_after_thinking_segment() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Reasoning {
                text: "thinking it through".into(),
            },
            StreamItem::Delta {
                text: "the answer".into(),
            },
            StreamItem::Done {
                text: "the answer".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_cancel = false;
    let mut saw_token = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::TokenReceived { .. } => saw_token = true,
            Event::StreamCancelled => {
                saw_cancel = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_cancel);
    assert!(
        !saw_token,
        "text following the thinking segment must never stream"
    );
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    let guard = session.lock().await;
    assert_eq!(guard.messages.len(), 1);
    assert_eq!(guard.messages[0].content, "");
    assert_eq!(guard.interrupted.get(&0), Some(&true));
    let reasoning = guard.reasoning.get(&0).expect("reasoning persisted");
    assert_eq!(reasoning.len(), 1);
    assert_eq!(reasoning[0].text, "thinking it through");
}

#[tokio::test]
async fn steered_prompt_waits_for_full_turn_without_boundary() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Delta { text: "hi".into() },
            StreamItem::Done {
                text: "hi".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_done = false;
    while let Some(event) = event_rx.recv().await {
        if matches!(event, Event::StreamDone { .. }) {
            saw_done = true;
        }
    }
    assert!(saw_done, "no boundary crossed: the turn completes normally");
    assert_eq!(
        stream_done_rx.recv().await,
        Some(StreamOutcome::Finished),
        "armed steering must not preempt a turn that never crosses a boundary"
    );
    let guard = session.lock().await;
    assert!(guard.interrupted.is_empty(), "turn committed cleanly");
}

#[tokio::test]
async fn steered_prompt_ignores_worker_activity() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::WorkerStart {
                name: "explore_workspace".into(),
                args: serde_json::json!({}),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({}),
                worker: Some("explore_workspace".into()),
                call_id: "t1".into(),
            },
            worker_tool_result("t1"),
            StreamItem::Delta {
                text: "straggler-era delta".into(),
            },
            StreamItem::Done {
                text: "x".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    // The worker's internal tool call is not the batch settling (the
    // worker tool itself is still running), so no cut may fire — the
    // following text delta streams and the turn completes normally.
    let mut saw_token = false;
    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::TokenReceived { .. } => saw_token = true,
            Event::StreamCancelled => saw_cancel = true,
            _ => {}
        }
    }
    assert!(saw_token, "worker activity must not trigger the cut");
    assert!(!saw_cancel);
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
    assert!(
        session.lock().await.interrupted.is_empty(),
        "the turn committed cleanly"
    );
}

#[tokio::test]
async fn steered_prompt_cuts_when_worker_batch_settles() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::WorkerStart {
                name: "explore_workspace".into(),
                args: serde_json::json!({}),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({}),
                worker: Some("explore_workspace".into()),
                call_id: "t1".into(),
            },
            worker_tool_result("t1"),
            StreamItem::WorkerResult {
                name: "explore_workspace".into(),
                output: "summary".into(),
                ok: true,
                call_id: "w1".into(),
            },
            StreamItem::Delta {
                text: "next round".into(),
            },
            StreamItem::Done {
                text: "next round".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_worker_finish = false;
    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::WorkerFinished { .. } => saw_worker_finish = true,
            Event::StreamCancelled => {
                saw_cancel = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_worker_finish);
    assert!(saw_cancel, "cut fires after the worker batch settles");
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    let guard = session.lock().await;
    assert_eq!(
        guard.tool_records.len(),
        1,
        "the worker's internal tool call stays persisted"
    );
    assert_eq!(
        guard.tool_records[0].worker.as_deref(),
        Some("explore_workspace")
    );
}

#[tokio::test]
async fn steered_prompt_cuts_only_after_whole_multi_tool_batch() {
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            main_tool_start("read_file", "c1"),
            main_tool_start("read_file", "c2"),
            main_tool_result("read_file", "c1"),
            main_tool_result("read_file", "c2"),
            StreamItem::Delta {
                text: "next round".into(),
            },
            StreamItem::Done {
                text: "next round".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut tool_finishes = 0;
    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::ToolFinished { .. } => tool_finishes += 1,
            Event::StreamCancelled => {
                saw_cancel = true;
                break;
            }
            _ => {}
        }
    }
    assert_eq!(
        tool_finishes, 2,
        "both batch results surface before the cut"
    );
    assert!(saw_cancel, "cut fires once the whole batch settled");
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    assert_eq!(session.lock().await.tool_records.len(), 2);
}

#[tokio::test]
async fn permission_deny_cuts_the_turn_after_the_denied_result() {
    let deny_cut = DenyCut::default();
    let wait_cut = deny_cut.clone();
    let stream: shuvarie_llm::StreamStream = Box::pin(
        futures_util::stream::iter(vec![
            main_tool_start("read_file", "c1"),
            StreamItem::ToolResult {
                name: "read_file".into(),
                output: "permission denied: paths: deny \".env\"".into(),
                ok: false,
                worker: None,
                file_change: None,
                streams: None,
                call_id: "c1".into(),
            },
            StreamItem::Done {
                text: "the model keeps going".into(),
                usage: TokenUsage::default(),
            },
        ])
        .enumerate()
        .then(move |(i, item)| {
            let wait_cut = wait_cut.clone();
            async move {
                // The tool denies while its call runs: the producer only
                // yields the result once the denial happened.
                if i == 1 {
                    while !wait_cut.is_set() {
                        tokio::task::yield_now().await;
                    }
                }
                item
            }
        }),
    );
    let (session, mut event_rx, mut stream_done_rx) =
        spawn_stream_core(stream, SteerSignal::default(), deny_cut.clone()).await;

    // The tool denies while its call runs: after the start, before the
    // result is streamed.
    let mut saw_start = false;
    let denied;
    loop {
        let event = event_rx.recv().await.expect("events before the cut");
        match event {
            Event::ToolStarted { .. } => {
                saw_start = true;
                deny_cut.trigger();
            }
            Event::ToolFinished { ok, output, .. } => {
                denied = Some((ok, output));
                break;
            }
            _ => {}
        }
    }
    assert!(saw_start);
    let (ok, output) = denied.expect("the denied call finished before the cut");
    assert!(!ok, "the denial is a failed result");
    assert!(
        output.contains("permission denied"),
        "the denial reason stays visible: {output}"
    );

    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        if matches!(event, Event::StreamCancelled) {
            saw_cancel = true;
            break;
        }
    }
    assert!(saw_cancel, "the denial cut emits StreamCancelled");
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    assert!(!deny_cut.is_set(), "the stream task consumed the signal");

    let guard = session.lock().await;
    assert_eq!(guard.interrupted.get(&0), Some(&true));
    assert_eq!(guard.tool_records.len(), 1);
    let record = &guard.tool_records[0];
    assert!(
        !record.killed,
        "the denied call returned, so it is not killed"
    );
    assert!(!record.ok);
    assert!(
        record.output.contains("permission denied"),
        "{:#}",
        record.output
    );
}

#[tokio::test]
async fn deny_after_done_cannot_discard_a_finished_turn() {
    let deny_cut = DenyCut::default();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            main_tool_start("grep", "c1"),
            main_tool_result("grep", "c1"),
            StreamItem::Done {
                text: "the reply".into(),
                usage: TokenUsage::default(),
            },
        ],
        SteerSignal::default(),
        deny_cut.clone(),
    )
    .await;

    let mut saw_done = false;
    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::StreamDone { .. } => {
                saw_done = true;
                deny_cut.trigger();
            }
            Event::StreamCancelled => {
                saw_cancel = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_done, "the finished turn committed");
    assert!(
        !saw_cancel,
        "a straggling denial cannot cut a finished turn"
    );
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
    let guard = session.lock().await;
    assert!(guard.interrupted.is_empty());
    assert_eq!(
        guard.messages.last().map(|m| m.content.as_str()),
        Some("the reply")
    );
}

#[tokio::test]
async fn steered_prompt_waits_for_thinking_to_complete() {
    // Mid-segment deltas continue the running thinking action: the cut
    // fires only once the segment completed (the text action starting),
    // and the text delta is consumed without streaming.
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Reasoning {
                text: "first burst".into(),
            },
            StreamItem::Reasoning {
                text: " — second burst".into(),
            },
            StreamItem::Delta {
                text: "the answer".into(),
            },
            StreamItem::Done {
                text: "the answer".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut reasoning_chunks = 0;
    let mut saw_token = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::ReasoningReceived { .. } => reasoning_chunks += 1,
            Event::TokenReceived { .. } => saw_token = true,
            Event::StreamCancelled => break,
            _ => {}
        }
    }
    assert_eq!(
        reasoning_chunks, 2,
        "both thinking deltas stream before the cut"
    );
    assert!(!saw_token, "the text action never streams once steered");
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    let guard = session.lock().await;
    assert_eq!(guard.interrupted.get(&0), Some(&true));
    let reasoning = guard.reasoning.get(&0).expect("reasoning persisted");
    assert_eq!(reasoning.len(), 1, "both bursts append into one segment");
    assert_eq!(reasoning[0].text, "first burst — second burst");
}

#[tokio::test]
async fn steered_prompt_waits_for_text_to_complete() {
    // Steering while the model writes its answer waits for that segment to
    // complete; with no further action the turn finishes normally and the
    // prompt is dispatched at turn end (committed cleanly).
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Delta { text: "one".into() },
            StreamItem::Delta {
                text: " two".into(),
            },
            StreamItem::Done {
                text: "one two".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut tokens = 0;
    while let Some(event) = event_rx.recv().await {
        if let Event::TokenReceived { .. } = event {
            tokens += 1;
        }
    }
    assert_eq!(tokens, 2, "the whole text segment streams");
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
    let guard = session.lock().await;
    assert!(guard.interrupted.is_empty(), "turn committed cleanly");
}

#[tokio::test]
async fn steered_prompt_cuts_when_worker_straggler_result_lags() {
    // A worker's internal tool result may straggle past its
    // `WorkerResult` on the merged stream; the lagging pair must not
    // keep the main batch from settling, so the cut still fires.
    let steer = SteerSignal::default();
    steer.arm();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::WorkerStart {
                name: "explore_workspace".into(),
                args: serde_json::json!({}),
                call_id: "w1".into(),
            },
            StreamItem::ToolStart {
                name: "grep".into(),
                args: serde_json::json!({}),
                worker: Some("explore_workspace".into()),
                call_id: "t1".into(),
            },
            StreamItem::WorkerResult {
                name: "explore_workspace".into(),
                output: "summary".into(),
                ok: true,
                call_id: "w1".into(),
            },
            worker_tool_result("t1"),
            StreamItem::Delta {
                text: "next round".into(),
            },
            StreamItem::Done {
                text: "next round".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_straggler = false;
    while let Some(event) = event_rx.recv().await {
        match event {
            Event::ToolFinished { name, .. } if name == "grep" => {
                saw_straggler = true;
            }
            Event::StreamCancelled => break,
            _ => {}
        }
    }
    assert!(
        saw_straggler,
        "the lagging worker-internal result still surfaces"
    );
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    let guard = session.lock().await;
    assert_eq!(guard.tool_records.len(), 1);
}

#[tokio::test]
async fn unarmed_signal_never_cuts() {
    let steer = SteerSignal::default();
    let (session, mut event_rx, mut stream_done_rx) = spawn_preempt_stream(
        vec![
            StreamItem::Delta { text: "a".into() },
            main_tool_start("read_file", "c1"),
            main_tool_result("read_file", "c1"),
            StreamItem::Done {
                text: "a".into(),
                usage: TokenUsage::default(),
            },
        ],
        steer,
        DenyCut::default(),
    )
    .await;

    let mut saw_cancel = false;
    while let Some(event) = event_rx.recv().await {
        if matches!(event, Event::StreamCancelled) {
            saw_cancel = true;
        }
    }
    assert!(!saw_cancel);
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Finished));
    assert!(session.lock().await.interrupted.is_empty());
}

#[tokio::test]
async fn steered_prompt_cuts_mid_stream_when_armed_between_actions() {
    // Drives the stream from a channel so the signal can be armed after
    // the text action started, mirroring a user submitting mid-stream:
    // the cut lands at the next action boundary (before the second tool
    // call executes).
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("interleaved", None, None, None)
        .await
        .unwrap();
    session.lock().await.id = Some(id);
    let (item_tx, item_rx) = tokio::sync::mpsc::channel::<StreamItem>(8);
    let stream: shuvarie_llm::StreamStream =
        Box::pin(futures_util::stream::unfold(item_rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }));
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, mut stream_done_rx) = tokio::sync::mpsc::channel(1);
    let steer = SteerSignal::default();
    let session_shared = session.clone();
    let steer_shared = steer.clone();
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state,
            stream_done_tx,
            steer_shared,
            DenyCut::default(),
        )
        .await;
    });

    let _ = item_tx
        .send(StreamItem::Delta {
            text: "working".into(),
        })
        .await;
    match event_rx.recv().await {
        Some(Event::TokenReceived { content }) => assert_eq!(content, "working"),
        other => panic!("expected token event, got {other:?}"),
    }
    let _ = item_tx.send(main_tool_start("edit_file", "c1")).await;
    match event_rx.recv().await {
        Some(Event::ToolStarted { name, .. }) => assert_eq!(name, "edit_file"),
        other => panic!("expected tool start, got {other:?}"),
    }
    // The user steers while the tool runs; the cut must wait for the
    // batch to settle, then fire before the next segment.
    steer.arm();
    let _ = item_tx.send(main_tool_result("edit_file", "c1")).await;
    match event_rx.recv().await {
        Some(Event::ToolFinished { name, .. }) => assert_eq!(name, "edit_file"),
        other => panic!("expected tool finish, got {other:?}"),
    }
    let _ = item_tx
        .send(StreamItem::Delta {
            text: "should never stream".into(),
        })
        .await;
    match event_rx.recv().await {
        Some(Event::StreamCancelled) => {}
        other => panic!("expected cancellation at the boundary, got {other:?}"),
    }
    assert_eq!(stream_done_rx.recv().await, Some(StreamOutcome::Preempted));
    drop(item_tx);
    let guard = session.lock().await;
    assert_eq!(guard.messages.len(), 1);
    assert_eq!(guard.messages[0].content, "working");
    assert_eq!(guard.interrupted.get(&0), Some(&true));
    assert_eq!(guard.tool_records.len(), 1);
}

#[tokio::test]
async fn interrupted_turn_persists_pending_tools_as_killed() {
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store.create_session("cut", None, None, None).await.unwrap();
    {
        let mut guard = session.lock().await;
        guard.id = Some(id);
        guard.push_user("go");
    }
    store
        .append_message(id, None, shuvarie_llm::Role::User, "go")
        .await
        .unwrap();
    let user_row = store.load_session(id).await.unwrap().messages[0].id;
    let assistant = store
        .append_message(id, Some(user_row), shuvarie_llm::Role::Assistant, "")
        .await
        .unwrap();

    let turn_state = Arc::new(Mutex::new(TurnState {
        assistant_message_id: Some(assistant.id),
        assistant_seq: 1,
        text_segments: vec![shuvarie_db::TextSegment {
            after_tool: 0,
            text: "partial".into(),
        }],
        tool_records: vec![crate::tool_record::ToolRecord {
            name: "read_file".into(),
            args_json: "{}".into(),
            output: "out".into(),
            stderr: String::new(),
            ok: true,
            killed: false,
            worker: None,
            message_id: assistant.id,
            message_seq: 1,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 5,
        }],
        pending_tools: vec![PendingToolCall {
            call_id: "c2".into(),
            name: "run_shell".into(),
            args_json: r#"{"command":"sleep 30"}"#.into(),
            worker: None,
            started: std::time::Instant::now(),
        }],
        ..TurnState::default()
    }));

    persist_interrupted_turn(
        Some(turn_state.clone()),
        &mut store,
        &Some(session.clone()),
        &event_tx,
    )
    .await;

    let stored = store.load_session(id).await.unwrap();
    let killed: Vec<_> = stored.tool_calls.iter().filter(|tc| tc.killed).collect();
    let finished: Vec<_> = stored.tool_calls.iter().filter(|tc| !tc.killed).collect();
    assert_eq!(killed.len(), 1, "the pending call persists as killed");
    assert!(finished.is_empty(), "only the kill was written here");
    assert_eq!(killed[0].name, "run_shell");
    assert_eq!(killed[0].seq, 1, "seq continues after the finished calls");
    assert!(!killed[0].ok);
    assert_eq!(killed[0].message_id, assistant.id);

    let guard = session.lock().await;
    assert_eq!(guard.messages.len(), 2);
    assert_eq!(guard.interrupted.get(&1), Some(&true));
    assert_eq!(guard.tool_records.len(), 2);
    assert!(!guard.tool_records[0].killed);
    assert!(guard.tool_records[1].killed);
    assert_eq!(guard.tool_records[1].message_seq, 1);
    assert_eq!(guard.tool_records[1].name, "run_shell");
}

#[tokio::test]
async fn tool_result_clears_the_pending_call() {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("settle", None, None, None)
        .await
        .unwrap();
    session.lock().await.id = Some(id);
    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
        main_tool_start("edit_file", "c1"),
        main_tool_result("edit_file", "c1"),
        StreamItem::Done {
            text: "done".into(),
            usage: TokenUsage::default(),
        },
    ]));
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
    let session_shared = session.clone();
    let turn_state_shared = turn_state.clone();
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state_shared,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });
    while let Some(event) = event_rx.recv().await {
        if matches!(event, Event::StreamDone { .. }) {
            break;
        }
    }
    assert!(turn_state.lock().await.pending_tools.is_empty());
    let guard = session.lock().await;
    assert_eq!(guard.tool_records.len(), 1);
    assert!(!guard.tool_records[0].killed);
}

#[tokio::test]
async fn text_runs_seal_at_tool_gaps() {
    let client = ProviderClient::build(ProviderType::Ollama, None, None).unwrap();
    let session = Arc::new(Mutex::new(Session::new()));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let mut store = Store::open_in_memory().await.unwrap();
    let id = store
        .create_session("runs", None, None, None)
        .await
        .unwrap();
    {
        let mut guard = session.lock().await;
        guard.id = Some(id);
        guard.push_user("go");
    }
    store
        .append_message(id, None, shuvarie_llm::Role::User, "go")
        .await
        .unwrap();
    let stream: shuvarie_llm::StreamStream = Box::pin(futures_util::stream::iter(vec![
        StreamItem::Delta {
            text: "start".into(),
        },
        main_tool_start("read_file", "c1"),
        main_tool_result("read_file", "c1"),
        StreamItem::Delta {
            text: "\n\nresumed".into(),
        },
        StreamItem::Done {
            text: "start\n\nresumed".into(),
            usage: TokenUsage::default(),
        },
    ]));
    let worker_usage = Arc::new(std::sync::Mutex::new(TokenUsage::default()));
    let turn_state = Arc::new(Mutex::new(TurnState::default()));
    let (stream_done_tx, _stream_done_rx) = tokio::sync::mpsc::channel(1);
    let session_shared = session.clone();
    let turn_state_shared = turn_state.clone();
    let store_shared = store.clone();
    tokio::spawn(async move {
        stream_stream_to_events(
            stream,
            session_shared,
            client,
            None,
            store_shared,
            "ollama-model".into(),
            20_000,
            worker_usage,
            None,
            event_tx,
            turn_state_shared,
            stream_done_tx,
            SteerSignal::default(),
            DenyCut::default(),
        )
        .await;
    });
    while let Some(event) = event_rx.recv().await {
        if matches!(event, Event::StreamDone { .. }) {
            break;
        }
    }
    let stored = store.load_session(id).await.unwrap();
    let assistant = &stored.messages[1];
    assert_eq!(assistant.content, "start\n\nresumed");
    assert_eq!(
        assistant.text_segments,
        vec![
            shuvarie_db::TextSegment {
                after_tool: 0,
                text: "start".into(),
            },
            shuvarie_db::TextSegment {
                after_tool: 1,
                text: "\n\nresumed".into(),
            },
        ],
        "the resumed run seals at the tool gap"
    );
    let guard = session.lock().await;
    assert_eq!(
        guard.text_segments.get(&1).map(Vec::as_slice),
        Some(assistant.text_segments.as_slice())
    );
}

async fn chain_session() -> (Store, uuid::Uuid, Vec<u64>) {
    let mut store = Store::open_in_memory().await.unwrap();
    let sid = store
        .create_session("fork", None, None, None)
        .await
        .unwrap();
    let user = store
        .append_message(sid, None, shuvarie_llm::Role::User, "one")
        .await
        .unwrap();
    let a1 = store
        .append_message(sid, Some(user.id), shuvarie_llm::Role::Assistant, "r1")
        .await
        .unwrap();
    let u2 = store
        .append_message(sid, Some(a1.id), shuvarie_llm::Role::User, "two")
        .await
        .unwrap();
    let a2 = store
        .append_message(sid, Some(u2.id), shuvarie_llm::Role::Assistant, "r2")
        .await
        .unwrap();
    store.set_active_leaf(sid, Some(a2.id)).await.unwrap();
    (store, sid, vec![user.id, a1.id, u2.id, a2.id])
}

#[tokio::test]
async fn fork_session_undo_forks_before_the_last_user_prompt() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, None, false, false, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt.as_deref(), Some("two"), "the prompt is recalled");

    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(stored.leaf_id, Some(ids[1]), "the tip walks back to a1");
    assert_eq!(
        stored.messages.len(),
        4,
        "forking keeps every row in the tree"
    );
}

#[tokio::test]
async fn fork_session_at_an_assistant_node_forks_before_it() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, Some(ids[1]), false, false, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt.as_deref(), Some("r1"), "the reply is recalled");
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(
        stored.leaf_id,
        Some(ids[0]),
        "the tip walks back before the reply"
    );
}

#[tokio::test]
async fn fork_session_at_a_user_node_forks_before_it() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, Some(ids[2]), false, false, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt.as_deref(), Some("two"), "the prompt is recalled");
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(
        stored.leaf_id,
        Some(ids[1]),
        "the tip walks back before the prompt"
    );
}

#[tokio::test]
async fn fork_session_at_the_root_prompt_starts_over() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, Some(ids[0]), false, false, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt.as_deref(), Some("one"));
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(
        stored.leaf_id,
        Some(shuvarie_db::EMPTY_LEAF),
        "the active path is empty again"
    );
    assert_eq!(stored.messages.len(), 4, "every row stays in the tree");

    let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
    assert!(
        loaded.messages.is_empty(),
        "a reload of a cleared path shows an empty chat"
    );
    assert_eq!(loaded.leaf_id, None);
}

#[tokio::test]
async fn fork_session_at_a_summary_node_walks_to_it() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let summary = store
        .append_summary(sid, Some(ids[1]), "the talk so far")
        .await
        .unwrap();

    let prompt = fork_session(
        &mut store,
        sid,
        Some(summary.id),
        false,
        false,
        None,
        &event_tx,
    )
    .await
    .unwrap();
    assert_eq!(prompt, None, "summary markers carry no recall");
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(stored.leaf_id, Some(summary.id));
}

#[tokio::test]
async fn fork_session_after_a_turn_walks_to_the_reply() {
    let (mut store, sid, ids) = chain_session().await;
    store
        .append_tool_call(
            sid,
            ids[1],
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
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, Some(ids[1]), false, true, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt, None, "walk-to forks recall nothing");
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(
        stored.leaf_id,
        Some(ids[1]),
        "the tip walks to the reply itself"
    );

    let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
    assert_eq!(
        loaded.messages.len(),
        2,
        "the path keeps the prompt and the reply"
    );
    assert_eq!(
        loaded.tool_records.len(),
        1,
        "the reply's tool calls stay on the path"
    );
}

#[tokio::test]
async fn fork_session_without_summarizer_errors_on_summarize() {
    let (mut store, sid, ids) = chain_session().await;
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let err = fork_session(&mut store, sid, Some(ids[2]), true, false, None, &event_tx)
        .await
        .unwrap_err();
    assert!(
        err.contains("without an active provider"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn fork_session_undo_on_a_root_only_session_starts_over() {
    let mut store = Store::open_in_memory().await.unwrap();
    let sid = store
        .create_session("solo", None, None, None)
        .await
        .unwrap();
    store
        .append_message(sid, None, shuvarie_llm::Role::User, "only")
        .await
        .unwrap();
    let (event_tx, _event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let prompt = fork_session(&mut store, sid, None, false, false, None, &event_tx)
        .await
        .unwrap();
    assert_eq!(prompt.as_deref(), Some("only"));
    let stored = store.load_session(sid).await.unwrap();
    assert_eq!(
        stored.leaf_id,
        Some(shuvarie_db::EMPTY_LEAF),
        "the active path is empty again"
    );
    assert_eq!(stored.messages.len(), 1, "the root prompt survives");

    let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
    assert!(
        loaded.messages.is_empty(),
        "a reload of a cleared path shows an empty chat"
    );

    let restart = store
        .append_message(sid, None, shuvarie_llm::Role::User, "again")
        .await
        .unwrap();
    let loaded = Session::from_stored(store.load_session(sid).await.unwrap());
    assert_eq!(
        loaded.messages.len(),
        1,
        "a turn after the undo starts a fresh root prompt"
    );
    assert_eq!(loaded.messages[0].content, "again");
    assert_eq!(loaded.leaf_id, Some(restart.id));
}

#[tokio::test]
async fn delete_branch_refuses_the_active_leaf() {
    let (mut store, sid, ids) = chain_session().await;
    let stored = store.load_session(sid).await.unwrap();
    assert!(subtree_contains(&stored, ids[0], stored.leaf_id));
}

#[tokio::test]
async fn shell_chunks_resolve_to_their_own_concurrent_calls() {
    // One worker runs two `run_shell` calls concurrently: each streamed
    // chunk resolves against the pending calls' args and must carry its
    // own call id. A chunk arriving after its call settled (no pending
    // candidate left) resolves to `None`.
    let items = vec![
        StreamItem::WorkerStart {
            name: "run_tests".into(),
            args: serde_json::json!({ "task": "verify" }),
            call_id: "w1".into(),
        },
        StreamItem::ToolStart {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            worker: Some("run_tests".into()),
            call_id: "s1".into(),
        },
        StreamItem::ToolStart {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo clippy" }),
            worker: Some("run_tests".into()),
            call_id: "s2".into(),
        },
        StreamItem::ShellOutput {
            worker: Some("run_tests".into()),
            command: "cargo clippy".into(),
            stdout: "clippy tail".into(),
            stderr: String::new(),
        },
        StreamItem::ShellOutput {
            worker: Some("run_tests".into()),
            command: "cargo test".into(),
            stdout: "test tail".into(),
            stderr: String::new(),
        },
        StreamItem::ToolResult {
            name: "run_shell".into(),
            output: "exit 0\ntest tail".into(),
            ok: true,
            worker: Some("run_tests".into()),
            file_change: None,
            streams: None,
            call_id: "s1".into(),
        },
        StreamItem::ShellOutput {
            worker: Some("run_tests".into()),
            command: "cargo test".into(),
            stdout: "straggler".into(),
            stderr: String::new(),
        },
        StreamItem::WorkerResult {
            name: "run_tests".into(),
            output: "done".into(),
            ok: true,
            call_id: "w1".into(),
        },
        StreamItem::Done {
            text: "done".into(),
            usage: TokenUsage::default(),
        },
    ];
    let (_session, mut event_rx, mut done_rx) =
        spawn_preempt_stream(items, SteerSignal::default(), DenyCut::default()).await;

    let mut chunks: Vec<(Option<String>, Option<String>, String)> = Vec::new();
    while let Some(event) = event_rx.recv().await {
        if let Event::ToolOutput {
            worker,
            call_id,
            stdout,
            ..
        } = event
        {
            chunks.push((worker, call_id, stdout));
        }
    }
    assert_eq!(
        done_rx.recv().await,
        Some(StreamOutcome::Finished),
        "the turn completes normally"
    );
    assert_eq!(
        chunks,
        vec![
            (
                Some("run_tests".into()),
                Some("s2".into()),
                "clippy tail".into()
            ),
            (
                Some("run_tests".into()),
                Some("s1".into()),
                "test tail".into()
            ),
            (Some("run_tests".into()), None, "straggler".into()),
        ],
        "each chunk routes to its own call; the settled call's chunk is unresolved"
    );
}

#[tokio::test]
async fn ambiguous_shell_chunk_stays_unresolved() {
    // Two concurrent calls running the identical command cannot be told
    // apart: the chunk resolves to `None` instead of guessing.
    let items = vec![
        StreamItem::WorkerStart {
            name: "run_tests".into(),
            args: serde_json::json!({ "task": "verify" }),
            call_id: "w1".into(),
        },
        StreamItem::ToolStart {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            worker: Some("run_tests".into()),
            call_id: "s1".into(),
        },
        StreamItem::ToolStart {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            worker: Some("run_tests".into()),
            call_id: "s2".into(),
        },
        StreamItem::ShellOutput {
            worker: Some("run_tests".into()),
            command: "cargo test".into(),
            stdout: "tail".into(),
            stderr: String::new(),
        },
        StreamItem::Done {
            text: "done".into(),
            usage: TokenUsage::default(),
        },
    ];
    let (_session, mut event_rx, _done_rx) =
        spawn_preempt_stream(items, SteerSignal::default(), DenyCut::default()).await;

    while let Some(event) = event_rx.recv().await {
        if let Event::ToolOutput { call_id, .. } = event {
            assert_eq!(call_id, None, "identical commands stay ambiguous");
        }
    }
}

#[tokio::test]
async fn merged_shell_chunks_follow_their_tool_start() {
    // The shell sub-stream polls after the LLM stream, so a call's
    // ToolStart event precedes its own chunk even when both are ready at
    // the same poll — the block the chunk streams into exists by then.
    let (chunk_tx, chunk_rx) = tokio::sync::mpsc::channel::<crate::tools::ShellChunk>(4);
    let _ = chunk_tx
        .send(crate::tools::ShellChunk {
            worker: Some("run_tests".into()),
            command: "cargo test".into(),
            stdout: "tail".into(),
            stderr: String::new(),
        })
        .await;
    drop(chunk_tx);
    let llm: shuvarie_llm::StreamStream =
        Box::pin(futures_util::stream::iter(vec![StreamItem::ToolStart {
            name: "run_shell".into(),
            args: serde_json::json!({ "command": "cargo test" }),
            worker: Some("run_tests".into()),
            call_id: "s1".into(),
        }]));
    let merged = merge_shell_chunks(llm, chunk_rx);

    let mut items = Vec::new();
    let mut merged = merged;
    while let Some(item) = merged.next().await {
        items.push(item);
    }
    assert_eq!(items.len(), 2, "the chunk stream ends after senders drop");
    assert!(
        matches!(&items[0], StreamItem::ToolStart { call_id, .. } if call_id == "s1"),
        "the ToolStart is polled first: {items:?}"
    );
    assert!(
        matches!(&items[1], StreamItem::ShellOutput { command, .. } if command == "cargo test"),
        "the chunk follows: {items:?}"
    );
}
