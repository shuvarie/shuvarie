//! MCP (Model Context Protocol) integration for Shuvarie.
//!
//! This crate owns the lifecycle of MCP servers configured in
//! `config.kdl`'s `tools { mcp { … } }` section, using the official Rust SDK
//! ([`rmcp`]) as the client side. It is depended on by `shuvarie-core`, which
//! wraps an [`McpManager`] in an `Arc<tokio::sync::Mutex<..>>`, bridges the
//! servers' tools into the agent's tool roster as `mcp__<server>__<tool>`,
//! and surfaces status through the `mcp` command.
//!
//! Supported transports ([`McpServerSpec`]):
//!
//! - `Stdio`: the server is spawned as a child process and talked to over
//!   stdio (the `transport-child-process` feature).
//! - `Http`: the server is reached over the streamable-HTTP transport with
//!   per-request custom headers (the
//!   `transport-streamable-http-client-reqwest` feature).
//!
//! Connections are established lazily on first use; a server that goes away
//! is reconnected automatically on the next call against it. Stdio servers
//! have their stderr captured into a bounded ring buffer so crash messages
//! can quote it.

#![allow(dead_code)]

pub mod config;
mod connection;
mod manager;
#[cfg(test)]
mod tests;
pub mod types;

pub use config::{EnvSpec, McpServerSpec};
pub use manager::{DEFAULT_CALL_TIMEOUT, McpManager, SharedMcpManager};
pub use types::{
    McpStatus, McpStatusState, McpToolInfo, McpToolMap, McpToolOutput, composite_tool_name,
};
