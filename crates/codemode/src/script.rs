//! Running a script: the interpreter, its limits, and the thread bridge.
//!
//! Python is embedded (`pyo3`), and a tool called from a script cannot be
//! awaited from inside the interpreter either. The pattern is the same one the
//! engine had before: the interpreter runs the script on a dedicated thread,
//! every tool call is a native function that sends a request over a channel and
//! **blocks** on the reply, and a task on the async side performs the real call.
//! While it waits, the call releases the GIL (`Python::detach`), so the runtime
//! serving it -- and the watchdog that can interrupt it -- keep running. The
//! interpreter therefore never crosses a thread boundary, and -- the reason it is
//! worth a thread -- a nested call is the *real* tool call, running through the
//! same permission checks and the same result plumbing as a direct one.
//!
//! The Python side is `runtime.py`, embedded in this crate and loaded as a module
//! once per run. It owns everything the language decides: the script's globals,
//! the value it produces, how a failure reads, and the interrupt that stops a
//! runaway script. What crosses back is one JSON envelope plus the collected
//! output and call trace, so no Python object outlives the run.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pyo3::types::PyModule;
use serde_json::{Value, json};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::python;
use crate::spec::Catalog;
use crate::text;
use crate::{DEFAULT_NAMESPACE_LIMIT, DEFAULT_SEARCH_LIMIT, discovery};

/// The runtime, compiled into the binary: a single-file binary has nowhere to
/// keep a `.py` beside it, and a script that only works after `cargo install`
/// put the file in the right place would be worse than no script at all.
const RUNTIME: &str = include_str!("runtime.py");

/// How many `text()` items and console lines are kept. A script can print
/// forever; the collector cannot.
const MAX_COLLECTED: usize = 1_000;

/// A nested tool call, as the host performs it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    /// The roster name to dispatch on.
    pub host_name: String,
    /// The arguments the script passed.
    pub args: Value,
}

/// The host side of a nested call.
///
/// The invoker is what makes this crate host-agnostic and unit-testable: the
/// real one dispatches the roster's tools (with their permissions, streams and
/// side effects); a test one returns canned JSON.
pub type InvokeFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, String>> + Send + 'a>>;

pub trait ToolInvoker: Send + Sync + 'static {
    /// Perform one call. An `Err` is the message a script sees if it catches,
    /// and what the call's trace records if it does not.
    fn invoke(&self, call: ToolCall) -> InvokeFuture<'_>;
}

/// State a script may keep between calls (`store`/`load`).
///
/// Deliberately not persisted: the host decides how long a store lives, and
/// today it lives for the session.
#[derive(Debug, Default)]
pub struct Store {
    entries: Mutex<BTreeMap<String, Value>>,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<Value> {
        self.entries.lock().unwrap().get(key).cloned()
    }

    pub fn set(&self, key: &str, value: Value) {
        self.entries.lock().unwrap().insert(key.to_string(), value);
    }

    pub fn keys(&self) -> Vec<String> {
        self.entries.lock().unwrap().keys().cloned().collect()
    }
}

/// What a script may spend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Wall-clock budget for the whole script. `None` is no deadline, which
    /// leaves a runaway script bounded only by `max_operations`.
    pub timeout: Option<Duration>,
    /// The script's own lines, counted by a trace hook. `None` is unlimited, and
    /// also means no trace hook at all -- the interpreter runs at full speed.
    pub max_operations: Option<u64>,
    /// Nested tool calls. `None` is unlimited.
    pub max_tool_calls: Option<usize>,
    /// Ceiling on the rendered result, applied when the block is built.
    pub max_output_chars: usize,
    /// Whether `store`/`load` exist at all.
    pub store: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: None,
            max_operations: None,
            max_tool_calls: None,
            max_output_chars: 20_000,
            store: true,
        }
    }
}

/// Why a script stopped before it was done.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The host dropped the run, or its turn was aborted.
    Cancelled,
    /// The wall-clock budget ran out.
    TimedOut,
    /// The host side went away mid-call.
    HostGone,
    /// `max_tool_calls` was reached.
    ToolLimit,
}

