//! The MCP registry: configured servers, their live connections, and the
//! bookkeeping the rest of Shuvarie talks to.
//!
//! Mirrors `shuvarie_lsp::LspManager`: specs come from config (converted and
//! env-resolved in `shuvarie-core`), connections are established lazily on
//! first use, and a crashed connection is re-established automatically on
//! the next call against that server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::config::McpServerSpec;
use crate::connection::McpConnection;
use crate::types::{
    McpStatus, McpStatusState, McpToolInfo, McpToolMap, McpToolOutput, composite_tool_name,
};

/// The per-call timeout when the caller does not specify one.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Registry of configured MCP servers and their connections.
pub struct McpManager {
    specs: BTreeMap<String, McpServerSpec>,
    connections: BTreeMap<String, McpConnection>,
    /// The failure reason of the last connect/call per server; cleared when
    /// a connection is (re-)established successfully.
    last_errors: BTreeMap<String, String>,
    call_timeout: Duration,
}

impl McpManager {
    pub fn new(specs: BTreeMap<String, McpServerSpec>) -> Self {
        Self {
            specs,
            connections: BTreeMap::new(),
            last_errors: BTreeMap::new(),
            call_timeout: DEFAULT_CALL_TIMEOUT,
        }
    }

    /// Override the default per-call timeout.
    #[must_use]
    pub fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }

    /// The configured specs, keyed by server name.
    pub fn specs(&self) -> &BTreeMap<String, McpServerSpec> {
        &self.specs
    }

    /// Whether a server with this name is configured.
    pub fn has(&self, name: &str) -> bool {
        self.specs.contains_key(name)
    }

    /// Names of servers with a live connection.
    pub fn connected_servers(&self) -> Vec<String> {
        self.connections.keys().cloned().collect()
    }

    /// Whether the server is connected and its transport is alive.
    pub fn connected(&self, name: &str) -> bool {
        self.connections
            .get(name)
            .is_some_and(|conn| !conn.closed())
    }

    /// Connect if the server is not already reachable. A closed-but-present
    /// connection is replaced by a fresh one (automatic reconnect).
    pub async fn ensure_connected(&mut self, name: &str) -> Result<(), String> {
        if self.connected(name) {
            return Ok(());
        }
        self.connect(name).await
    }

    /// Force a fresh connection: drops any existing one first.
    pub async fn reconnect(&mut self, name: &str) -> Result<(), String> {
        self.disconnect(name);
        self.connect(name).await
    }

    async fn connect(&mut self, name: &str) -> Result<(), String> {
        let Some(spec) = self.specs.get(name).cloned() else {
            return Err(format!("no MCP server configured with name '{name}'"));
        };
        self.connections.remove(name);
        match McpConnection::connect(name, &spec).await {
            Ok(connection) => {
                self.last_errors.remove(name);
                self.connections.insert(name.to_string(), connection);
                Ok(())
            }
            Err(error) => {
                self.last_errors.insert(name.to_string(), error);
                Err(format!("failed to connect to MCP server '{name}'"))
            }
        }
    }

    /// Call a tool on a server, connecting (or reconnecting) first. A call
    /// that fails because the server is gone drops the connection so the
    /// next call reconnects.
    pub async fn call_tool(
        &mut self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
        timeout: Option<Duration>,
    ) -> Result<McpToolOutput, String> {
        self.ensure_connected(server).await?;
        let Some(conn) = self.connections.get(server) else {
            return Err(format!("no MCP server configured with name '{server}'"));
        };
        let result = conn
            .call_tool(tool, arguments, timeout.unwrap_or(self.call_timeout))
            .await;
        if let Err(error) = &result
            && self
                .connections
                .get(server)
                .is_some_and(McpConnection::closed)
        {
            // The server is gone: drop the connection so the next call
            // reconnects, and remember why for the status line.
            self.last_errors.insert(server.to_string(), error.clone());
            self.connections.remove(server);
        }
        result
    }

    /// The cached tools of a connected server.
    pub fn cached_tools(&self, server: &str) -> Option<&[McpToolInfo]> {
        self.connections.get(server).map(McpConnection::tools)
    }

    /// The cached tools of a connected server, keyed by composite tool name
    /// (`mcp__<server>__<tool>`).
    pub fn tool_map(&self, server: &str) -> McpToolMap {
        let Some(tools) = self.cached_tools(server) else {
            return BTreeMap::new();
        };
        tools
            .iter()
            .map(|info| (composite_tool_name(server, &info.name), info.clone()))
            .collect()
    }

    /// Re-fetch the tool list of a connected server.
    pub async fn refresh_tools(&mut self, server: &str) -> Result<Vec<McpToolInfo>, String> {
        self.ensure_connected(server).await?;
        self.connections
            .get_mut(server)
            .ok_or_else(|| format!("no MCP server configured with name '{server}'"))?
            .refresh_tools()
            .await
    }

    /// Disconnect a server, if connected. The connection's service loop
    /// closes the transport asynchronously; for stdio that gracefully shuts
    /// the child down.
    pub fn disconnect(&mut self, name: &str) {
        if let Some(mut conn) = self.connections.remove(name) {
            conn.shutdown();
        }
    }

    /// Disconnect every server.
    pub fn shutdown_all(&mut self) {
        let names: Vec<String> = self.connections.keys().cloned().collect();
        for name in names {
            self.disconnect(&name);
        }
    }

    /// The status of every configured server, for the `mcp` overlay.
    pub fn status_snapshot(&self) -> Vec<McpStatus> {
        self.specs
            .iter()
            .map(|(name, spec)| {
                let (state, tools, error) = match self.connections.get(name) {
                    Some(conn) if conn.closed() => {
                        (McpStatusState::Crashed, conn.tools().len(), None)
                    }
                    Some(conn) => (McpStatusState::Connected, conn.tools().len(), None),
                    None => match self.last_errors.get(name) {
                        Some(error) => (McpStatusState::Failed, 0, Some(error.clone())),
                        None => (McpStatusState::Configured, 0, None),
                    },
                };
                McpStatus {
                    name: name.clone(),
                    transport: spec.transport_label(),
                    state,
                    tools,
                    error,
                }
            })
            .collect()
    }
}

/// The shared handle core keeps around, mirroring the LSP manager.
pub type SharedMcpManager = Arc<Mutex<McpManager>>;

#[cfg(test)]
impl McpManager {
    /// Insert a pre-built connection (test support only).
    pub(crate) fn inject_connection(&mut self, name: &str, connection: McpConnection) {
        self.connections.insert(name.to_string(), connection);
    }
}
