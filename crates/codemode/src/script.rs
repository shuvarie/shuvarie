//! Running a script: the engine, its limits, and the thread bridge.
//!
//! Rhai has no `async`, so a tool called from a script cannot be awaited. The
//! pattern is the one Rhai's own book documents for async host APIs: the engine
//! lives on a dedicated thread, every tool is a registered closure that sends a
//! request over a channel and **blocks** on the reply, and a task on the async
//! side performs the real call. The engine therefore never crosses a thread
//! boundary, Rhai's `sync` feature stays off, and -- the reason it is worth a
//! thread -- a nested call is the *real* tool call, running through the same
//! permission checks and the same result plumbing as a direct one.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rhai::{Dynamic, Engine, EvalAltResult, FuncRegistration, Module, Scope};
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::convert::{self, from_dynamic, to_dynamic};
use crate::error::to_runtime_error;
use crate::spec::Catalog;
use crate::{DEFAULT_NAMESPACE_LIMIT, DEFAULT_SEARCH_LIMIT, discovery};

/// Engine limits that exist for safety rather than configuration: a script is
/// not supposed to hit them, and one that does is a runaway rather than a user
/// preference.
const MAX_CALL_LEVELS: usize = 64;
const MAX_EXPR_DEPTH: usize = 64;
const MAX_STRING_SIZE: usize = 1 << 20;
const MAX_ARRAY_SIZE: usize = 100_000;
const MAX_MAP_SIZE: usize = 10_000;
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
    /// Rhai operations. `None` is unlimited.
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