/// The stop signal, shared by the caller, the timer, and the script's watchdog.
///
/// One token per run: the caller flips it (a turn abort), a timer flips it when
/// the budget runs out, and dropping [`run`]'s future flips it too, so a script
/// never outlives the call that started it. The first reason to arrive wins,
/// which is why the state is a compare-and-swap rather than a flag.
#[derive(Clone, Debug, Default)]
pub struct Stop {
    state: Arc<AtomicU8>,
}

impl Stop {
    pub fn new() -> Self {
        Self::default()
    }

    /// The first reason wins; later ones are ignored.
    fn set(&self, reason: StopReason) {
        let _ = self.state.compare_exchange(
            RUNNING,
            reason.code(),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub fn cancel(&self) {
        self.set(StopReason::Cancelled);
    }

    pub fn timed_out(&self) {
        self.set(StopReason::TimedOut);
    }

    fn host_gone(&self) {
        self.set(StopReason::HostGone);
    }

    fn tool_limit(&self) {
        self.set(StopReason::ToolLimit);
    }

    fn reason(&self) -> Option<StopReason> {
        StopReason::decode(self.state.load(Ordering::Acquire))
    }
}

// The stop signal's states. `StopReason`'s own discriminants are deliberately
// not reused: `RUNNING` has to be a value no reason can take, or the first
// reason to arrive (`Cancelled`, at discriminant zero) would be
// indistinguishable from a script that is still going.
const RUNNING: u8 = 0;

impl StopReason {
    const fn code(self) -> u8 {
        match self {
            StopReason::Cancelled => 1,
            StopReason::TimedOut => 2,
            StopReason::HostGone => 3,
            StopReason::ToolLimit => 4,
        }
    }

    const fn decode(state: u8) -> Option<Self> {
        match state {
            1 => Some(StopReason::Cancelled),
            2 => Some(StopReason::TimedOut),
            3 => Some(StopReason::HostGone),
            4 => Some(StopReason::ToolLimit),
            _ => None,
        }
    }

    /// What the runtime's watchdog reads, and the only thing about a stop the
    /// interpreter needs to know.
    const fn name(self) -> &'static str {
        match self {
            StopReason::Cancelled => "cancelled",
            StopReason::TimedOut => "timed-out",
            StopReason::HostGone => "host-gone",
            StopReason::ToolLimit => "tool-limit",
        }
    }
}

/// One script, and everything it needs.
pub struct ScriptRequest {
    pub script: String,
    /// The tools this script may call. Names are the catalog's, not the
    /// roster's.
    pub catalog: Arc<Catalog>,
    /// Where `store`/`load` keep state, when the limits allow them.
    pub store: Option<Arc<Store>>,
    pub limits: Limits,
}

/// What a script produced.
#[derive(Debug)]
pub struct ScriptValue {
    /// The values `text()` emitted, in order.
    pub items: Vec<Value>,
    /// The script's own value, when it produced one.
    pub value: Option<Value>,
}

/// One nested call, for the result's trace.
#[derive(Debug)]
pub struct CallTrace {
    /// The name the script used.
    pub qualified: String,
    /// The roster tool behind it.
    pub host_name: String,
    /// The arguments, previewed.
    pub args: String,
    pub duration: Duration,
    /// The failure message, when the call failed.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The script did not compile.
    Parse,
    /// The script failed while running.
    Eval,
    /// A configured limit stopped it.
    Limit,
    /// The host stopped it: cancelled, timed out, or the host went away.
    Stopped,
}

/// Why a script did not produce a value.
#[derive(Debug)]
pub struct ScriptError {
    pub kind: ErrorKind,
    pub message: String,
    /// The script's line, when the failure has one (not every failure does).
    pub line: Option<usize>,
    pub column: Option<usize>,
}

/// The outcome of one run, successful or not.
#[derive(Debug)]
pub struct ScriptOutcome {
    pub result: Result<ScriptValue, ScriptError>,
    /// Everything the script printed, in order.
    pub logs: Vec<String>,
    /// Every nested call, in order.
    pub calls: Vec<CallTrace>,
    pub elapsed: Duration,
    /// How many `text()`/console entries were dropped at [`MAX_COLLECTED`].
    pub dropped: usize,
}

impl ScriptOutcome {
    /// A run that never started, for a failure that happens before the thread
    /// does: a panic in it, or an interpreter that could not be started.
    fn failed(error: ScriptError, elapsed: Duration) -> Self {
        Self {
            result: Err(error),
            logs: Vec::new(),
            calls: Vec::new(),
            elapsed,
            dropped: 0,
        }
    }
}

/// Run one script against one invoker.
///
/// Runs on the caller's runtime: the script gets a thread of its own, the calls
/// it makes happen on this one. Dropping the returned future stops the script
/// (see [`Stop`]).
pub async fn run(
    request: ScriptRequest,
    invoker: Arc<dyn ToolInvoker>,
    stop: Stop,
) -> ScriptOutcome {
    let started = Instant::now();

    // Requests go one way, replies the other; the script thread blocks on the
    // reply, which is what makes a synchronous-looking call possible at all.
    let (requests, incoming) = tokio::sync::mpsc::unbounded_channel::<CallRequest>();
    // The serving task belongs to the run, so the run says when it is over rather
    // than waiting for every sender to drop. A sender lives inside the host object
    // the interpreter holds, and a reference to *that* surviving in the interpreter
    // -- a closure's globals, an exception's traceback -- would otherwise leave a
    // task (with the whole roster in it) alive for the life of the process.
    let (served, served_done) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(serve_calls(incoming, invoker, served_done));
    let mut served = Finished(Some(served));

    // The deadline lives on this side, as one task that flips the same signal the
    // caller does; the script's watchdog is what turns the signal into an
    // interrupt. `done` is what stops the timer from outliving the run: it is
    // dropped when this future is, so a cancelled turn takes its timer with it.
    let (finished, done) = tokio::sync::oneshot::channel::<()>();
    if let Some(timeout) = request.limits.timeout {
        let alarm = stop.clone();
        tokio::spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(timeout) => alarm.timed_out(),
                _ = done => {}
            }
        });
    }

    let (outcome_tx, outcome) = tokio::sync::oneshot::channel::<ScriptOutcome>();
    // The interpreter, the module, and the script never leave this thread.
    let script_stop = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("codemode-script".to_string())
        .spawn(move || {
            let outcome = evaluate(request, requests, script_stop.clone(), started);
            // The receiver may be gone (the caller dropped the run); the result
            // is then nobody's to read, which is not an error.
            let _ = outcome_tx.send(outcome);
            // Keep the timer's exit signal alive until the script is done.
            drop(finished);
        });

    if spawned.is_err() {
        served.finish();
        return ScriptOutcome::failed(
            ScriptError {
                kind: ErrorKind::Stopped,
                message: "could not start the script thread".to_string(),
                line: None,
                column: None,
            },
            started.elapsed(),
        );
    }

    // Whoever walks away first stops the script: a dropped run sets the signal
    // the script's watchdog checks.
    let guard = StopOnDrop(stop);
    match outcome.await {
        Ok(outcome) => {
            drop(guard);
            served.finish();
            outcome
        }
        Err(_) => {
            drop(guard);
            served.finish();
            ScriptOutcome::failed(
                ScriptError {
                    kind: ErrorKind::Stopped,
                    message: "the script thread ended without a result".to_string(),
                    line: None,
                    column: None,
                },
                started.elapsed(),
            )
        }
    }
}

