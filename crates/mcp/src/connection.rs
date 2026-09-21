//! One live MCP client connection: the rmcp `RunningService` plus the cached
//! tool list and diagnostics for a single configured server.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, ContentBlock, Implementation,
    JsonObject,
};
use rmcp::service::{
    ClientLifecycleMode, ClientServiceExt, RoleClient, RunningService,
    RunningServiceCancellationToken, ServiceError,
};
use rmcp::transport::{IntoTransport, TokioChildProcess, which_command};
use serde_json::Value;
use tokio::process::Command;

use crate::config::{EnvSpec, McpServerSpec};
use crate::types::{McpToolInfo, McpToolOutput};

/// The upper bound for the whole connect flow: handshake + `tools/list`.
/// Stdio servers started through `npx` may download packages on first use,
/// so this is deliberately generous.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(60);

/// How many trailing stderr lines are kept from a stdio server for error
/// messages.
const STDERR_TAIL_LINES: usize = 16;

type StderrTail = Arc<Mutex<VecDeque<String>>>;

/// A live connection to one MCP server.
///
/// The rmcp service runs in its own task; this handle talks to it through
/// `&self` calls and holds a cancellation token so shutdown never needs to
/// consume (and await) the service.
pub struct McpConnection {
    server_name: String,
    service: RunningService<RoleClient, ClientConfig>,
    tools: Vec<McpToolInfo>,
    stderr_tail: Option<StderrTail>,
    cancel_token: Option<RunningServiceCancellationToken>,
}

impl McpConnection {
    /// Connect to a server and cache its tool list.
    pub async fn connect(server_name: &str, spec: &McpServerSpec) -> Result<Self, String> {
        match spec {
            McpServerSpec::Stdio {
                command,
                args,
                envs,
            } => {
                let cmd = command_for(command, args, envs);
                let (transport, stderr) = TokioChildProcess::builder(cmd)
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|e| {
                        format!("MCP server '{server_name}': failed to spawn '{command}': {e}")
                    })?;
                let tail = Arc::default();
                if let Some(stderr) = stderr {
                    spawn_stderr_drain(stderr, Arc::clone(&tail));
                }
                Self::serve(server_name, transport, Some(tail)).await
            }
            McpServerSpec::Http { url, headers } => {
                let header_map = http_headers(headers)?;
                let config = rmcp::transport::streamable_http_client::
                    StreamableHttpClientTransportConfig::with_uri(url.clone())
                    .custom_headers(header_map);
                let transport = rmcp::transport::StreamableHttpClientTransport::from_config(config);
                Self::serve(server_name, transport, None).await
            }
        }
    }

    /// Serve the client over an arbitrary transport. The transport is
    /// consumed by the service loop; the connect timeout bounds the whole
    /// handshake + tool listing, and aborting it drops the transport (which
    /// kills a spawned child process).
    pub(crate) async fn serve<T, E, A>(
        server_name: &str,
        transport: T,
        stderr_tail: Option<StderrTail>,
    ) -> Result<Self, String>
    where
        T: IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let flow = async {
            let service = client_config()
                .serve_with_lifecycle(transport, ClientLifecycleMode::Initialize)
                .await
                .map_err(|e| e.to_string())?;
            let tools = service.list_all_tools().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((service, tools))
        };
        let (service, tools) = match tokio::time::timeout(CONNECT_TIMEOUT, flow).await {
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                return Err(format!(
                    "MCP server '{server_name}' failed to initialize: {e}"
                ));
            }
            Err(_) => {
                return Err(format!(
                    "MCP server '{server_name}' did not initialize within {}s",
                    CONNECT_TIMEOUT.as_secs()
                ));
            }
        };
        let cancel_token = service.cancellation_token();
        Ok(Self {
            server_name: server_name.to_string(),
            service,
            tools: tools.iter().map(McpToolInfo::from_tool).collect(),
            stderr_tail,
            cancel_token: Some(cancel_token),
        })
    }

    /// Call a tool on the connected server.
    pub async fn call_tool(
        &self,
        tool: &str,
        arguments: Option<JsonObject>,
        timeout: Duration,
    ) -> Result<McpToolOutput, String> {
        let mut params = CallToolRequestParams::new(tool.to_owned());
        if let Some(args) = arguments {
            params = params.with_arguments(args);
        }
        let call = self.service.call_tool(params);
        let result = tokio::time::timeout(timeout, call)
            .await
            .map_err(|_| {
                format!(
                    "MCP tool '{tool}' on server '{}' timed out after {}s",
                    self.server_name,
                    timeout.as_secs()
                )
            })?
            .map_err(|e| self.describe_error(e))?;
        Ok(McpToolOutput::from_result(result))
    }

    /// Re-fetch and cache the tool list.
    pub async fn refresh_tools(&mut self) -> Result<Vec<McpToolInfo>, String> {
        let tools = self
            .service
            .list_all_tools()
            .await
            .map_err(|e| self.describe_error(e))?;
        self.tools = tools.iter().map(McpToolInfo::from_tool).collect();
        Ok(self.tools.clone())
    }

    /// The cached tool descriptors from the last `tools/list`.
    pub fn tools(&self) -> &[McpToolInfo] {
        &self.tools
    }

    /// Whether the underlying transport or service loop is gone.
    pub fn closed(&self) -> bool {
        self.service.is_transport_closed() || self.service.is_closed()
    }

    /// The last stderr lines captured from a stdio server, if any.
    pub fn stderr_tail(&self) -> Option<String> {
        let tail = self.stderr_tail.as_ref()?;
        let tail = tail.lock().ok()?;
        if tail.is_empty() {
            return None;
        }
        Some(tail.iter().cloned().collect::<Vec<_>>().join("\n"))
    }

    /// The server name this connection belongs to.
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Cancel the service loop. The loop then closes the transport, which
    /// for stdio gracefully shuts the child down (waiting up to its own
    /// timeout before killing it).
    pub fn shutdown(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
        self.tools.clear();
    }

    fn describe_error(&self, error: ServiceError) -> String {
        let mut message = match error {
            ServiceError::McpError(e) => format!("server error: {e}"),
            ServiceError::TransportSend(_) | ServiceError::TransportClosed => {
                format!(
                    "transport to server '{}' closed (it may have crashed); \
                     reconnecting happens automatically on the next call",
                    self.server_name
                )
            }
            ServiceError::Cancelled { reason } => {
                format!("cancelled: {}", reason.as_deref().unwrap_or("<unknown>"))
            }
            other => other.to_string(),
        };
        if let Some(tail) = self.stderr_tail() {
            message.push_str("; server stderr: ");
            message.push_str(&tail);
        }
        message
    }
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        if let Some(token) = self.cancel_token.take() {
            token.cancel();
        }
    }
}

