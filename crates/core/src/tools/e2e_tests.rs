//! Phase-6 end-to-end: the full chain from config-file text to a tool
//! result, exactly as a turn drives it — KDL text →
//! [`shuvarie_config::Config`] → spec conversion → live [`McpManager`] →
//! [`all_tools`] roster → rig's canonical [`ToolSet::execute`] dispatch, with
//! the permission gate answering in the loop.
//!
//! Three levels of transport realism:
//! 1. the in-process duplex server ([`shuvarie_mcp::testkit`]) — the full
//!    dispatch path with no subprocess;
//! 2. a stdio tool spawning a real `printf` subprocess;
//! 3. a real MCP server child process (the `mcp-test-server` example binary),
//!    skipped when that binary is absent (unusual target layout).

use std::collections::BTreeMap;

use serde_json::json;
use shuvarie_config::PermissionsConfig;
use shuvarie_llm::{ToolResult, ToolSet};
use shuvarie_mcp::testkit;

use super::all_tools;
use crate::Skills;
use crate::permissions::{AskScope, PermissionAnswer};
use crate::question::QuestionGate;
use crate::scenes::ToolScene;
use crate::test_util::{access_with_answering_gate, new_ctx};
use crate::tools::{FileLocks, ReadCache, ShellOutputTx, todos};

/// Parses config text through the public file path (the module is private in
/// `shuvarie_config`), like a real config file on disk.
fn config_from_kdl(text: &str) -> shuvarie_config::Config {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("shuvarie.kdl");
    std::fs::write(&path, text).expect("write config");
    shuvarie_config::Config::load_from(&path).expect("parse config")
}

/// The roster for one turn as the core task builds it, wrapped in rig's
/// canonical dispatch surface.
fn roster(
    config: &shuvarie_config::Config,
    access: crate::permissions::Access,
    mcp: Option<&shuvarie_mcp::SharedMcpManager>,
    mcp_roster: &BTreeMap<String, shuvarie_mcp::McpToolMap>,
) -> ToolSet {
    let (question_tx, _question_rx) = tokio::sync::mpsc::channel(1);
    let (shell_tx, _shell_rx) = tokio::sync::mpsc::channel(1);
    let lsp = std::sync::Arc::new(tokio::sync::Mutex::new(shuvarie_lsp::LspManager::new(
        std::path::PathBuf::from("."),
        false,
        Default::default(),
    )));
    let tools = all_tools(
        lsp,
        FileLocks::new(),
        ReadCache::new(),
        100,
        100,
        QuestionGate::new(question_tx),
        access,
        ShellOutputTx::new(shell_tx),
        crate::shell::resolve(None).shell,
        todos::TodoState::from_records(&[]),
        &ToolScene::default(),
        config.tools.web_search.as_ref(),
        &Skills::default(),
        &config.tools.tools,
        mcp,
        mcp_roster,
    );
    ToolSet::from_dynamic_tools(tools)
}

fn definition(set: &ToolSet, name: &str) -> shuvarie_llm::ToolDefinition {
    set.get_tool_definitions()
        .into_iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no definition for {name}"))
}

/// The model-visible text of a dispatch result.
fn text(result: &ToolResult) -> &str {
    result
        .output()
        .as_text()
        .expect("result carries model-visible text")
}

/// The config used by the dispatch tests: one stdio tool and one MCP server,
/// the mixed roster a real config produces. The stdio server command is never
/// spawned in the duplex test (the injected connection bypasses it); it only
/// proves the spec came from the file.
const MIXED_KDL: &str = r#"
    tools {
      tool name="greet" {
        description "Greet someone by name"
        cmd "printf" "%s\n" "hello {{msg}}"
        timeout 30
        params {
          param "msg" type="string" required=#true description="Who to greet"
        }
      }
      mcp {
        stdio name="tester" {
          command "shuvarie-e2e-never-spawned"
        }
      }
    }
"#;