/// Tells the call-serving task that the run is over, wherever `run` returns from —
/// including the caller dropping the future.
struct Finished(Option<tokio::sync::oneshot::Sender<()>>);

impl Finished {
    fn finish(&mut self) {
        if let Some(told) = self.0.take() {
            let _ = told.send(());
        }
    }
}

impl Drop for Finished {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Dropping the run stops the script.
struct StopOnDrop(Stop);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// A call from the script thread, waiting for its reply.
struct CallRequest {
    host_name: String,
    args: Value,
    reply: tokio::sync::oneshot::Sender<Result<Value, String>>,
}

/// Perform the calls the script asks for.
///
/// One task per call: the script thread is blocked on its reply, so calls are
/// sequential in practice -- but this is the shape a fan-out needs, and it
/// keeps a slow tool from delaying the accept loop.
async fn serve_calls(
    mut requests: UnboundedReceiver<CallRequest>,
    invoker: Arc<dyn ToolInvoker>,
    mut finished: tokio::sync::oneshot::Receiver<()>,
) {
    loop {
        let request = tokio::select! {
            // The run is over. Whatever still holds a sender is a leftover in the
            // interpreter, not a script waiting for an answer.
            _ = &mut finished => break,
            request = requests.recv() => match request {
                Some(request) => request,
                None => break,
            },
        };
        let invoker = Arc::clone(&invoker);
        tokio::spawn(async move {
            let result = invoker
                .invoke(ToolCall {
                    host_name: request.host_name,
                    args: request.args,
                })
                .await;
            // The script may be gone; a reply nobody reads is fine, but the
            // sender must be dropped so a blocked `blocking_recv` wakes up.
            let _ = request.reply.send(result);
        });
    }
}

/// What the script's own callbacks collected.
#[derive(Default)]
struct Collected {
    items: Vec<Value>,
    logs: Vec<String>,
    calls: Vec<CallTrace>,
    dropped: usize,
    tool_calls: usize,
}

/// One run's host side: everything the runtime may ask of it, and its counters.
struct Run {
    catalog: Arc<Catalog>,
    store: Option<Arc<Store>>,
    limits: Limits,
    requests: UnboundedSender<CallRequest>,
    stop: Stop,
    collected: Mutex<Collected>,
    search_limit: usize,
    namespace_limit: usize,
}

impl Run {
    fn collect(&self, entry: impl FnOnce(&mut Collected)) {
        entry(&mut self.collected.lock().unwrap());
    }
}

/// The host, as Python sees it: a `Host` instance handed to the runtime as
/// `_HOST`, with one method per thing a script may ask for.
///
/// A value per run rather than module-level functions, because two sessions can
/// run scripts at once and a global would have them answer for each other.
#[pyclass(module = "shuvarie_codemode_runtime")]
struct Host {
    run: Arc<Run>,
}

#[pymethods]
impl Host {
    /// A nested tool call: the one thing the script thread waits for.
    fn call(
        &self,
        py: Python<'_>,
        qualified: &str,
        host_name: &str,
        args_json: &str,
    ) -> PyResult<String> {
        let args: Value = serde_json::from_str(args_json)
            .map_err(|err| host_error(format!("arguments: {err}")))?;
        let preview = text::preview(&args, 160);

        // Counted before the call so the trace of the call that hit the limit is
        // still recorded, and the script stops cleanly at the next step.
        let mut over_limit = false;
        self.run.collect(|collected| {
            collected.tool_calls += 1;
            over_limit = self
                .run
                .limits
                .max_tool_calls
                .is_some_and(|max| collected.tool_calls > max);
        });
        if over_limit {
            self.run.stop.tool_limit();
            let max = self.run.limits.max_tool_calls.unwrap_or_default();
            return Err(host_error(format!(
                "the limit of {max} tool calls for one script was reached at {qualified}"
            )));
        }

        let started = Instant::now();
        let (reply, answer) = tokio::sync::oneshot::channel();
        let sent = self.run.requests.send(CallRequest {
            host_name: host_name.to_string(),
            args,
            reply,
        });
        let outcome = match sent {
            Err(_) => {
                self.run.stop.host_gone();
                Err("the host is no longer accepting calls".to_string())
            }
            // The GIL goes back while the tool runs: the runtime has to be able to
            // serve this very call, and the watchdog has to be able to run.
            Ok(()) => py.detach(|| match answer.blocking_recv() {
                // The reply channel closes only when the host side is gone.
                Ok(reply) => reply,
                Err(_) => {
                    self.run.stop.host_gone();
                    Err("the host stopped before the call finished".to_string())
                }
            }),
        };
        let duration = started.elapsed();

        self.run.collect(|collected| {
            collected.calls.push(CallTrace {
                qualified: qualified.to_string(),
                host_name: host_name.to_string(),
                args: preview,
                duration,
                error: outcome.as_ref().err().cloned(),
            });
        });

        let result = outcome.map_err(host_error)?;
        Ok(result.to_string())
    }