/// The normalized view of a `tools/call` result.
impl McpToolOutput {
    fn from_result(result: rmcp::model::CallToolResult) -> Self {
        let text: Vec<&str> = result
            .content
            .iter()
            .filter_map(|block: &ContentBlock| block.as_text().map(|t| t.text.as_str()))
            .collect();
        let text = if text.is_empty() {
            match result.structured_content {
                Some(value) => serde_json::to_string_pretty(&value).unwrap_or_default(),
                None => String::new(),
            }
        } else {
            text.join("\n")
        };
        Self {
            text,
            is_error: result.is_error.unwrap_or(false),
        }
    }
}

/// The descriptor view of an rmcp `Tool`.
impl McpToolInfo {
    pub(crate) fn from_tool(tool: &rmcp::model::Tool) -> Self {
        Self {
            name: tool.name.to_string(),
            title: tool.title.clone(),
            description: tool.description.as_ref().map(|d| d.to_string()),
            input_schema: serde_json_schema(&tool.input_schema),
        }
    }
}

fn serde_json_schema(schema: &JsonObject) -> Option<Value> {
    if schema.is_empty() {
        None
    } else {
        Some(Value::Object(schema.clone()))
    }
}

/// Build the child-process command for a stdio server: the executable
/// resolved through `PATH` where possible (Windows `.cmd` shims need the
/// absolute path), argv, and the environment overlay.
pub(crate) fn command_for(command: &str, args: &[String], envs: &EnvSpec) -> Command {
    let mut cmd = which_command(command).unwrap_or_else(|_| Command::new(command));
    cmd.args(args);
    if envs.inherit {
        for (name, value) in &envs.entries {
            cmd.env(name, value);
        }
    } else {
        cmd.env_clear();
        cmd.envs(envs.entries.iter());
    }
    cmd
}

/// Parse header names/values for the streamable-HTTP transport.
fn http_headers(
    headers: &BTreeMap<String, String>,
) -> Result<HashMap<http::HeaderName, http::HeaderValue>, String> {
    let mut out = HashMap::new();
    for (name, value) in headers {
        let name = http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| format!("invalid HTTP header name '{name}': {e}"))?;
        let value = http::HeaderValue::from_str(value)
            .map_err(|e| format!("invalid HTTP header value for '{name}': {e}"))?;
        out.insert(name, value);
    }
    Ok(out)
}

/// Drain a server's stderr into a bounded ring buffer so error messages can
/// quote it without the pipe ever filling up and blocking the server.
fn spawn_stderr_drain(stderr: tokio::process::ChildStderr, tail: StderrTail) {
    tokio::spawn(async move {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(mut guard) = tail.lock() else {
                break;
            };
            guard.push_back(line);
            while guard.len() > STDERR_TAIL_LINES {
                guard.pop_front();
            }
        }
    });
}

/// The client identity Shuvarie advertises to servers.
fn client_config() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("shuvarie", env!("CARGO_PKG_VERSION")),
    )
}