/// The stop signal, shared by the caller and the script thread.
///
/// One token per run: the caller flips it (a turn abort), the script thread
/// reads it from `on_progress`, and dropping [`run`]'s future flips it too, so
/// a script never outlives the call that started it. The first reason to arrive
/// wins, which is why the state is a compare-and-swap rather than a flag.
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
    /// Rhai's position, when the error has one (not every error does).
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
    /// does: a panic in it, or an engine that could not be built.
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
    tokio::spawn(serve_calls(incoming, invoker));

    let (finished, outcome) = tokio::sync::oneshot::channel::<ScriptOutcome>();
    // The engine is built, compiled and evaluated here, and never leaves.
    let script_stop = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("codemode-script".to_string())
        .spawn(move || {
            let outcome = evaluate(request, requests, script_stop.clone(), started);
            // The receiver may be gone (the caller dropped the run); the result
            // is then nobody's to read, which is not an error.
            let _ = finished.send(outcome);
        });

    if spawned.is_err() {
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
    // the script thread checks.
    let guard = StopOnDrop(stop);
    match outcome.await {
        Ok(outcome) => {
            drop(guard);
            outcome
        }
        Err(_) => {
            drop(guard);
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
async fn serve_calls(mut requests: UnboundedReceiver<CallRequest>, invoker: Arc<dyn ToolInvoker>) {
    while let Some(request) = requests.recv().await {
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

/// The `console` object, because `console.log(...)` is the one piece of
/// JavaScript the model will reach for without being told. Rhai has no member
/// access into a static module (`console::log(..)` resolves, `console.log(..)`
/// does not), so this is a real value registered as a type and pushed into the
/// script's scope -- which is what makes the familiar form work.
#[derive(Clone)]
struct Console {
    sink: Arc<Mutex<Collected>>,
}

impl Console {
    fn write(&self, values: &[Dynamic]) {
        let line = values.iter().map(describe).collect::<Vec<_>>().join(" ");
        collect_line(&self.sink, &line);
    }
}

/// The script thread: build the engine, compile, run, report.
fn evaluate(
    request: ScriptRequest,
    requests: UnboundedSender<CallRequest>,
    stop: Stop,
    started: Instant,
) -> ScriptOutcome {
    let collected = Arc::new(Mutex::new(Collected::default()));
    let engine = build_engine(&request, &requests, &stop, &collected, started);
    let mut scope = Scope::new();
    scope.push(
        "console",
        Console {
            sink: Arc::clone(&collected),
        },
    );
    // `ALL_TOOLS` is a value rather than a function: when the roster is too big
    // to inline, the cheapest way for a script to find something is to filter
    // the names itself.
    let all_tools: rhai::Array = discovery::all_tools(&request.catalog)
        .into_iter()
        .map(Dynamic::from)
        .collect();
    scope.push("ALL_TOOLS", all_tools);

    let script = request.script;
    let ast = match engine.compile(&script) {
        Ok(ast) => ast,
        Err(err) => {
            let position = err.position();
            return ScriptOutcome {
                result: Err(ScriptError {
                    kind: ErrorKind::Parse,
                    message: err.to_string(),
                    line: position.line(),
                    column: position.position(),
                }),
                logs: Vec::new(),
                calls: Vec::new(),
                elapsed: started.elapsed(),
                dropped: 0,
            };
        }
    };

    let evaluated = engine.eval_ast_with_scope::<Dynamic>(&mut scope, &ast);
    let elapsed = started.elapsed();

    let mut collected = collected.lock().unwrap();
    let Collected {
        items,
        logs,
        calls,
        dropped,
        ..
    } = std::mem::take(&mut *collected);
    drop(collected);

    // A stop reason outranks the error that carried it out: the script's own
    // `catch` may have swallowed the call that hit the limit.
    let result = match stop.reason() {
        Some(reason) => Err(stopped(reason, request.limits.timeout)),
        None => match evaluated {
            Ok(value) => Ok(ScriptValue {
                items,
                value: (!value.is_unit()).then(|| from_dynamic(&value).unwrap_or(Value::Null)),
            }),
            Err(err) => Err(classify(&err)),
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

/// Build the engine a script runs on: limits first, then the surface it may
/// touch.
fn build_engine(
    request: &ScriptRequest,
    requests: &UnboundedSender<CallRequest>,
    stop: &Stop,
    collected: &Arc<Mutex<Collected>>,
    started: Instant,
) -> Engine {
    let limits = &request.limits;
    let deadline = limits.timeout.map(|timeout| started + timeout);

    let mut engine = Engine::new();
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.set_max_expr_depths(MAX_EXPR_DEPTH, MAX_EXPR_DEPTH / 2);
    engine.set_max_string_size(MAX_STRING_SIZE);
    engine.set_max_array_size(MAX_ARRAY_SIZE);
    engine.set_max_map_size(MAX_MAP_SIZE);
    if let Some(operations) = limits.max_operations {
        engine.set_max_operations(operations);
    }

    // The interrupt: a Rhai callback cannot raise, so returning a value is how
    // a script is stopped, and the reason is read back from the signal.
    let interrupt = stop.clone();
    engine.on_progress(move |_| {
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            interrupt.timed_out();
        }
        interrupt.reason().is_some().then_some(Dynamic::UNIT)
    });

    // Console output is captured, not streamed: it belongs to the result.
    let sink = Arc::clone(collected);
    engine.on_print(move |line| collect_line(&sink, line));
    let sink = Arc::clone(collected);
    engine.on_debug(move |line, _source, _position| collect_line(&sink, line));

    register_globals(&mut engine, request, collected);

    for namespace in request.catalog.namespaces() {
        let mut module = Module::new();
        for spec in request.catalog.in_namespace(&namespace) {
            let call = ToolCallTarget {
                qualified: spec.qualified(),
                host_name: spec.host_name.clone(),
                request: requests.clone(),
                stop: stop.clone(),
                collected: Arc::clone(collected),
                max_tool_calls: request.limits.max_tool_calls,
            };
            register_tool(&mut module, spec.callable.clone(), call);
        }
        engine.register_static_module(namespace.join("::"), module.into());
    }

    engine
}

/// Register one tool as two arities: the map argument the card teaches, and
/// none at all, so a tool whose arguments are all optional can be called
/// plainly.
fn register_tool(module: &mut Module, callable: String, target: ToolCallTarget) {
    let no_arguments = target.clone();
    let _ = FuncRegistration::new(callable.clone())
        .in_internal_namespace()
        .with_purity(false)
        .with_volatility(true)
        .set_into_module(module, move || {
            no_arguments.call(Value::Object(Default::default()))
        });

    let _ = FuncRegistration::new(callable)
        .in_internal_namespace()
        .with_purity(false)
        .with_volatility(true)
        .set_into_module(module, move |args: Dynamic| {
            let args =
                from_dynamic(&args).map_err(|err| to_runtime_error(format!("arguments: {err}")))?;
            target.call(args)
        });
}

/// One callable tool, as the script thread sees it: everything a call needs
/// except the arguments.
#[derive(Clone)]
struct ToolCallTarget {
    qualified: String,
    host_name: String,
    request: UnboundedSender<CallRequest>,
    stop: Stop,
    collected: Arc<Mutex<Collected>>,
    max_tool_calls: Option<usize>,
}

impl ToolCallTarget {
    fn call(&self, args: Value) -> Result<Dynamic, Box<EvalAltResult>> {
        let preview = convert::preview(&args, 160);

        // Counted before the call so the trace of the call that hit the limit
        // is still recorded and the script stops cleanly at the next step.
        let over_limit = {
            let mut collected = self.collected.lock().unwrap();
            collected.tool_calls += 1;
            self.max_tool_calls
                .is_some_and(|max| collected.tool_calls > max)
        };
        if over_limit {
            self.stop.tool_limit();
            let max = self.max_tool_calls.unwrap_or_default();
            return Err(to_runtime_error(format!(
                "the limit of {max} tool calls for one script was reached at {}",
                self.qualified
            )));
        }

        let started = Instant::now();
        let (reply, answer) = tokio::sync::oneshot::channel();
        let sent = self.request.send(CallRequest {
            host_name: self.host_name.clone(),
            args,
            reply,
        });
        let outcome = match sent {
            Err(_) => {
                self.stop.host_gone();
                Err("the host is no longer accepting calls".to_string())
            }
            Ok(()) => match answer.blocking_recv() {
                // The reply channel closes only when the host side is gone.
                Ok(reply) => reply,
                Err(_) => {
                    self.stop.host_gone();
                    Err("the host stopped before the call finished".to_string())
                }
            },
        };
        let duration = started.elapsed();

        let mut collected = self.collected.lock().unwrap();
        collected.calls.push(CallTrace {
            qualified: self.qualified.clone(),
            host_name: self.host_name.clone(),
            args: preview,
            duration,
            error: outcome.as_ref().err().cloned(),
        });
        let result = outcome?;
        drop(collected);

        to_dynamic(&result).map_err(|err| to_runtime_error(format!("result: {err}")))
    }
}

/// The globals a script may call, beyond the tools.
fn register_globals(
    engine: &mut Engine,
    request: &ScriptRequest,
    collected: &Arc<Mutex<Collected>>,
) {
    let sink = Arc::clone(collected);
    engine.register_fn("text", move |value: Dynamic| {
        let mut collected = sink.lock().unwrap();
        if collected.items.len() < MAX_COLLECTED {
            collected
                .items
                .push(from_dynamic(&value).unwrap_or(Value::Null));
        } else {
            collected.dropped += 1;
        }
    });

    // `console.log(...)` and friends, as an object in the script's scope.
    engine.register_type_with_name::<Console>("Console");
    for level in ["log", "info", "warn", "error", "debug"] {
        engine.register_fn(level, |console: &mut Console, value: Dynamic| {
            console.write(&[value]);
        });
        engine.register_fn(
            level,
            |console: &mut Console, first: Dynamic, second: Dynamic| {
                console.write(&[first, second]);
            },
        );
        engine.register_fn(
            level,
            |console: &mut Console, first: Dynamic, second: Dynamic, third: Dynamic| {
                console.write(&[first, second, third]);
            },
        );
    }

    // Discovery: the tools the description did not have room to inline, and
    // the names a model half-remembers. Both take the whole catalog, so an
    // answer is always about what this script may actually call.
    let catalog = Arc::clone(&request.catalog);
    engine.register_fn("search_tools", move |query: &str| {
        search(&catalog, query, None, DEFAULT_SEARCH_LIMIT)
    });

    let catalog = Arc::clone(&request.catalog);
    engine.register_fn("search_tools", move |query: &str, options: rhai::Map| {
        let limit = options
            .get("limit")
            .and_then(|value| value.as_int().ok())
            .map(|limit| limit.max(0) as usize)
            .unwrap_or(DEFAULT_SEARCH_LIMIT);
        let namespace = options
            .get("namespace")
            .and_then(|value| value.clone().try_cast::<rhai::ImmutableString>());
        search(&catalog, query, namespace.as_deref(), limit)
    });

    let catalog = Arc::clone(&request.catalog);
    engine.register_fn("describe_tool", move |name: &str| {
        discovery::describe_tool(&catalog, name).map_err(to_runtime_error)
    });

    let catalog = Arc::clone(&request.catalog);
    engine.register_fn("describe_namespace", move |namespace: &str| {
        discovery::describe_namespace(&catalog, namespace, DEFAULT_NAMESPACE_LIMIT)
            .map_err(to_runtime_error)
    });

    if !request.limits.store {
        return;
    }
    let Some(store) = request.store.as_ref().map(Arc::clone) else {
        return;
    };
    let sink = store.clone();
    engine.register_fn(
        "store",
        move |key: rhai::ImmutableString, value: Dynamic| {
            sink.set(&key, from_dynamic(&value).unwrap_or(Value::Null));
        },
    );
    engine.register_fn("load", move |key: rhai::ImmutableString| -> Dynamic {
        match store.get(&key) {
            Some(value) => to_dynamic(&value).unwrap_or(Dynamic::UNIT),
            // A key that was never stored reads as `()`, like a missing map
            // key -- one rule, not two.
            None => Dynamic::UNIT,
        }
    });
}

/// What `search_tools` answers with: the hits, and how many matched, so a
/// search that had to stop says so instead of looking complete.
fn search(
    catalog: &Catalog,
    query: &str,
    namespace: Option<&str>,
    limit: usize,
) -> Result<Dynamic, Box<EvalAltResult>> {
    let (hits, matched) = discovery::search(catalog, query, namespace, limit);
    let tools: Vec<Value> = hits
        .iter()
        .map(|hit| serde_json::json!({ "name": hit.name, "description": hit.description }))
        .collect();
    let answer = serde_json::json!({ "matched": matched, "tools": tools });
    to_dynamic(&answer).map_err(|err| to_runtime_error(format!("search results: {err}")))
}

/// A value as console output shows it: strings bare, everything else as JSON.
fn describe(value: &Dynamic) -> String {
    match value.clone().try_cast::<rhai::ImmutableString>() {
        Some(text) => text.to_string(),
        None => from_dynamic(value)
            .map(|value| match value {
                Value::String(text) => text,
                other => other.to_string(),
            })
            .unwrap_or_else(|_| value.to_string()),
    }
}

fn collect_line(sink: &Arc<Mutex<Collected>>, line: &str) {
    let mut collected = sink.lock().unwrap();
    if collected.logs.len() < MAX_COLLECTED {
        collected.logs.push(line.to_string());
    } else {
        collected.dropped += 1;
    }
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

/// Rhai's error, as much of it as the result block reports.
///
/// Rhai wraps an error raised inside a script function in
/// `ErrorInFunctionCall`, so the innermost cause is the one worth showing --
/// and the outermost position is the one that points at the call site. Both
/// are used.
fn classify(error: &EvalAltResult) -> ScriptError {
    let position = error.position();
    let (kind, message) = match error {
        EvalAltResult::ErrorTooManyOperations(..) => (
            ErrorKind::Limit,
            "the script reached its operation limit".to_string(),
        ),
        EvalAltResult::ErrorDataTooLarge(what, ..) => (
            ErrorKind::Limit,
            format!("a {what} grew past the size limit"),
        ),
        EvalAltResult::ErrorStackOverflow(..) => (
            ErrorKind::Limit,
            "the script recursed past the call-depth limit".to_string(),
        ),
        // The payload is the message: `Display` would prefix it, and a host
        // failure raised this way is already a whole sentence.
        EvalAltResult::ErrorRuntime(payload, ..) => (ErrorKind::Eval, describe(payload)),
        // A failure inside a script function: report the cause, not the frame.
        EvalAltResult::ErrorInFunctionCall(name, _, cause, ..) => {
            let cause = classify(cause);
            (cause.kind, format!("in `{name}`: {}", cause.message))
        }
        other => (
            ErrorKind::Eval,
            match other {
                EvalAltResult::ErrorTerminated(..) => {
                    "the script was stopped before it finished".to_string()
                }
                _ => other.to_string(),
            },
        ),
    };

    ScriptError {
        kind,
        message,
        line: position.line(),
        column: position.position(),
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
