//! Failures a script can see.
//!
//! A tool call that the host refuses (a denied permission a script cannot see
//! past, a tool that failed, a name that is not in the roster) becomes a Rhai
//! runtime error, so a script can `try`/`catch` it and choose what to do. It
//! carries no position: the position is the host's to know, and the call's
//! trace records it instead.

use rhai::{Dynamic, EvalAltResult, Position};

/// Wrap a host-side failure as a Rhai error a script can catch.
pub fn to_runtime_error(err: impl std::fmt::Display) -> Box<EvalAltResult> {
    Box::new(EvalAltResult::ErrorRuntime(
        Dynamic::from(err.to_string()),
        Position::NONE,
    ))
}
