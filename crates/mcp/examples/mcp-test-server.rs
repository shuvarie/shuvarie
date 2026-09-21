//! A tiny MCP stdio server used by `tests/stdio.rs` to exercise the real
//! child-process transport end to end. Exposes `echo` and `fail`.
//!
//! Run standalone: `cargo run -p shuvarie-mcp --example mcp-test-server`.

use std::sync::Arc;

use rmcp::model::{
    CallToolResponse, ContentBlock, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};

struct TestServer;

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
        let mut echo = Tool::default();
        echo.name = "echo".to_owned().into();
        echo.description = Some("Echo a message".to_owned().into());
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
        let mut fail = Tool::default();
        fail.name = "fail".to_owned().into();
        fail.description = Some("Always fails".to_owned().into());
        Ok(rmcp::model::ListToolsResult {
            tools: vec![echo, fail],
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

#[tokio::main]
async fn main() {
    let service = TestServer
        .serve((tokio::io::stdin(), tokio::io::stdout()))
        .await
        .expect("test server failed to start");
    // Keep the service alive until the client disconnects; the returned
    // quit reason is irrelevant here.
    let _ = service.waiting().await;
}