    /// `text(value)`: one value into the result, beside the script's own.
    fn emit(&self, value_json: &str) {
        let value = serde_json::from_str(value_json).unwrap_or(Value::Null);
        self.run.collect(|collected| {
            if collected.items.len() < MAX_COLLECTED {
                collected.items.push(value);
            } else {
                collected.dropped += 1;
            }
        });
    }

    /// Console output: captured rather than streamed, because it belongs to the
    /// result.
    fn log(&self, line: &str) {
        let line = line.to_string();
        self.run.collect(|collected| {
            if collected.logs.len() < MAX_COLLECTED {
                collected.logs.push(line);
            } else {
                collected.dropped += 1;
            }
        });
    }

    /// Why the host wants the script to stop, if it does. Polled by the runtime's
    /// watchdog, which turns the answer into an interrupt.
    fn stop_reason(&self) -> Option<&'static str> {
        self.run.stop.reason().map(StopReason::name)
    }

    /// `search_tools` / `describe_tool` / `describe_namespace`, one entry point.
    /// The catalog is the host's, so an answer is always about what this script
    /// may actually call.
    fn discover(&self, request_json: &str) -> PyResult<String> {
        let request: Value = serde_json::from_str(request_json)
            .map_err(|err| host_error(format!("discovery request: {err}")))?;
        let field = |key: &str| request.get(key).and_then(Value::as_str);
        match field("op") {
            Some("search") => {
                let query = field("query").unwrap_or_default();
                let limit = request
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|limit| limit as usize)
                    .unwrap_or(self.run.search_limit);
                Ok(search(&self.run.catalog, query, field("namespace"), limit))
            }
            Some("describe") => {
                let name = field("name").unwrap_or_default();
                discovery::describe_tool(&self.run.catalog, name).map_err(host_error)
            }
            Some("namespace") => {
                let namespace = field("namespace").unwrap_or_default();
                let limit = request
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map(|limit| limit as usize)
                    .unwrap_or(self.run.namespace_limit);
                discovery::describe_namespace(&self.run.catalog, namespace, limit)
                    .map_err(host_error)
            }
            other => Err(host_error(format!("unknown discovery request {other:?}"))),
        }
    }

    fn store_set(&self, key: &str, value_json: &str) {
        let Some(store) = self.run.store.as_ref() else {
            return;
        };
        let value = serde_json::from_str(value_json).unwrap_or(Value::Null);
        store.set(key, value);
    }

    fn store_get(&self, key: &str) -> Option<String> {
        self.run
            .store
            .as_ref()?
            .get(key)
            .map(|value| value.to_string())
    }
}

