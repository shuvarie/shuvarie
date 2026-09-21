//! Public data types of the MCP module.

use std::collections::BTreeMap;

/// One tool advertised by a connected MCP server, captured from
/// `tools/list` at connect time (and on explicit refresh).
#[derive(Debug, Clone, PartialEq)]
pub struct McpToolInfo {
    /// The raw MCP tool name, e.g. `create_issue`. Composite agent-facing
    /// names are `mcp__<server>__<tool>` and assembled where the tool is
    /// registered.
    pub name: String,
    pub title: Option<String>,
    pub description: Option<String>,
    /// The tool's `inputSchema` JSON object, passed through to the model.
    pub input_schema: Option<serde_json::Value>,
}

/// The outcome of a `tools/call` request, normalized for the agent tool
/// bridge: text content blocks joined with newlines plus the `isError` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpToolOutput {
    /// The tool's text content; empty when the server returned only
    /// non-text blocks. Falls back to pretty-printed `structuredContent`
    /// when there is no text at all.
    pub text: String,
    /// `isError` from the result: the tool ran but reported a failure. The
    /// connection is still alive — this is not a protocol error.
    pub is_error: bool,
}

/// Connection state of one configured MCP server, for status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpStatusState {
    /// Configured but never connected (or intentionally disconnected).
    Configured,
    /// Connected; the cached tool list is fresh.
    Connected,
    /// The last connect or call attempt failed with an error.
    Failed,
    /// The connection existed but the server went away (crash, network).
    Crashed,
}

impl McpStatusState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured (not connected)",
            Self::Connected => "connected",
            Self::Failed => "failed",
            Self::Crashed => "crashed",
        }
    }
}

/// One line of an MCP status snapshot, mirroring the LSP manager's
/// `LspStatus` shape.
#[derive(Debug, Clone, PartialEq)]
pub struct McpStatus {
    pub name: String,
    /// `"stdio"` or `"http"`.
    pub transport: &'static str,
    pub state: McpStatusState,
    /// Number of tools advertised (0 unless connected).
    pub tools: usize,
    /// The failure reason for `Failed`/`Crashed`, when known.
    pub error: Option<String>,
}

/// The tool names a server offers, keyed by server name — the composite
/// naming helper.
///
/// Composite tool names follow Claude's convention:
/// `mcp__<server>__<tool>`.
pub fn composite_tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// The tool map of one MCP server, as a sorted map from composite tool name
/// to descriptor.
pub type McpToolMap = BTreeMap<String, McpToolInfo>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_names_are_prefixed_with_reserved_marker() {
        assert_eq!(
            composite_tool_name("github", "create_issue"),
            "mcp__github__create_issue"
        );
    }

    #[test]
    fn status_state_strings() {
        assert_eq!(
            McpStatusState::Configured.as_str(),
            "configured (not connected)"
        );
        assert_eq!(McpStatusState::Connected.as_str(), "connected");
        assert_eq!(McpStatusState::Failed.as_str(), "failed");
        assert_eq!(McpStatusState::Crashed.as_str(), "crashed");
    }
}
