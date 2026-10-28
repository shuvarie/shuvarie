//! LSP integration for Shuvarie.
//!
//! This crate owns the lifecycle of Language Server Protocol servers spawned as child
//! processes and talked to via stdio, using `async-lsp` as the client side. It is depended
//! on by `shuvarie-core`, which wraps an `LspManager` in an `Arc<tokio::sync::Mutex<..>>`
//! and shares it with the agent tools (file tools auto-start the matching server; the
//! `lsp` tool exposes start/list/stop/restart).
//!
//! Server specs are sourced from a built-in per-language table (`registry::builtin`)
//! and can be overridden or extended through `server <lang>` nodes in `config.kdl`'s `lsp` section
//! (see `config::LspConfig`). Missing binaries are probed lazily on `start` and skipped
//! silently so the rest of the manager keeps working on systems without a given server.

#![allow(dead_code)]
// This crate runs inside the TUI, which owns the terminal: anything printed by
// a background LSP task would land on top of the rendered frame and stay there
// until ratatui happens to repaint those cells. Failures belong on the
// manager's event channel (see `Server::exit_error`), never on stdout/stderr.
#![deny(clippy::print_stdout, clippy::print_stderr)]

pub mod config;
mod manager;
pub mod registry;
mod server;
mod types;
mod util;

pub use config::{LspConfig, LspServerSpec};
pub use manager::{LspManager, ServerListEntry, ServerListStatus};
pub use types::{DiagnosticInfo, DiagnosticSeverity, LspStatus, ServerStatus};
