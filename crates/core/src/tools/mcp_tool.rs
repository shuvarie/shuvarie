//! The MCP tool bridge: one configured MCP server tool, surfaced to the agent
//! as `mcp__<server>__<tool>`. Calls go through the shared
//! [`shuvarie_mcp::McpManager`], which connects (or reconnects) lazily, so a
//! tool call may pay the connection cost once per server.

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};
use shuvarie_mcp::{McpToolInfo, SharedMcpManager};

use crate::permissions::Access;
use crate::truncate::truncate_output;

/// How much of an MCP tool's arguments are echoed into the ask prompt.
const ARG_DETAIL_CHARS: usize = 512;

/// One tool of one configured MCP server.
pub(crate) struct McpTool {
    manager: SharedMcpManager,
    server: String,
    tool: String,
    composite: String,
    info: McpToolInfo,
    max_output_chars: usize,
    access: Access,
}

impl McpTool {
    pub(crate) fn new(
        manager: SharedMcpManager,
        server: String,
        info: McpToolInfo,
        max_output_chars: usize,
        access: Access,
    ) -> Self {
        let composite = shuvarie_mcp::composite_tool_name(&server, &info.name);
        Self {
            manager,
            server,
            tool: info.name.clone(),
            composite,
            info,
            max_output_chars,
            access,
        }
    }
}

impl Tool for McpTool {
    /// Not the agent-facing name — registration passes the composite name
    /// (`mcp__<server>__<tool>`) to `into_dynamic` explicitly; the trait name
    /// is never consulted on the dynamic-tool path.
    const NAME: &'static str = "mcp_tool";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.info.description.clone().unwrap_or_else(|| {
            format!(
                "Call the `{tool}` tool on the `{server}` MCP server.",
                tool = self.tool,
                server = self.server,
            )
        })
    }

    fn parameters(&self) -> Value {
        match &self.info.input_schema {
            Some(schema) if schema.is_object() => schema.clone(),
            _ => json!({"type": "object", "properties": {}}),
        }
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let composite = self.composite.clone();
        let max_output_chars = self.max_output_chars;
        let result: Result<ToolOutput, String> = async move {
            let arguments = args
                .as_object()
                .ok_or_else(|| "arguments must be a JSON object".to_string())?
                .clone();
            let detail = format!(
                "Server `{server}`, tool `{tool}`. Arguments: {arguments}",
                server = self.server,
                tool = self.tool,
                arguments = summarize(&serde_json::to_string(&arguments).unwrap_or_default()),
            );
            self.access.authorize_tool(&composite, &detail).await?;

            let output = {
                let mut manager = self.manager.lock().await;
                manager
                    .call_tool(&self.server, &self.tool, Some(arguments), None)
                    .await?
            };
            if output.is_error {
                return Err(format!(
                    "MCP tool `{composite}` reported failure: {}",
                    summarize(&output.text)
                ));
            }
            let text = match truncate_output(
                &output.text,
                max_output_chars,
                "increase max_output_chars",
            ) {
                Some(truncated) => truncated,
                None => output.text,
            };
            Ok(ToolOutput::text(text))
        }
        .await;
        result.map_err(ToolExecutionError::other)
    }
}