#[tokio::test]
async fn kdl_config_reaches_a_live_mcp_tool_through_the_canonical_dispatch() {
    let config = config_from_kdl(MIXED_KDL);
    assert!(config.tools.tools.contains_key("greet"));
    assert!(config.tools.mcp.stdio.contains_key("tester"));

    // The real constructor core uses, plus the in-process server standing in
    // for the transport the stdio spec would spawn.
    let mcp = crate::mcp_manager::mcp_manager(&config.tools.mcp);
    let (roster_map, _server) = {
        let mut manager = mcp.lock().await;
        assert!(
            manager.specs().contains_key("tester"),
            "specs came from the config"
        );
        let (conn, server) = testkit::duplex_connection("tester").await;
        manager.inject_connection("tester", conn);
        // No-op over the injected connection; a turn connects before the
        // roster is read.
        manager.ensure_connected("tester").await.expect("connected");
        (manager.roster(), server)
    };

    let (access, mut requests) = access_with_answering_gate(&PermissionsConfig::builtin());
    let set = roster(&config, access, Some(&mcp), &roster_map);
    assert!(set.contains("greet"));
    assert!(set.contains("mcp__tester__echo"));

    // Definitions flow from the config and the server's `tools/list`.
    let greet = definition(&set, "greet");
    assert_eq!(greet.description, "Greet someone by name");
    assert!(
        greet.parameters["properties"]["msg"].is_object(),
        "params: {}",
        greet.parameters
    );
    let echo = definition(&set, "mcp__tester__echo");
    assert_eq!(echo.description, "Echo a message");
    assert!(
        echo.parameters["properties"]["message"].is_object(),
        "params: {}",
        echo.parameters
    );

    // First call asks; the answerer must reply while the call is parked.
    let mut ctx = new_ctx();
    let (result, _) = tokio::join!(
        set.execute(
            "mcp__tester__echo",
            json!({"message": "asked"}).to_string(),
            &mut ctx
        ),
        async {
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::task::yield_now().await;
                requests.recv().await
            })
            .await
            .expect("no permission request within 5s")
            .expect("permission request");
            assert!(
                request
                    .description
                    .contains("Allow tool `mcp__tester__echo`?"),
                "got: {}",
                request.description
            );
            assert_eq!(
                request.scope,
                Some(AskScope::Tool("mcp__tester__echo".to_string()))
            );
            request
                .respond
                .send(PermissionAnswer::AllowSession)
                .expect("answer sent");
        }
    );
    assert!(result.error().is_none(), "result: {result:?}");
    assert_eq!(text(&result), "echo: asked");

    // The session grant covers the composite name: no further ask.
    assert!(requests.try_recv().is_err());
    let mut ctx = new_ctx();
    let result = set
        .execute(
            "mcp__tester__echo",
            json!({"message": "granted"}).to_string(),
            &mut ctx,
        )
        .await;
    assert!(requests.try_recv().is_err(), "grant covers later calls");
    assert_eq!(text(&result), "echo: granted");

    // The server-side `isError` tool surfaces as a structured dispatch error.
    // A distinct tool name asks again; answer while the call is parked.
    let mut ctx = new_ctx();
    let (result, _) = tokio::join!(set.execute("mcp__tester__fail", "{}", &mut ctx), async {
        let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::task::yield_now().await;
            requests.recv().await
        })
        .await
        .expect("no permission request within 5s")
        .expect("permission request");
        assert_eq!(
            request.scope,
            Some(AskScope::Tool("mcp__tester__fail".to_string()))
        );
        request
            .respond
            .send(PermissionAnswer::AllowSession)
            .expect("answer sent");
    });
    assert!(result.error().is_some(), "result: {result:?}");
    assert!(text(&result).contains("boom"), "text: {}", text(&result));
}