/// A host failure, as a Python exception the script can catch.
fn host_error(message: impl std::fmt::Display) -> PyErr {
    PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(message.to_string())
}

/// The script thread: load the runtime, compile, run, report.
fn evaluate(
    request: ScriptRequest,
    requests: UnboundedSender<CallRequest>,
    stop: Stop,
    started: Instant,
) -> ScriptOutcome {
    let run = Run {
        catalog: Arc::clone(&request.catalog),
        store: request.store.clone(),
        limits: request.limits.clone(),
        requests,
        stop: stop.clone(),
        collected: Mutex::new(Collected::default()),
        search_limit: DEFAULT_SEARCH_LIMIT,
        namespace_limit: DEFAULT_NAMESPACE_LIMIT,
    };
    let run = Arc::new(run);

    let envelope = match python::availability().interpreter() {
        Some(_) => {
            let script = request.script;
            Python::attach(|py| execute(py, &run, &script))
        }
        None => Err(python::availability()
            .reason()
            .unwrap_or("the Python interpreter is not available")
            .to_string()),
    };

    let elapsed = started.elapsed();
    let (items, logs, calls, dropped) = {
        let mut collected = run.collected.lock().unwrap();
        let taken = std::mem::take(&mut *collected);
        (taken.items, taken.logs, taken.calls, taken.dropped)
    };

    // A stop reason outranks the error that carried it out: the script's own
    // `except BaseException` may have swallowed the interrupt.
    let result = match stop.reason() {
        Some(reason) => Err(stopped(reason, request.limits.timeout)),
        None => match envelope {
            Ok(envelope) => read_envelope(&envelope, items),
            Err(failure) => Err(ScriptError {
                kind: ErrorKind::Eval,
                message: failure,
                line: None,
                column: None,
            }),
        },
    };

    ScriptOutcome {
        result,
        logs,
        calls,
        elapsed,
        dropped,
    }
}

