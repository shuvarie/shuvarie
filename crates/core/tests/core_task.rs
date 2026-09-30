use std::path::PathBuf;

use shuvarie_core::{
    Active, Command, Config, Connections, Event, ProviderConfig, Session, StartupSession,
    TitleConfig, TrustGrants, run,
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

fn temp_config_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "shuvarie-test-{name}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    dir.join("shuvarie").join("config.kdl")
}

async fn recv_skills_loaded(
    event_rx: &mut tokio::sync::mpsc::Receiver<Event>,
) -> Vec<shuvarie_core::McpStatus> {
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::SkillsLoaded { .. }),
        "expected SkillsLoaded as the first startup event, got {ev:?}"
    );
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::CustomCommandsLoaded { .. }),
        "expected CustomCommandsLoaded as the second startup event, got {ev:?}"
    );
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::ScenesLoaded { .. }),
        "expected ScenesLoaded as the third startup event, got {ev:?}"
    );
    let ev = event_rx.recv().await.expect("event");
    match ev {
        Event::McpStatus { servers } => servers,
        other => panic!("expected McpStatus as the fourth startup event, got {other:?}"),
    }
}

#[tokio::test]
async fn scenes_loaded_carries_conflict_warnings() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let scene_set = shuvarie_core::SceneSet {
        scenes: Default::default(),
        warnings: vec![
            "scene `Plan` is defined multiple times in the local config \
             (shuvarie.kdl, scene.d/plan.kdl); loading none of them"
                .to_string(),
        ],
    };

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        scene_set,
        cmd_rx,
        event_tx,
    ));
    let first = event_rx.recv().await.expect("event");
    assert!(
        matches!(first, Event::SkillsLoaded { .. }),
        "expected SkillsLoaded first, got {first:?}"
    );
    let ev = event_rx.recv().await.expect("event");
    assert!(
        matches!(ev, Event::CustomCommandsLoaded { .. }),
        "expected CustomCommandsLoaded second, got {ev:?}"
    );
    let ev = event_rx.recv().await.expect("event");
    match ev {
        Event::ScenesLoaded { warnings, .. } => {
            assert_eq!(warnings.len(), 1);
            assert!(warnings[0].contains("scene `Plan`"), "{}", warnings[0]);
        }
        other => panic!("expected ScenesLoaded, got {other:?}"),
    }
    drop(cmd_tx);
    let _ = handle.await;
}

/// The builtin scenes materialize at core startup: the picker list carries
/// the code-defined modes (Advisor/Orchestrator/Reviewer) under the built-in
/// default, and a configured scene of the same name merges field-wise over
/// its builtin.
#[tokio::test]
async fn scenes_loaded_lists_the_builtin_scenes_under_the_configured_ones() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let mut scenes = shuvarie_config::ScenesConfig::default();
    scenes.scenes.insert(
        "Draft".into(),
        shuvarie_config::SceneConfig {
            description: Some("draft mode".into()),
            ..Default::default()
        },
    );
    // A same-name builtin redefinition overrides only what it sets.
    let override_kdl = r#"
        scenes {
            scene name="Advisor" {
                description "my advisor"
            }
        }
    "#;
    let user_advisor = shuvarie_config::ScenesConfig::from_kdl(override_kdl).unwrap();
    let mut merged = shuvarie_core::SceneSet {
        scenes,
        warnings: Vec::new(),
    };
    merged.scenes.stack(user_advisor);
    let scene_set = merged;
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        scene_set,
        cmd_rx,
        event_tx,
    ));
    let _ = event_rx.recv().await.expect("SkillsLoaded");
    let _ = event_rx.recv().await.expect("CustomCommandsLoaded");
    let row_names = match event_rx.recv().await.expect("event") {
        Event::ScenesLoaded { scenes: rows, .. } => rows,
        other => panic!("expected ScenesLoaded, got {other:?}"),
    };
    let expected: Vec<(Option<&str>, &str)> = vec![
        (None, "Default"),
        (Some("Advisor"), "Advisor"),
        (Some("Draft"), "Draft"),
        (Some("Orchestrator"), "Orchestrator"),
        (Some("Reviewer"), "Reviewer"),
    ];
    assert_eq!(
        row_names
            .iter()
            .map(|row| (row.id.as_deref(), row.name.as_str()))
            .collect::<Vec<_>>(),
        expected,
        "the builtins sit under the configured scenes in name order"
    );
    for (name, id, description) in [
        ("Default", None, "Built-in behavior, no scene configured"),
        ("Advisor", Some("Advisor"), "my advisor"),
        (
            "Orchestrator",
            Some("Orchestrator"),
            "Agent orchestration: decompose the task, delegate to subagent workers, verify, integrate",
        ),
        (
            "Reviewer",
            Some("Reviewer"),
            "Code review: read the code, run checks, report findings without applying them",
        ),
    ] {
        let row = row_names
            .iter()
            .find(|row| row.name == name)
            .unwrap_or_else(|| panic!("{name} row missing"));
        assert_eq!(row.id.as_deref(), id);
        assert_eq!(row.description.as_deref(), Some(description));
        assert!(row.switchable, "{name} is re-enterable mid-session");
    }
    let draft = row_names.iter().find(|row| row.name == "Draft").unwrap();
    assert_eq!(draft.description.as_deref(), Some("draft mode"));
    assert!(
        !draft.switchable,
        "the interlude-less user scene stays start-only"
    );

    drop(cmd_tx);
    let _ = handle.await;
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

