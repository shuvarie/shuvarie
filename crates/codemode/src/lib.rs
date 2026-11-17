//! `shuvarie-codemode`: Python as the codemode scripting language.
//!
//! Codemode presents the roster as **one** tool that runs a script, and the
//! script calls the other tools. Intermediate results never enter the context
//! window, several dependent calls become one round trip, and the model reads
//! back only what it asked for. See `docs/designs/codemode.md`.
//!
//! The language is Python, embedded through `pyo3`: no subprocess, no protocol,
//! no isolate. Python was chosen over a purpose-built script language because
//! the model already knows it -- the dialect card only has to describe *this*
//! surface, not a language. This crate is everything language-side and knows
//! nothing about shuvarie: the host is a [`ToolInvoker`], so the crate is
//! unit-testable with stub tools.
//!
//! The path through it:
//!
//! - [`python`] -- whether there is an interpreter to run, and which one. No
//!   interpreter, no tool: a probe, not a fallback.
//! - [`spec`] -- the catalog: which tools a script may call, and under which
//!   name. Python identifiers are not tool names (MCP tools are full of `-`), so
//!   names are mangled here and collisions resolved deterministically.
//! - [`render`] -- the catalog as the tool's description: a Python signature per
//!   tool, inlined up to a budget of estimated tokens.
//! - [`doc`] -- the dialect card: this surface, in the language the model already
//!   speaks.
//! - [`discovery`] -- `search_tools` / `describe_tool` / `describe_namespace` /
//!   `ALL_TOOLS`: how a script reaches a tool the description did not inline.
//! - [`skill`] -- the fuller reference, shipped as a built-in skill and loaded
//!   on demand.
//! - [`text`] -- JSON, the only thing that crosses between host and script, as
//!   the text a result is rendered from.
//! - [`script`] -- the interpreter, its limits, and the thread bridge that lets a
//!   script call an async tool.
//! - [`result`] -- the result block the model reads back.

pub mod discovery;
pub mod doc;
pub mod python;
pub mod render;
pub mod result;
pub mod script;
pub mod skill;
pub mod spec;
pub mod text;

pub use discovery::{DEFAULT_NAMESPACE_LIMIT, DEFAULT_SEARCH_LIMIT};
pub use render::{RenderOptions, describe, estimated_tokens, signature};
pub use result::block;
pub use script::{
    CallTrace, ErrorKind, Limits, ScriptError, ScriptOutcome, ScriptRequest, ScriptValue, Stop,
    StopReason, Store, ToolCall, ToolInvoker, humantime, run,
};
pub use spec::{Catalog, MCP_NS, TOOLS_NS, ToolEntry, ToolSpec, callable_name};