/// Run one script inside the interpreter, answering with the runtime's envelope.
fn execute(py: Python<'_>, run: &Arc<Run>, script: &str) -> Result<String, String> {
    // A module per run, under a name of its own. `PyImport_ExecCodeModule` hands
    // back whatever `sys.modules` already holds under a name, so a shared name
    // would let two scripts share one module -- and one `_HOST`, which is the
    // whole per-run state. The name is then dropped again, because a module per
    // run that is never forgotten is a leak with a function object per tool in it.
    static RUNS: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "_shuvarie_codemode_{}",
        RUNS.fetch_add(1, Ordering::Relaxed)
    );
    let module_name = CString::new(name.as_str())
        .map_err(|_| "the codemode runtime name was malformed".to_string())?;

    let answer = load_and_call(py, &module_name, run, script);
    forget_module(py, &name);
    answer
}

/// Load the runtime as a module and let it run the script.
fn load_and_call(
    py: Python<'_>,
    module_name: &CStr,
    run: &Arc<Run>,
    script: &str,
) -> Result<String, String> {
    let module = PyModule::from_code(py, runtime_code(), c"codemode_runtime.py", module_name)
        .map_err(|err| format!("the codemode runtime did not load: {err}"))?;

    let host = Py::new(
        py,
        Host {
            run: Arc::clone(run),
        },
    )
    .map_err(|err| format!("the codemode runtime did not load: {err}"))?;
    module
        .setattr("_HOST", host)
        .map_err(|err| format!("the codemode runtime did not load: {err}"))?;

    // The catalog as the runtime needs it: the namespaces to build, the roster
    // name to dispatch on, and the dotted name to trace. Python resolves the rest.
    let manifest = json!({
        "tools": run
            .catalog
            .tools()
            .iter()
            .map(|spec| json!({
                "namespace": spec.namespace,
                "callable": spec.callable,
                "host": spec.host_name,
                "qualified": spec.qualified(),
                "description": spec.description,
            }))
            .collect::<Vec<_>>(),
        "all_tools": discovery::all_tools(&run.catalog),
        // A tool script calls nothing, so `store`/`load` only exist when a store
        // was handed over and the limits allow it.
        "store": run.limits.store && run.store.is_some(),
    })
    .to_string();
    let options = json!({ "max_operations": run.limits.max_operations.unwrap_or(0) }).to_string();

    module
        .getattr("start")
        .and_then(|start| start.call1((script, manifest, options)))
        .and_then(|envelope| envelope.extract::<String>())
        .map_err(|err| format!("the codemode runtime failed: {err}"))
}

/// Drop a run's module from `sys.modules`, so the interpreter keeps nothing of it.
fn forget_module(py: Python<'_>, name: &str) {
    let Ok(sys) = py.import("sys") else {
        return;
    };
    let Ok(modules) = sys.getattr("modules") else {
        return;
    };
    let _ = modules.call_method1("pop", (name, py.None()));
}

/// The embedded runtime, as the C string `PyModule::from_code` wants.
fn runtime_code() -> &'static CStr {
    static CODE: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
    CODE.get_or_init(|| CString::new(RUNTIME).expect("the embedded runtime holds no NUL bytes"))
}

/// The runtime's answer, as the types in this file.
#[derive(serde::Deserialize)]
struct Envelope {
    ok: bool,
    /// The script's value, still JSON: it never crosses as a Python object.
    #[serde(default)]
    value: Option<String>,
    /// `parse`, `limit`, `stopped`, or `error`.
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    column: Option<usize>,
}

/// Read the envelope into a result, keeping the values `text()` emitted.
fn read_envelope(envelope: &str, items: Vec<Value>) -> Result<ScriptValue, ScriptError> {
    let envelope: Envelope = serde_json::from_str(envelope).map_err(|err| ScriptError {
        kind: ErrorKind::Eval,
        message: format!("the codemode runtime answered with something else: {err}"),
        line: None,
        column: None,
    })?;

    if envelope.ok {
        return Ok(ScriptValue {
            items,
            // A value that is not JSON was encoded as its own repr, so a string is
            // the right fallback rather than a failure.
            value: envelope
                .value
                .map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text))),
        });
    }

    Err(ScriptError {
        kind: match envelope.kind.as_deref() {
            Some("parse") => ErrorKind::Parse,
            Some("limit") => ErrorKind::Limit,
            Some("stopped") => ErrorKind::Stopped,
            _ => ErrorKind::Eval,
        },
        message: envelope.message.unwrap_or_default(),
        line: envelope.line,
        column: envelope.column,
    })
}