/// A stdio MCP server config whose command does not exist: connect attempts
/// fail fast (spawn error), keeping the lifecycle tests deterministic.
fn mcp_stdio_only_config(command: &str) -> shuvarie_config::McpConfig {
    shuvarie_config::McpConfig {
        stdio: std::collections::BTreeMap::from([(
            "flaky".to_string(),
            shuvarie_config::McpStdioConfig {
                command: command.to_string(),
                args: Vec::new(),
                envs: Default::default(),
            },
        )]),
        http: Default::default(),
    }
}

fn expect_mcp_status(event: Event) -> Vec<shuvarie_core::McpStatus> {
    match event {
        Event::McpStatus { servers } => servers,
        other => panic!("expected McpStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn mcp_startup_snapshot_lists_configured_servers() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let mut config = empty_config();
    config.tools.mcp = mcp_stdio_only_config("shuvarie-definitely-missing-binary-xyz");

    let handle = tokio::spawn(run(
        config,
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    let servers = recv_skills_loaded(&mut event_rx).await;
    assert_eq!(servers.len(), 1);
    let server = &servers[0];
    assert_eq!(server.name, "flaky");
    assert_eq!(server.transport, "stdio");
    assert_eq!(server.state, shuvarie_core::McpStatusState::Configured);
    assert_eq!(server.tools, 0);
    assert!(server.error.is_none());
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn mcp_list_reports_snapshot() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    assert!(recv_skills_loaded(&mut event_rx).await.is_empty());
    cmd_tx.send(Command::McpList).await.unwrap();
    assert!(expect_mcp_status(event_rx.recv().await.expect("event")).is_empty());
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn mcp_reconnect_unknown_server_errors_and_reports_status() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;
    cmd_tx
        .send(Command::McpReconnect {
            name: "nope".into(),
        })
        .await
        .unwrap();
    let ev = event_rx.recv().await.expect("error event");
    match ev {
        Event::McpError { error } => assert!(error.contains("nope"), "{error}"),
        other => panic!("expected McpError, got {other:?}"),
    }
    assert!(expect_mcp_status(event_rx.recv().await.expect("event")).is_empty());
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn mcp_reconnect_failure_shows_per_server_detail() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let mut config = empty_config();
    config.tools.mcp = mcp_stdio_only_config("shuvarie-definitely-missing-binary-xyz");

    let handle = tokio::spawn(run(
        config,
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    let servers = recv_skills_loaded(&mut event_rx).await;
    assert_eq!(
        servers[0].state,
        shuvarie_core::McpStatusState::Configured,
        "startup does not connect; the server shows as configured"
    );
    cmd_tx
        .send(Command::McpReconnect {
            name: "flaky".into(),
        })
        .await
        .unwrap();
    let ev = event_rx.recv().await.expect("error event");
    match ev {
        Event::McpError { error } => assert!(error.contains("flaky"), "{error}"),
        other => panic!("expected McpError, got {other:?}"),
    }
    let servers = expect_mcp_status(event_rx.recv().await.expect("event"));
    assert_eq!(servers[0].state, shuvarie_core::McpStatusState::Failed);
    assert!(
        servers[0].error.is_some(),
        "the snapshot carries the detail"
    );
    drop(cmd_tx);
    let _ = handle.await;
}

/// A minimal OpenAI-compatible chat-completions mock: any request gets one SSE
/// response whose single text delta carries `reply`, then `data: [DONE]`.
fn spawn_mock_openai(reply: &'static str) -> std::net::SocketAddr {
    spawn_mock_openai_recording(reply, Default::default())
}

/// The same mock, also recording every request body it serves: the captured
/// strings let a test assert what the outgoing request carried (e.g. an
/// injected scene interlude).
fn spawn_mock_openai_recording(
    reply: &'static str,
    bodies: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> std::net::SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            use std::io::{Read, Write};
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let mut header_end = None;
            while header_end.is_none() {
                match stream.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(_) => break,
                }
                header_end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
            }
            if let Some(end) = header_end {
                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.trim() == "content-length" {
                            value.trim().parse::<usize>().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                while buf.len() < end + length {
                    match stream.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(_) => break,
                    }
                }
                bodies
                    .lock()
                    .expect("request sink poisoned")
                    .push(String::from_utf8_lossy(&buf[end..]).into_owned());
            }
            let sse = format!(
                "data: {{\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"sequence_number\":1,\"delta\":\"{reply}\"}}\n\n
data: {{\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{{\"id\":\"resp_1\",\"object\":\"response\",\"created_at\":0,\"status\":\"completed\",\"model\":\"test-model\",\"output\":[],\"tools\":[]}}}}\n\n
data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(),
                sse
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    addr
}

/// The `ui.title` `auto-gen` switch: the session is created with the
/// provisional first-prompt title, and the configured provider's model drafts
/// the real title in a background call once the first prompt is in.
#[tokio::test]
async fn auto_gen_drafts_the_session_title_after_the_first_user_prompt() {
    let addr = spawn_mock_openai("Mocked title");
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    let mut config = empty_config();
    config.ui.title = TitleConfig {
        auto_gen: true,
        model: Some("test-model".into()),
        ..TitleConfig::default()
    };

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        config,
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
            model: None,
        })
        .await
        .unwrap();

    // The session is created with the provisional first-prompt title, and a
    // later `SessionTitleChanged` upgrades it to the model's reply.
    let mut provisional: Option<String> = None;
    let mut generated: Option<String> = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while generated.is_none() {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for title events")
            .expect("core task ended");
        match ev {
            Event::SessionCreated { title, .. } => provisional = Some(title),
            Event::SessionTitleChanged { title, .. } => generated = Some(title),
            _ => {}
        }
    }
    assert_eq!(provisional.as_deref(), Some("hello world"));
    assert_eq!(generated.as_deref(), Some("Mocked title"));

    drop(cmd_tx);
    let _ = handle.await;
}

/// The default policy (no `auto-gen`) never spawns a title generation:
/// the provisional title stands and no `SessionTitleChanged` is ever sent.
#[tokio::test]
async fn default_title_policy_never_triggers_generation() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        empty_config(),
        dead_endpoint_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
            model: None,
        })
        .await
        .unwrap();

    let mut saw_created = false;
    while !saw_created {
        let ev = event_rx.recv().await.expect("core task ended");
        if let Event::SessionCreated { title, .. } = ev {
            assert_eq!(title, "hello world");
            saw_created = true;
        }
    }

    // The turn itself fails against the dead endpoint; no title generation
    // runs for this policy, so no title change may arrive in the window.
    let drain = tokio::time::timeout(std::time::Duration::from_millis(300), async {
        while let Some(ev) = event_rx.recv().await {
            if matches!(ev, Event::SessionTitleChanged { .. }) {
                panic!("title generation ran under the default policy");
            }
        }
    })
    .await;
    assert!(
        drain.is_err(),
        "no SessionTitleChanged under the default policy"
    );

    drop(cmd_tx);
    let _ = handle.await;
}

/// The `gen-title` command drafts the session title on demand from the
/// active path's first user prompt — even with `auto-gen` off.
#[tokio::test]
async fn gen_title_command_drafts_the_title_from_the_first_prompt() {
    let addr = spawn_mock_openai("Mocked title");
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    // Default config: `auto-gen` off, so only the manual command may draft.
    let mut config = empty_config();
    config.ui.title.model = Some("test-model".into());

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        config,
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
            model: None,
        })
        .await
        .unwrap();

    let mut saw_created = false;
    while !saw_created {
        let ev = event_rx.recv().await.expect("core task ended");
        if let Event::SessionCreated { title, .. } = ev {
            assert_eq!(title, "hello world");
            saw_created = true;
        }
    }

    cmd_tx.send(Command::GenTitle).await.unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let changed = loop {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for the generated title")
            .expect("core task ended");
        if let Event::SessionTitleChanged { title, .. } = ev {
            break title;
        }
    };
    assert_eq!(changed, "Mocked title");

    drop(cmd_tx);
    let _ = handle.await;
}

/// The `gen-title` command's guards: without a session it reports
/// `no active session`, with a session but no user prompt yet it reports
/// `no user prompt yet`, and with a title provider that resolves no model
/// it reports that no model is available.
#[tokio::test]
async fn gen_title_command_reports_guards_and_resolve_failures() {
    let addr = spawn_mock_openai("Mocked title");
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    // A configured provider connection that does not exist:
    // `resolve_model` never finds it.
    let mut config = empty_config();
    config.ui.title.provider = Some("ghost".into());

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        config,
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    // No session yet: the core reports the guard instead of drafting.
    cmd_tx.send(Command::GenTitle).await.unwrap();
    next_session_error(&mut event_rx, "no active session").await;

    // A fresh session with no prompt on it: the command still refuses.
    cmd_tx.send(Command::NewSession).await.unwrap();
    cmd_tx.send(Command::GenTitle).await.unwrap();
    next_session_error(&mut event_rx, "no user prompt yet").await;

    // With a first prompt on the session, the resolve failure surfaces.
    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
            model: None,
        })
        .await
        .unwrap();
    let mut saw_created = false;
    while !saw_created {
        let ev = event_rx.recv().await.expect("core task ended");
        saw_created = matches!(ev, Event::SessionCreated { .. });
    }
    cmd_tx.send(Command::GenTitle).await.unwrap();
    next_session_error(&mut event_rx, "no model available for title generation").await;

    drop(cmd_tx);
    let _ = handle.await;
}

/// Drain events until the next `SessionError`, which must carry `expected`.
async fn next_session_error(event_rx: &mut tokio::sync::mpsc::Receiver<Event>, expected: &str) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for the gen-title error")
            .expect("core task ended");
        match ev {
            Event::SessionError { error } => {
                assert_eq!(error, expected, "unexpected session error");
                return;
            }
            _ => {}
        }
    }
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
        Default::default(),
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
        Default::default(),
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
async fn set_ui_theme_persists_ui_theme() {
    // The core task persists to the given config path; assert the event is
    // emitted and `ui.theme` lands in the temp config file.
    let config_path = temp_config_path("set-ui-theme");
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);

    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        Some(config_path.clone()),
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::SetUiTheme {
            pref: Some("Kanagawa:wave".into()),
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

    let loaded = Config::load_from(&config_path).expect("load persisted config");
    assert_eq!(loaded.ui.theme.as_deref(), Some("Kanagawa:wave"));

    // Re-selecting the unset default clears the key again.
    cmd_tx
        .send(Command::SetUiTheme { pref: None })
        .await
        .unwrap();
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::ConfigSaved) => break,
            Some(_) => {}
            None => break,
        }
    }
    let loaded = Config::load_from(&config_path).expect("load persisted config");
    assert_eq!(loaded.ui.theme, None, "the unset default is cleared");

    drop(cmd_tx);
    let _ = handle.await;
    let _ = std::fs::remove_file(config_path);
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
        Default::default(),
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;
    cmd_tx
        .send(Command::SendMessage {
            content: "hello".into(),
            model: None,
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
        Default::default(),
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::SendMessage {
            content: "first".into(),
            model: None,
        })
        .await
        .unwrap();
    cmd_tx
        .send(Command::SendMessage {
            content: "second".into(),
            model: None,
        })
        .await
        .unwrap();

    recv_skills_loaded(&mut event_rx).await;
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::SendMessage {
            content: "first".into(),
            model: None,
        })
        .await
        .unwrap();
    cmd_tx
        .send(Command::SendMessage {
            content: "second".into(),
            model: None,
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::SendMessage {
            content: "first".into(),
            model: None,
        })
        .await
        .unwrap();
    cmd_tx
        .send(Command::SendMessage {
            content: "second".into(),
            model: None,
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx
        .send(Command::SendMessage {
            content: "first".into(),
            model: None,
        })
        .await
        .unwrap();
    cmd_tx
        .send(Command::SendMessage {
            content: "second".into(),
            model: None,
        })
        .await
        .unwrap();

    let mut retrying = false;
    // The startup events (skills, custom commands, scenes, MCP) ride the same
    // channel; leave room for them.
    for _ in 0..12 {
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx
        .send(Command::SendMessage {
            content: "hello world".into(),
            model: None,
        })
        .await
        .unwrap();
    // Wait for the stream error (no real ollama running) after the session row is created.
    recv_skills_loaded(&mut event_rx).await;
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
        .create_session("existing", Some("ollama"), Some("model"), None)
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    recv_skills_loaded(&mut event_rx).await;
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
        .create_session("other", Some("ollama"), Some("model"), None)
        .await
        .unwrap();
    let target = store
        .create_session("target", Some("ollama"), Some("model"), None)
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    recv_skills_loaded(&mut event_rx).await;
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
        .create_session("existing", Some("ollama"), Some("model"), None)
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
        Default::default(),
        cmd_rx,
        event_tx,
    ));

    cmd_tx.send(Command::Ping).await.unwrap();

    recv_skills_loaded(&mut event_rx).await;
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
        .create_session("existing", Some("ollama"), Some("model"), None)
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
        Default::default(),
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
        .create_session("t", Some("ollama"), Some("model"), None)
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
        .create_session("t", Some("ollama"), Some("model"), None)
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

/// A mid-session switch only moves the in-memory scene: the DB persist is
/// deferred to the first request under the new scene, which is also what
/// injects the interlude once. Without a prompt (this test sends none after
/// the switches) every persist stays deferred.
#[tokio::test]
async fn switch_scene_defers_the_persist_and_reports() {
    let mut store = Store::open_in_memory().await.unwrap();
    let mut scenes = shuvarie_config::ScenesConfig::default();
    let with_interlude = |interlude: &str| shuvarie_config::SceneConfig {
        system_prompts: shuvarie_config::SystemPromptsConfig {
            interlude: Some(interlude.into()),
            ..Default::default()
        },
        ..shuvarie_config::SceneConfig::default()
    };
    scenes.scenes.insert(
        "Plan".into(),
        shuvarie_config::SceneConfig {
            description: Some("plan first".into()),
            ..with_interlude("we are in Plan mode")
        },
    );
    scenes.scenes.insert(
        "Default".into(),
        shuvarie_config::SceneConfig {
            description: Some("custom default".into()),
            ..with_interlude("we are in the custom Default")
        },
    );
    scenes.scenes.insert(
        "Draft".into(),
        shuvarie_config::SceneConfig {
            description: Some("no interlude".into()),
            ..shuvarie_config::SceneConfig::default()
        },
    );
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        store.clone(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes,
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::Ping).await.unwrap();
    recv_skills_loaded(&mut event_rx).await;
    let _ = event_rx.recv().await.expect("pong");

    // No session yet: a pre-pick records the scene the session will start
    // under (an interlude-less scene may still start a session).
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Draft".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_started = false;
    let mut picked = None;
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SessionStarted) => saw_started = true,
            Some(Event::SceneChanged { name }) => picked = name,
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_started, "the pre-pick starts an in-memory session");
    assert_eq!(picked.as_deref(), Some("Draft"));

    // The first turn creates the row under the pre-picked scene.
    let ev = tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv()).await;
    assert!(
        ev.is_err(),
        "the pre-pick emitted everything before the ping"
    );

    // Create a session, then switch: SceneChanged reports it.
    cmd_tx
        .send(Command::SendMessage {
            content: "hello".into(),
            model: None,
        })
        .await
        .unwrap();
    // SendMessage errors (no provider); wait for the error before switching.
    let mut session_id = None;
    let mut created_scene = None;
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SessionCreated { id, scene, .. }) => {
                session_id = Some(id);
                created_scene = scene;
            }
            Some(Event::StreamError { .. }) => break,
            Some(_) => {}
            None => panic!("core task ended"),
        }
    }
    assert_eq!(
        created_scene.as_deref(),
        Some("Draft"),
        "SessionCreated reports the scene the session started under"
    );
    {
        let stored = store
            .load_session(session_id.expect("session created"))
            .await
            .unwrap();
        assert_eq!(
            stored.scene.as_deref(),
            Some("Draft"),
            "the row is created under the pre-picked scene"
        );
    }

    // Mid-session, the built-in Default switches back (its hard-coded
    // default interlude keeps it switchable).
    cmd_tx
        .send(Command::SwitchScene { name: None })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_builtin = false;
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { name }) => {
                assert_eq!(name, None, "the built-in scene switches by identity");
                saw_builtin = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_builtin, "the built-in scene is switchable mid-session");
    {
        let stored = store
            .load_session(session_id.expect("session created"))
            .await
            .unwrap();
        assert_eq!(
            stored.scene.as_deref(),
            Some("Draft"),
            "the switch only moved the in-memory scene: the persist waits for \
             the first prompt under it"
        );
    }

    // An interlude-bearing configured scene switches mid-session.
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Plan".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_changed = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { name }) => {
                assert_eq!(name.as_deref(), Some("Plan"));
                saw_changed = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_changed, "SceneChanged reported");

    // Re-selecting the current scene is a no-op even mid-session.
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Plan".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { .. }) => {
                panic!("re-selecting the current scene must be a no-op")
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }

    // A configured scene named "Default" switches by identity — it must not
    // collapse into the built-in scene.
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Default".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_custom_default = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { name }) => {
                assert_eq!(name.as_deref(), Some("Default"));
                saw_custom_default = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_custom_default, "the configured Default was switched to");

    // No prompt was sent after the switches: every persist stayed deferred
    // and the row still holds the scene the last prompt announced.
    let stored = store
        .load_session(session_id.expect("session created"))
        .await
        .unwrap();
    assert_eq!(stored.scene.as_deref(), Some("Draft"));
    drop(cmd_tx);
    let _ = handle.await;
}

