//! In-process end-to-end tests over a `tokio::io::duplex` pair: a real rmcp
//! server (the testkit double) on one half, the real client connection on the
//! other — the full JSON-RPC codec, `tools/list`, and `tools/call` path
//! without spawning processes.

use std::collections::BTreeMap;

use serde_json::json;

use crate::connection::McpConnection;
use crate::manager::McpManager;
use crate::testkit::{McpServerSpec, duplex_connection};
use crate::types::{McpStatusState, composite_tool_name};

/// A connected client over one duplex pair with a fresh test-server task.
async fn connect_duplex(server_name: &str) -> (McpConnection, tokio::task::JoinHandle<()>) {
    duplex_connection(server_name).await
}

#[tokio::test]
async fn duplex_connect_lists_and_calls() {
    let (conn, server) = connect_duplex("test").await;
    let names: Vec<&str> = conn.tools().iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "tools: {names:?}");
    assert!(names.contains(&"fail"));

    let echo = conn
        .call_tool(
            "echo",
            Some(json!({"message": "hi"}).as_object().unwrap().clone()),
            std::time::Duration::from_secs(10),
        )
        .await
        .expect("echo call");
    assert!(!echo.is_error);
    assert_eq!(echo.text, "echo: hi");

    let failed = conn
        .call_tool("fail", None, std::time::Duration::from_secs(10))
        .await
        .expect("fail call (transport-wise)");
    assert!(failed.is_error, "isError surfaces as an error output");
    assert_eq!(failed.text, "boom");

    drop(server);
}

#[tokio::test]
async fn duplex_unknown_tool_is_a_protocol_error() {
    let (conn, server) = connect_duplex("test").await;
    let err = conn
        .call_tool("nope", None, std::time::Duration::from_secs(10))
        .await
        .expect_err("unknown tool must fail");
    assert!(err.contains("server error"), "got: {err}");
    drop(server);
}

#[tokio::test]
async fn duplex_tool_descriptors_carry_schema_and_description() {
    let (conn, server) = connect_duplex("test").await;
    let echo = conn
        .tools()
        .iter()
        .find(|t| t.name == "echo")
        .expect("echo");
    assert_eq!(echo.description.as_deref(), Some("Echo a message"));
    let schema = echo.input_schema.clone().expect("input schema");
    assert_eq!(schema["type"], json!("object"));
    assert_eq!(schema["properties"]["message"]["type"], json!("string"));
    drop(server);
}

#[tokio::test]
async fn duplex_closed_is_detected_after_server_goes_away() {
    let (conn, server) = connect_duplex("test").await;
    assert!(!conn.closed());
    // Aborting the server task drops its transport half; the client's
    // transport must notice and report closed.
    server.abort();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !conn.closed() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "connection should be detected as closed");
}

#[tokio::test]
async fn manager_connects_lists_and_calls_through_an_injected_connection() {
    let (conn, server) = connect_duplex("test").await;
    let mut manager = McpManager::new(BTreeMap::from([(
        "test".to_string(),
        McpServerSpec::Http {
            url: "https://unused.invalid/mcp".to_string(),
            headers: BTreeMap::new(),
        },
    )]));
    manager.inject_connection("test", conn);

    assert!(manager.connected("test"));
    let map = manager.tool_map("test");
    assert!(
        map.contains_key(composite_tool_name("test", "echo").as_str()),
        "map keys: {:?}",
        map.keys()
    );

    let out = manager
        .call_tool(
            "test",
            "echo",
            Some(
                json!({"message": "through-manager"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
            Some(std::time::Duration::from_secs(10)),
        )
        .await
        .expect("manager echo");
    assert_eq!(out.text, "echo: through-manager");

    let statuses = manager.status_snapshot();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].state, McpStatusState::Connected);
    assert_eq!(statuses[0].transport, "http");
    assert_eq!(statuses[0].tools, 2);

    manager.disconnect("test");
    assert_eq!(
        manager.status_snapshot()[0].state,
        McpStatusState::Configured
    );
    drop(server);
}

#[tokio::test]
async fn manager_call_on_unknown_server_fails() {
    let mut manager = McpManager::new(BTreeMap::new());
    let err = manager
        .call_tool(
            "ghost",
            "echo",
            None,
            Some(std::time::Duration::from_secs(1)),
        )
        .await
        .expect_err("unknown server");
    assert!(err.contains("no MCP server configured"), "got: {err}");
}

#[tokio::test]
async fn manager_status_reflects_configured_and_failed_servers() {
    let mut manager = McpManager::new(BTreeMap::from([
        (
            "http-server".to_string(),
            McpServerSpec::Http {
                url: "https://unused.invalid/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        ),
        (
            "bad-stdio".to_string(),
            McpServerSpec::Stdio {
                command: "definitely-not-a-real-binary-12345".to_string(),
                args: Vec::new(),
                envs: Default::default(),
            },
        ),
    ]));
    // Both configured, neither connected, no failed attempts yet.
    for status in manager.status_snapshot() {
        assert_eq!(status.state, McpStatusState::Configured, "{status:?}");
    }
    // A failed connect marks the server Failed with a reason.
    assert!(manager.ensure_connected("bad-stdio").await.is_err());
    let statuses = manager.status_snapshot();
    let bad = statuses
        .iter()
        .find(|s| s.name == "bad-stdio")
        .expect("bad");
    assert_eq!(bad.state, McpStatusState::Failed);
    assert!(bad.error.is_some(), "failure reason recorded");
    // The http server is untouched.
    let http = statuses
        .iter()
        .find(|s| s.name == "http-server")
        .expect("http-server");
    assert_eq!(http.state, McpStatusState::Configured);
    // `has` and `specs` bookkeeping.
    assert!(manager.has("http-server"));
    assert!(!manager.has("zz"));
    assert_eq!(manager.specs().len(), 2);
    assert_eq!(manager.connected_servers(), Vec::<String>::new());
}

#[tokio::test]
async fn tool_map_uses_composite_names() {
    let (conn, server) = connect_duplex("test").await;
    let mut manager = McpManager::new(BTreeMap::new());
    manager.inject_connection("test", conn);
    let map = manager.tool_map("test");
    assert!(map.contains_key("mcp__test__echo"));
    assert!(map.contains_key("mcp__test__fail"));
    drop(server);
}