#[cfg(unix)]
#[tokio::test]
async fn kdl_config_spawns_a_real_subprocess_through_the_canonical_dispatch() {
    let config = config_from_kdl(MIXED_KDL);
    let (access, mut requests) = access_with_answering_gate(&PermissionsConfig::builtin());
    let set = roster(&config, access, None, &Default::default());

    // First call asks, is granted, and runs the real `printf`.
    let mut ctx = new_ctx();
    let (result, _) = tokio::join!(
        set.execute("greet", json!({"msg": "e2e"}).to_string(), &mut ctx),
        async {
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::task::yield_now().await;
                requests.recv().await
            })
            .await
            .expect("no permission request within 5s")
            .expect("permission request");
            assert_eq!(request.scope, Some(AskScope::Tool("greet".to_string())));
            request
                .respond
                .send(PermissionAnswer::AllowSession)
                .expect("answer sent");
        }
    );
    assert!(
        result.error().is_none(),
        "result: {result:?} output: {:?}",
        result.output().as_text()
    );
    assert_eq!(text(&result), "hello e2e\n");

    // The grant covers the name: no further ask.
    let mut ctx = new_ctx();
    let result = set
        .execute("greet", json!({"msg": "again"}).to_string(), &mut ctx)
        .await;
    assert!(requests.try_recv().is_err(), "grant covers later calls");
    assert_eq!(text(&result), "hello again\n");

    // Arguments the tool did not declare are rejected through the dispatch.
    let mut ctx = new_ctx();
    let result = set
        .execute(
            "greet",
            json!({"msg": "x", "extra": 1}).to_string(),
            &mut ctx,
        )
        .await;
    assert!(result.error().is_some(), "result: {result:?}");
}

/// The example binary built by `cargo test --workspace`; a missing path
/// (unusual profile or target layout) skips the test instead of failing it.
fn example_binary() -> Option<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/debug/examples/mcp-test-server"
    );
    std::path::Path::new(path)
        .exists()
        .then(|| path.to_string())
}

#[cfg(unix)]
#[tokio::test]
async fn kdl_config_spawns_a_real_mcp_child_process_end_to_end() {
    let Some(bin) = example_binary() else {
        eprintln!("skipping: mcp-test-server example binary not found");
        return;
    };
    let kdl = format!(
        r#"
        tools {{
          mcp {{
            stdio name="tester" {{
              command "{bin}"
            }}
          }}
        }}
    "#
    );
    let config = config_from_kdl(&kdl);

    // The manager spawns the child process from the file's spec alone.
    let mcp = crate::mcp_manager::mcp_manager(&config.tools.mcp);
    let (roster_map, status) = {
        let mut manager = mcp.lock().await;
        manager
            .ensure_connected("tester")
            .await
            .expect("spawn server");
        let snapshot = manager.status_snapshot();
        (manager.roster(), snapshot)
    };
    assert!(mcp.lock().await.connected("tester"));
    assert_eq!(
        status.first().map(|s| (s.name.as_str(), s.state)),
        Some(("tester", shuvarie_mcp::McpStatusState::Connected))
    );

    let (access, mut requests) = access_with_answering_gate(&PermissionsConfig::builtin());
    let set = roster(&config, access, Some(&mcp), &roster_map);
    assert!(set.contains("mcp__tester__echo"));

    let mut ctx = new_ctx();
    let (result, _) = tokio::join!(
        set.execute(
            "mcp__tester__echo",
            json!({"message": "hello child process"}).to_string(),
            &mut ctx,
        ),
        async {
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::task::yield_now().await;
                requests.recv().await
            })
            .await
            .expect("no permission request within 5s")
            .expect("permission request");
            assert_eq!(
                request.scope,
                Some(AskScope::Tool("mcp__tester__echo".to_string()))
            );
            request
                .respond
                .send(PermissionAnswer::AllowSession)
                .expect("answer sent");
        }
    );
    assert!(result.error().is_none(), "result: {result:?}");
    assert_eq!(text(&result), "echo: hello child process");
}
