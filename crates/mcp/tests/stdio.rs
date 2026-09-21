//! End-to-end test through the real child-process transport: the manager
//! spawns the `mcp-test-server` example binary (built by `cargo test`),
//! lists its tools, calls one, and disconnects.

use std::collections::BTreeMap;
use std::path::Path;

use shuvarie_mcp::{EnvSpec, McpManager, McpServerSpec};

/// The built example binary; `cargo test` compiles examples, but a missing
/// path (unusual profile or target layout) skips the test instead of
/// failing it.
fn example_binary() -> Option<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/debug/examples/mcp-test-server"
    );
    Path::new(path).exists().then(|| path.to_string())
}

#[tokio::test]
async fn stdio_end_to_end_through_child_process() {
    let Some(bin) = example_binary() else {
        eprintln!("skipping: mcp-test-server example binary not found");
        return;
    };
    let mut manager = McpManager::new(BTreeMap::from([(
        "test".to_string(),
        McpServerSpec::Stdio {
            command: bin,
            args: Vec::new(),
            envs: EnvSpec::default(),
        },
    )]));

    manager.ensure_connected("test").await.expect("connect");
    assert!(manager.connected("test"));

    let tools = manager.cached_tools("test").expect("cached tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"echo"), "tools: {names:?}");
    assert!(names.contains(&"fail"));

    let out = manager
        .call_tool(
            "test",
            "echo",
            Some(
                serde_json::json!({"message": "hello child process"})
                    .as_object()
                    .expect("object")
                    .clone(),
            ),
            Some(std::time::Duration::from_secs(10)),
        )
        .await
        .expect("echo call");
    assert!(!out.is_error);
    assert_eq!(out.text, "echo: hello child process");

    let failed = manager
        .call_tool(
            "test",
            "fail",
            None,
            Some(std::time::Duration::from_secs(10)),
        )
        .await
        .expect("fail call");
    assert!(failed.is_error);
    assert_eq!(failed.text, "boom");

    manager.disconnect("test");
    assert!(!manager.connected("test"));
}