/// Truncates a human-facing summary at a sane length.
fn summarize(text: &str) -> String {
    let mut text = text.trim().to_string();
    if text.chars().count() > ARG_DETAIL_CHARS {
        text = text.chars().take(ARG_DETAIL_CHARS).collect::<String>() + "…";
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;
    use shuvarie_llm::Tool;
    use shuvarie_mcp::testkit::duplex_connection;
    use shuvarie_mcp::{McpManager, McpServerSpec};
    use tokio::task::JoinHandle;

    use crate::permissions::{AskScope, PermissionAnswer};
    use crate::test_util::{access_for_config, access_with_answering_gate, new_ctx};

    use super::*;

    /// The `echo` descriptor of the testkit server, bridged into a tool over
    /// an injected duplex connection. The returned handle is the server task.
    async fn bridged_echo(access: Access) -> (McpTool, JoinHandle<()>) {
        let (conn, server) = duplex_connection("test").await;
        let info = conn
            .tools()
            .iter()
            .find(|tool| tool.name == "echo")
            .cloned()
            .expect("echo descriptor");
        let mut manager = McpManager::new(BTreeMap::from([(
            "test".to_string(),
            McpServerSpec::Http {
                url: "https://unused.invalid/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        )]));
        manager.inject_connection("test", conn);
        let manager: SharedMcpManager = Arc::new(tokio::sync::Mutex::new(manager));
        (
            McpTool::new(manager, "test".to_string(), info, 4096, access),
            server,
        )
    }

    /// The call path skips the ask: the top-level default verb allows, so
    /// the call tests below only exercise the call itself.
    fn allow_all_access() -> Access {
        access_for_config(&shuvarie_config::PermissionsConfig {
            default: Some(shuvarie_config::Verb::Allow),
            ..shuvarie_config::PermissionsConfig::builtin()
        })
    }

    #[test]
    fn descriptor_flows_into_schema_description_and_name() {
        let info = McpToolInfo {
            name: "echo".to_string(),
            title: Some("Echo".to_string()),
            description: Some("Echo a message".to_string()),
            input_schema: Some(
                json!({"type": "object", "properties": {"message": {"type": "string"}}}),
            ),
        };
        let manager: SharedMcpManager =
            Arc::new(tokio::sync::Mutex::new(McpManager::new(BTreeMap::new())));
        let tool = McpTool::new(manager, "test".to_string(), info, 4096, allow_all_access());
        assert_eq!(tool.composite, "mcp__test__echo");
        assert_eq!(tool.description(), "Echo a message");
        assert_eq!(
            tool.parameters(),
            json!({"type": "object", "properties": {"message": {"type": "string"}}})
        );
    }

    #[test]
    fn missing_schema_and_description_fall_back() {
        let info = McpToolInfo {
            name: "bare".to_string(),
            title: None,
            description: None,
            input_schema: None,
        };
        let manager: SharedMcpManager =
            Arc::new(tokio::sync::Mutex::new(McpManager::new(BTreeMap::new())));
        let tool = McpTool::new(manager, "srv".to_string(), info, 4096, allow_all_access());
        assert_eq!(tool.composite, "mcp__srv__bare");
        assert_eq!(
            tool.description(),
            "Call the `bare` tool on the `srv` MCP server."
        );
        assert_eq!(
            tool.parameters(),
            json!({"type": "object", "properties": {}})
        );

        // A non-object schema is also dropped in favor of the empty object.
        let info = McpToolInfo {
            name: "weird".to_string(),
            title: None,
            description: None,
            input_schema: Some(json!("not-an-object")),
        };
        let manager: SharedMcpManager =
            Arc::new(tokio::sync::Mutex::new(McpManager::new(BTreeMap::new())));
        let tool = McpTool::new(manager, "srv".to_string(), info, 4096, allow_all_access());
        assert_eq!(
            tool.parameters(),
            json!({"type": "object", "properties": {}})
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn call_round_trips_text_through_the_manager() {
        let (tool, server) = bridged_echo(allow_all_access()).await;
        let mut ctx = new_ctx();
        let out = tool
            .call(&mut ctx, json!({"message": "hi"}))
            .await
            .expect("echo call");
        assert_eq!(out.as_text().expect("text"), "echo: hi");
        drop(server);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_level_failure_is_a_tool_execution_error() {
        let (conn, server) = duplex_connection("test").await;
        let info = conn
            .tools()
            .iter()
            .find(|tool| tool.name == "fail")
            .cloned()
            .expect("fail descriptor");
        let mut manager = McpManager::new(BTreeMap::from([(
            "test".to_string(),
            McpServerSpec::Http {
                url: "https://unused.invalid/mcp".to_string(),
                headers: BTreeMap::new(),
            },
        )]));
        manager.inject_connection("test", conn);
        let manager: SharedMcpManager = Arc::new(tokio::sync::Mutex::new(manager));
        let tool = McpTool::new(manager, "test".to_string(), info, 4096, allow_all_access());
        let mut ctx = new_ctx();
        let err = tool
            .call(&mut ctx, json!({}))
            .await
            .expect_err("isError must fail the call");
        let message = err.to_string();
        assert!(
            message.contains("mcp__test__fail") && message.contains("boom"),
            "got: {message}"
        );
        drop(server);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn call_asks_with_composite_name_and_session_grant_covers_it() {
        let (access, mut requests) =
            access_with_answering_gate(&shuvarie_config::PermissionsConfig::builtin());
        let (tool, server) = bridged_echo(access).await;
        // The call pauses on the ask; the answerer future must send the
        // verdict while `call` is still parked on it.
        let mut ctx = new_ctx();
        let (result, _) = tokio::join!(tool.call(&mut ctx, json!({"message": "asked"})), async {
            let request = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                // The manager lock must not be held while the gate waits,
                // or the call could deadlock; the request arrives here.
                tokio::task::yield_now().await;
                requests.recv().await
            })
            .await
            .expect("no permission request within 5s")
            .expect("permission request");
            assert!(
                request
                    .description
                    .contains("Allow tool `mcp__test__echo`?"),
                "got: {}",
                request.description
            );
            assert_eq!(
                request.scope,
                Some(AskScope::Tool("mcp__test__echo".to_string()))
            );
            request
                .respond
                .send(PermissionAnswer::AllowSession)
                .expect("answer sent");
        });
        result.expect("allowed");

        // The grant covers the composite name: the next call skips the ask.
        assert!(requests.try_recv().is_err(), "no new ask after the grant");
        let mut ctx = new_ctx();
        tool.call(&mut ctx, json!({"message": "granted"}))
            .await
            .expect("covered by the grant");
        assert!(requests.try_recv().is_err(), "still no ask");
        drop(server);
    }
}