/// The scene announce: a mid-session switch defers the persist to the first
/// request under the new scene, which is also the only one that carries the
/// interlude.
#[tokio::test]
async fn the_interlude_announces_a_mid_session_switch_once() {
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let addr = spawn_mock_openai_recording("done", bodies.clone());
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    let mut scenes = shuvarie_config::ScenesConfig::default();
    scenes.scenes.insert(
        "Plan".into(),
        shuvarie_config::SceneConfig {
            description: Some("plan first".into()),
            system_prompts: shuvarie_config::SystemPromptsConfig {
                interlude: Some("we are in Plan mode".into()),
                ..Default::default()
            },
            ..shuvarie_config::SceneConfig::default()
        },
    );
    let mut store = Store::open_in_memory().await.unwrap();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        store.clone(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes,
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    // The `Event::StreamDone` of a turn precedes the run loop's internal
    // outcome handling by a beat; give it a moment so the next command is
    // not refused as busy.
    async fn drain_turn(event_rx: &mut tokio::sync::mpsc::Receiver<Event>) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
                .await
                .expect("timed out waiting for the turn to finish")
                .expect("core task ended");
            if matches!(ev, Event::StreamDone { .. }) {
                break;
            }
        }
    }
    async fn settle(event_rx: &mut tokio::sync::mpsc::Receiver<Event>) {
        drain_turn(event_rx).await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    fn chat_bodies(bodies: &[String]) -> Vec<&str> {
        bodies
            .iter()
            .filter(|b| b.contains("\"instructions\""))
            .map(|b| b.as_str())
            .collect()
    }

    // Turn 1: the session starts under no scene — a session does not
    // announce its own beginning, so no interlude rides the first request.
    cmd_tx
        .send(Command::SendMessage {
            content: "one".into(),
            model: None,
        })
        .await
        .unwrap();
    settle(&mut event_rx).await;

    // Mid-session switch to Plan: only the in-memory scene moves; the row
    // still holds the scene of the previous prompt.
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Plan".into()),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for SceneChanged")
            .expect("core task ended");
        match ev {
            Event::SceneChanged { name } => {
                assert_eq!(name.as_deref(), Some("Plan"));
                break;
            }
            Event::SceneError { error } => panic!("switch refused: {error}"),
            _ => {}
        }
    }
    let sid = store
        .most_recent_session()
        .await
        .unwrap()
        .expect("session row");
    assert_eq!(
        sid.scene, None,
        "the switch does not persist until the next prompt"
    );

    // Turn 2: the first request under Plan announces the switch — the
    // interlude rides it and the row now carries the scene.
    cmd_tx
        .send(Command::SendMessage {
            content: "two".into(),
            model: None,
        })
        .await
        .unwrap();
    settle(&mut event_rx).await;
    {
        let stored = store.load_session(sid.id).await.unwrap();
        assert_eq!(
            stored.scene.as_deref(),
            Some("Plan"),
            "the first prompt under the scene announces it"
        );
    }
    {
        let bodies = bodies.lock().unwrap();
        // The mock also serves the embedding requests that index each
        // message; only the chat-completions bodies (they carry the
        // `instructions` preamble) assert the interlude.
        let chat = chat_bodies(&bodies);
        assert_eq!(chat.len(), 2, "one chat request per turn");
        assert!(
            !chat[0].contains("we are in Plan mode"),
            "the session started under its scene: no interlude on request 1"
        );
        assert!(
            chat[1].contains("we are in Plan mode"),
            "the first request after the switch carries the interlude"
        );
    }

    // Turn 3: later requests run interlude-free.
    cmd_tx
        .send(Command::SendMessage {
            content: "three".into(),
            model: None,
        })
        .await
        .unwrap();
    settle(&mut event_rx).await;
    let bodies = bodies.lock().unwrap();
    let chat = chat_bodies(&bodies);
    assert_eq!(chat.len(), 3);
    assert!(
        !chat[2].contains("we are in Plan mode"),
        "the interlude is a one-shot announce, not a per-request wrap"
    );

    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn switch_scene_refuses_unknown_names() {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        Default::default(),
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::Ping).await.unwrap();
    recv_skills_loaded(&mut event_rx).await;
    let _ = event_rx.recv().await.expect("pong");

    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Ghost".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_error = false;
    for _ in 0..5 {
        match event_rx.recv().await {
            Some(Event::SceneError { error }) => {
                assert!(error.contains("unknown scene"), "{error}");
                saw_error = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_error, "SceneError reported");
    drop(cmd_tx);
    let _ = handle.await;
}

#[tokio::test]
async fn switch_scene_requires_interlude_mid_session() {
    let mut scenes = shuvarie_config::ScenesConfig::default();
    scenes.scenes.insert(
        "Draft".into(),
        shuvarie_config::SceneConfig {
            description: Some("no interlude".into()),
            ..shuvarie_config::SceneConfig::default()
        },
    );
    scenes.scenes.insert(
        "Plan".into(),
        shuvarie_config::SceneConfig {
            system_prompts: shuvarie_config::SystemPromptsConfig {
                interlude: Some("we are in Plan mode".into()),
                ..Default::default()
            },
            ..shuvarie_config::SceneConfig::default()
        },
    );
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(8);
    let handle = tokio::spawn(run(
        empty_config(),
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes,
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    cmd_tx.send(Command::Ping).await.unwrap();
    recv_skills_loaded(&mut event_rx).await;
    let _ = event_rx.recv().await.expect("pong");

    // A fresh session (no messages yet) may enter an interlude-less scene.
    cmd_tx.send(Command::NewSession).await.unwrap();
    loop {
        match event_rx.recv().await {
            Some(Event::SessionStarted) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Draft".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_changed = false;
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { name }) => {
                assert_eq!(name.as_deref(), Some("Draft"));
                saw_changed = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_changed, "a fresh session enters any scene");

    // With messages on the path, moving to the interlude-bearing scene
    // still works…
    cmd_tx
        .send(Command::SendMessage {
            content: "hello".into(),
            model: None,
        })
        .await
        .unwrap();
    for _ in 0..8 {
        match event_rx.recv().await {
            Some(Event::StreamError { .. }) => break,
            Some(_) => {}
            None => panic!("core task ended"),
        }
    }
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Plan".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SceneChanged { name }) => {
                assert_eq!(name.as_deref(), Some("Plan"));
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    // …and the interlude-less scene no longer does.
    cmd_tx
        .send(Command::SwitchScene {
            name: Some("Draft".into()),
        })
        .await
        .unwrap();
    cmd_tx.send(Command::Ping).await.unwrap();
    let mut saw_error = false;
    for _ in 0..6 {
        match event_rx.recv().await {
            Some(Event::SceneError { error }) => {
                assert!(error.contains("no interlude"), "{error}");
                assert!(error.contains("Draft"), "{error}");
                saw_error = true;
            }
            Some(Event::Pong) => break,
            Some(_) => {}
            None => panic!("core task stopped"),
        }
    }
    assert!(saw_error, "the interlude-less scene is refused mid-session");
    drop(cmd_tx);
    let _ = handle.await;
}

/// Drain events until the turn's `StreamDone`.
async fn drain_until_stream_done(event_rx: &mut tokio::sync::mpsc::Receiver<Event>) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for StreamDone")
            .expect("core task ended");
        if matches!(ev, Event::StreamDone { .. }) {
            return;
        }
    }
}

/// The active path root → tip of a stored session, walked from its leaf.
fn active_path_of(stored: &shuvarie_db::StoredSession) -> Vec<shuvarie_db::StoredMessage> {
    let by_id: std::collections::HashMap<u64, &shuvarie_db::StoredMessage> =
        stored.messages.iter().map(|m| (m.id, m)).collect();
    let mut ids = Vec::new();
    let mut cur = stored.leaf_id;
    while let Some(id) = cur {
        let msg = by_id.get(&id).expect("leaf resolves to a message");
        ids.push((*msg).clone());
        cur = msg.parent_id;
    }
    ids.reverse();
    ids
}

/// A manual `/compact` (with a focus instruction) runs the standard
/// compaction flow: the summary is spliced into the active path at the cut
/// point, the kept tail hangs under it, the leaf returns to the
/// pre-compaction tip so the compacted history stays on the active path, and
/// the summarizer request carries the instruction plus the summary format.
#[tokio::test]
async fn manual_compact_splices_the_summary_and_forwards_the_focus() {
    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let addr = spawn_mock_openai_recording("Compacted summary.", bodies.clone());
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });
    let store = Store::open_in_memory().await.unwrap();
    let mut store_handle = store.clone();

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        store,
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    // Three short turns: six messages, so the plan keeps the last four and
    // summarizes the first two.
    for prompt in ["first", "second", "third"] {
        cmd_tx
            .send(Command::SendMessage {
                content: prompt.into(),
                model: None,
            })
            .await
            .unwrap();
        drain_until_stream_done(&mut event_rx).await;
    }

    cmd_tx
        .send(Command::CompactSession {
            instruction: Some("Focus on the migration plan.".into()),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let (mut started, mut finished, mut compacted) = (false, false, None);
    while compacted.is_none() {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for SessionCompacted")
            .expect("core task ended");
        match ev {
            Event::CompactionStarted => started = true,
            Event::CompactionFinished => finished = true,
            Event::SessionCompacted { session } => compacted = Some(session),
            Event::SessionError { error } => panic!("compaction errored: {error}"),
            _ => {}
        }
    }
    assert!(started, "CompactionStarted brackets the summarizer");
    assert!(finished, "CompactionFinished closes the bracket");
    let session = compacted.expect("SessionCompacted carries the reloaded session");
    let sid = session.id.expect("compacted session has an id");

    let stored = store_handle.load_session(sid).await.unwrap();
    let summaries: Vec<_> = stored.messages.iter().filter(|m| m.summary).collect();
    assert_eq!(summaries.len(), 1, "exactly one summary was appended");
    assert_eq!(summaries[0].content, "Compacted summary.");
    let path = active_path_of(&stored);
    let summary_pos = path
        .iter()
        .position(|m| m.id == summaries[0].id)
        .expect("summary sits on the active path");
    // Head [u1, a1], summary, kept tail [u2, a2, u3, a3]: the tail's first
    // message hangs under the summary and the leaf is the old tip.
    assert_eq!(summary_pos, 2);
    assert_eq!(path.len(), 7, "summary + 6 original messages");
    assert_eq!(path[0].content, "first");
    assert_eq!(summaries[0].parent_id, Some(path[1].id));
    assert_eq!(path[3].content, "second");
    assert_eq!(path[3].parent_id, Some(summaries[0].id));
    assert_eq!(stored.leaf_id, Some(path[6].id), "leaf back at the old tip");
    assert_eq!(path[6].role, shuvarie_db::MsgRole::Assistant);

    // The summarizer request carried the focus instruction and the summary
    // format over a transcript of the summarized head.
    {
        let recorded = bodies.lock().expect("request sink poisoned");
        let last = recorded.last().expect("the compaction call was recorded");
        assert!(last.contains("Focus on the migration plan."), "{last}");
        assert!(last.contains("## Objective"), "{last}");
        assert!(last.contains("### User"), "{last}");
        assert!(!last.contains("third"), "the kept tail is not re-sent");
    }

    drop(cmd_tx);
    let _ = handle.await;
}

/// A mock registry server: serves `body` as JSON, recording each request's
/// head (path, headers) for assertions. Returns the base URL and the recorded
/// requests.
fn spawn_mock_registry(
    body: &'static str,
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let requests: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            use std::io::{Read, Write};
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).unwrap_or(0);
            sink.lock()
                .expect("request sink poisoned")
                .push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
            let response = format!(
                "HTTP/1.1 200 ok\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    (format!("http://{addr}/providers.json"), requests)
}

/// A config with one custom registry. Each test uses its own registry id:
/// the catalog is a process-global shared by the parallel tests, and ids are
/// merged by name.
fn registry_config(
    name: &str,
    mutate: impl FnOnce(&mut shuvarie_config::CustomRegistry),
) -> Config {
    let mut config = empty_config();
    let mut registry = shuvarie_config::CustomRegistry {
        name: name.into(),
        url: None,
        path: None,
        headers: Vec::new(),
        disabled: false,
        remote_first: false,
    };
    mutate(&mut registry);
    config.registries.custom.push(registry);
    config
}

async fn run_with_config(
    config: Config,
) -> (
    tokio::sync::mpsc::Sender<Command>,
    tokio::sync::mpsc::Receiver<Event>,
    tokio::task::JoinHandle<()>,
) {
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        config,
        empty_connections(),
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    (cmd_tx, event_rx, handle)
}

/// A remote-first custom registry is fetched once at startup, before any
/// event, so its snapshot is loaded by the time the first event arrives.
#[tokio::test]
async fn remote_first_custom_registry_fetches_at_startup() {
    let (url, requests) =
        spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme-remote\",\"models\":[]}]");
    let (cmd_tx, mut event_rx, handle) = run_with_config(registry_config("acme-rf", |registry| {
        registry.url = Some(url);
        registry.remote_first = true;
    }))
    .await;
    recv_skills_loaded(&mut event_rx).await;

    let snapshot = shuvarie_core::catalog::registry_catalog("acme-rf");
    assert_eq!(snapshot.id, "acme-rf");
    assert_eq!(
        snapshot.remote.map(|providers| providers.len()),
        Some(1),
        "the remote-first fetch ran before the startup events"
    );
    assert!(requests.lock().unwrap().len() >= 1, "one fetch");

    drop(cmd_tx);
    let _ = handle.await;
}

/// `Command::FetchRegistry` for a custom registry reports the loaded snapshot
/// tagged with the registry id.
#[tokio::test]
async fn fetch_registry_command_reports_the_configured_registry() {
    let (url, _requests) =
        spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme-remote\",\"models\":[]}]");
    let (cmd_tx, mut event_rx, handle) =
        run_with_config(registry_config("acme-fetch", |registry| {
            registry.url = Some(url);
        }))
        .await;
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::FetchRegistry {
            registry: "acme-fetch".into(),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
        .await
        .expect("timed out waiting for the registry event")
        .expect("core task ended");
    match ev {
        Event::RegistryLoaded {
            registry,
            providers,
        } => {
            assert_eq!(registry, "acme-fetch");
            assert_eq!(providers.len(), 1);
            assert_eq!(providers[0].id.0, "acme-remote");
        }
        other => panic!("expected RegistryLoaded, got {other:?}"),
    }

    drop(cmd_tx);
    let _ = handle.await;
}

/// A disabled registry is not registered at all: fetching its id is refused
/// as unknown, and no HTTP request is attempted.
#[tokio::test]
async fn fetch_registry_command_refuses_a_disabled_registry() {
    let (url, requests) =
        spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme-remote\",\"models\":[]}]");
    let (cmd_tx, mut event_rx, handle) =
        run_with_config(registry_config("acme-sleepy", |registry| {
            registry.url = Some(url);
            registry.disabled = true;
        }))
        .await;
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::FetchRegistry {
            registry: "acme-sleepy".into(),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
        .await
        .expect("timed out waiting for the registry event")
        .expect("core task ended");
    match ev {
        Event::RegistryError { registry, error } => {
            assert_eq!(registry, "acme-sleepy");
            assert!(error.contains("unknown registry"), "{error}");
        }
        other => panic!("expected RegistryError, got {other:?}"),
    }
    assert_eq!(requests.lock().unwrap().len(), 0, "no fetch was attempted");

    drop(cmd_tx);
    let _ = handle.await;
}

/// An unknown registry id is refused as unknown.
#[tokio::test]
async fn fetch_registry_command_refuses_an_unknown_registry() {
    let (cmd_tx, mut event_rx, handle) = run_with_config(empty_config()).await;
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::FetchRegistry {
            registry: "nowhere".into(),
        })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
        .await
        .expect("timed out waiting for the registry event")
        .expect("core task ended");
    match ev {
        Event::RegistryError { registry, error } => {
            assert_eq!(registry, "nowhere");
            assert!(error.contains("unknown registry"), "{error}");
        }
        other => panic!("expected RegistryError, got {other:?}"),
    }

    drop(cmd_tx);
    let _ = handle.await;
}

