//! In-memory MCP test doubles for dependent crates (behind the `test-util`
//! feature): a real rmcp server served over one half of a
//! [`tokio::io::duplex`] pair and helpers to hand the matching client
//! connection to a [`McpManager`]. The full JSON-RPC codec, `tools/list`, and
//! `tools/call` path run without spawning any process.
//!
//! The server exposes two tools:
//!
//! - `echo` — returns `echo: <message>` from the `message` argument.
//! - `fail` — always completes with `isError` set and text `boom`.

use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::model::{
    CallToolResponse, ContentBlock, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

pub use crate::config::McpServerSpec;
pub use crate::connection::McpConnection;
use crate::manager::{McpManager, SharedMcpManager};

/// The in-process test server: `echo` (text round-trip) and `fail`
/// (tool-level error).
pub struct TestServer;

impl ServerHandler for TestServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("shuvarie-mcp test server")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, McpError> {
        let mut echo = tool("echo", "Echo a message");
        echo.input_schema = Arc::new(
            json!({
                "type": "object",
                "properties": {"message": {"type": "string"}},
                "required": ["message"]
            })
            .as_object()
            .expect("object")
            .clone(),
        );
        Ok(rmcp::model::ListToolsResult {
            tools: vec![echo, tool("fail", "Always fails")],
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        match request.name.as_ref() {
            "echo" => {
                let message = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Ok(CallToolResponse::Complete(
                    rmcp::model::CallToolResult::success(vec![ContentBlock::text(format!(
                        "echo: {message}"
                    ))]),
                ))
            }
            "fail" => Ok(CallToolResponse::Complete(
                rmcp::model::CallToolResult::error(vec![ContentBlock::text("boom")]),
            )),
            other => Err(McpError::invalid_params(
                format!("unknown tool '{other}'"),
                None,
            )),
        }
    }
}

fn tool(name: &str, description: &str) -> Tool {
    let mut t = Tool::default();
    t.name = name.to_owned().into();
    t.description = Some(description.to_owned().into());
    t
}

/// A connected client over one duplex pair with a fresh [`TestServer`] task.
/// The returned handle is the server task: keep it alive (dropping it
/// eventually tears the service loop down) or `abort()` it to simulate a
/// crashed server.
pub async fn duplex_connection(server_name: &str) -> (McpConnection, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        // The returned `RunningService` must be kept alive: dropping it
        // cancels the service loop (which is why the handshake's successor
        // messages hit a broken pipe).
        match TestServer.serve(server_io).await {
            Ok(service) => {
                let _ = service.waiting().await;
            }
            Err(e) => panic!("test server failed to serve: {e}"),
        }
    });
    let conn = McpConnection::serve(server_name, client_io, None)
        .await
        .expect("connect over duplex");
    (conn, server)
}

/// A [`McpManager`] sharing one duplex connection, wrapped the way core keeps
/// it (`Arc<tokio::sync::Mutex<..>>`).
pub async fn shared_manager(server_name: &str) -> (SharedMcpManager, JoinHandle<()>) {
    let (conn, server) = duplex_connection(server_name).await;
    let mut manager = McpManager::new(BTreeMap::new());
    manager.inject_connection(server_name, conn);
    (Arc::new(tokio::sync::Mutex::new(manager)), server)
}