/// What `search_tools` answers with: the hits, and how many matched, so a
/// search that had to stop says so instead of looking complete.
fn search(catalog: &Catalog, query: &str, namespace: Option<&str>, limit: usize) -> String {
    let (hits, matched) = discovery::search(catalog, query, namespace, limit);
    let tools: Vec<Value> = hits
        .iter()
        .map(|hit| json!({ "name": hit.name, "description": hit.description }))
        .collect();
    json!({ "matched": matched, "tools": tools }).to_string()
}

/// The error a stop reason becomes.
fn stopped(reason: StopReason, timeout: Option<Duration>) -> ScriptError {
    let message = match reason {
        StopReason::Cancelled => "the script was cancelled".to_string(),
        StopReason::TimedOut => match timeout {
            Some(timeout) => format!("the script ran past its {} budget", humantime(timeout)),
            None => "the script ran past its budget".to_string(),
        },
        StopReason::HostGone => "the host stopped responding".to_string(),
        StopReason::ToolLimit => "the script reached its tool-call limit".to_string(),
    };
    ScriptError {
        kind: match reason {
            StopReason::ToolLimit => ErrorKind::Limit,
            _ => ErrorKind::Stopped,
        },
        message,
        line: None,
        column: None,
    }
}

/// A duration as a person reads it.
pub fn humantime(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis < 1_000 {
        format!("{millis}ms")
    } else if millis < 60_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!(
            "{}m{:02}s",
            duration.as_secs() / 60,
            duration.as_secs() % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_envelope_carries_the_script_value() {
        let envelope = json!({ "ok": true, "value": "{\"count\": 2}" }).to_string();
        let value = read_envelope(&envelope, vec![json!("emitted")]).unwrap();
        assert_eq!(value.value, Some(json!({ "count": 2 })));
        assert_eq!(value.items, vec![json!("emitted")]);
    }

    #[test]
    fn a_script_with_no_value_still_succeeds() {
        let envelope = json!({ "ok": true, "value": null }).to_string();
        let value = read_envelope(&envelope, Vec::new()).unwrap();
        assert_eq!(value.value, None);
    }

    #[test]
    fn a_value_python_could_not_encode_arrives_as_its_repr() {
        // `_dumps` falls back to `repr` for anything JSON cannot hold, so the
        // envelope's value is a JSON string that decodes back to text.
        let envelope = json!({ "ok": true, "value": "\"<object at 0x1>\"" }).to_string();
        let value = read_envelope(&envelope, Vec::new()).unwrap();
        assert_eq!(value.value, Some(json!("<object at 0x1>")));
    }

    #[test]
    fn a_failure_envelope_names_its_kind_and_position() {
        let envelope = json!({
            "ok": false,
            "kind": "error",
            "message": "NameError: name 'nope' is not defined",
            "line": 4,
            "column": 9,
        })
        .to_string();
        let error = read_envelope(&envelope, Vec::new()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Eval);
        assert_eq!(error.message, "NameError: name 'nope' is not defined");
        assert_eq!((error.line, error.column), (Some(4), Some(9)));
    }

    #[test]
    fn a_parse_failure_is_its_own_kind() {
        let envelope =
            json!({ "ok": false, "kind": "parse", "message": "SyntaxError: bad" }).to_string();
        assert_eq!(
            read_envelope(&envelope, Vec::new()).unwrap_err().kind,
            ErrorKind::Parse
        );
    }

    #[test]
    fn a_stop_reason_outranks_whatever_the_script_said() {
        let error = stopped(StopReason::TimedOut, Some(Duration::from_millis(800)));
        assert_eq!(error.kind, ErrorKind::Stopped);
        assert_eq!(error.message, "the script ran past its 800ms budget");
        assert_eq!(stopped(StopReason::ToolLimit, None).kind, ErrorKind::Limit);
    }
}