/// The built-in selune registry with `remote-first` fetches the hosted
/// catalog (routed through `CATALOG_URL`) at startup.
#[tokio::test]
async fn remote_first_selune_registry_fetches_the_hosted_catalog() {
    let (url, _requests) =
        spawn_mock_registry("[{\"name\":\"Acme\",\"id\":\"acme-remote\",\"models\":[]}]");
    unsafe { std::env::set_var("CATALOG_URL", &url) };
    let mut config = empty_config();
    config.registries.entries.insert(
        "selune".into(),
        shuvarie_config::RegistryEntry {
            disabled: false,
            remote_first: true,
        },
    );
    let (cmd_tx, mut event_rx, handle) = run_with_config(config).await;
    recv_skills_loaded(&mut event_rx).await;
    unsafe { std::env::remove_var("CATALOG_URL") };

    let snapshot =
        shuvarie_core::catalog::registry_catalog(shuvarie_core::catalog::SELUNE_REGISTRY);
    assert_eq!(
        snapshot.remote.map(|providers| providers.len()),
        Some(1),
        "the hosted catalog was fetched at startup"
    );

    drop(cmd_tx);
    let _ = handle.await;
}

/// With no summarizable span (at most the minimum tail), a manual compaction
/// reports `nothing to compact` instead of calling the summarizer.
#[tokio::test]
async fn manual_compact_without_enough_history_reports_nothing_to_compact() {
    let addr = spawn_mock_openai("reply");
    let mut connections = empty_connections();
    connections.providers.insert(
        "mock".into(),
        ProviderConfig::new(
            "mock",
            "openai-compat",
            Some("sk-test".into()),
            Some(format!("http://{addr}/v1")),
        ),
    );
    connections.active = Some(Active {
        provider: "mock".into(),
        model: Some("test-model".into()),
        variant: None,
    });

    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Event>(64);
    let handle = tokio::spawn(run(
        empty_config(),
        connections,
        Store::open_in_memory().await.unwrap(),
        StartupSession::None,
        None,
        None,
        permissions_for_tests(),
        TrustGrants::all(),
        shuvarie_core::SceneSet {
            scenes: Default::default(),
            warnings: Vec::new(),
        },
        cmd_rx,
        event_tx,
    ));
    recv_skills_loaded(&mut event_rx).await;

    cmd_tx
        .send(Command::SendMessage {
            content: "hello".into(),
            model: None,
        })
        .await
        .unwrap();
    drain_until_stream_done(&mut event_rx).await;

    cmd_tx
        .send(Command::CompactSession { instruction: None })
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let ev = tokio::time::timeout(deadline - tokio::time::Instant::now(), event_rx.recv())
            .await
            .expect("timed out waiting for the refusal")
            .expect("core task ended");
        match ev {
            Event::SessionError { error } => {
                assert!(error.contains("nothing to compact"), "{error}");
                break;
            }
            Event::CompactionStarted | Event::CompactionFinished => {}
            _ => {}
        }
    }

    drop(cmd_tx);
    let _ = handle.await;
}
