//! Running scripts end to end, against a stub roster: the bridge, the limits,
//! the globals, and the errors a script can and cannot catch.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use shuvarie_codemode::script::InvokeFuture;
use shuvarie_codemode::{
    Catalog, ErrorKind, Limits, ScriptRequest, Stop, Store, ToolCall, ToolEntry, ToolInvoker,
    block, callable_name, run,
};

/// A roster whose calls are canned, so the tests are about the script runtime
/// rather than about any tool.
#[derive(Default)]
struct Stub {
    /// Replies by roster name; a missing name is an error naming the tool, which
    /// is what a real invoker does with a name it does not know.
    replies: HashMap<String, Result<Value, String>>,
    calls: Mutex<Vec<ToolCall>>,
    calls_made: AtomicUsize,
}

impl Stub {
    fn with(replies: impl IntoIterator<Item = (&'static str, Value)>) -> Self {
        let mut stub = Self::default();
        for (name, reply) in replies {
            stub.replies.insert(name.to_string(), Ok(reply));
        }
        // A tool that always fails, for the tests about denials.
        stub.replies.insert(
            "deny".to_string(),
            Err("permission denied: deny".to_string()),
        );
        stub
    }

    fn calls(&self) -> Vec<ToolCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl ToolInvoker for Stub {
    fn invoke(&self, call: ToolCall) -> InvokeFuture<'_> {
        Box::pin(async move {
            self.calls_made.fetch_add(1, Ordering::Relaxed);
            self.calls.lock().unwrap().push(call.clone());
            match self.replies.get(&call.host_name) {
                Some(Ok(reply)) => Ok(reply.clone()),
                Some(Err(message)) => Err(message.clone()),
                None => Err(format!("no such tool: {}", call.host_name)),
            }
        })
    }
}

fn schema(properties: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The stub roster: roster tools and one MCP server's tool, including a name that
/// is not a Python identifier.
fn catalog() -> Arc<Catalog> {
    Arc::new(Catalog::new([
        ToolEntry::roster(
            "read_file",
            "Read a file.",
            schema(json!({ "path": { "type": "string" } }), json!(["path"])),
        ),
        ToolEntry::roster(
            "grep",
            "Search file contents.",
            schema(json!({ "pattern": { "type": "string" } }), json!([])),
        ),
        ToolEntry::roster("deny", "Always refused.", json!({})),
        ToolEntry::roster("todos", "The todo list.", json!({})),
        ToolEntry::mcp(
            "dev-radius",
            "dev_radius__resolve-library-id",
            "resolve-library-id",
            "Resolve a package.",
            schema(json!({ "name": { "type": "string" } }), json!([])),
        ),
    ]))
}

/// A roster whose `glob` answers with the given text, for the tests about what a
/// capped result looks like once a script has filtered it.
fn stub_with_glob(reply: String) -> Arc<Stub> {
    Arc::new(Stub::with([("glob", json!(reply))]))
}

/// The stub roster plus `glob`, which only those tests need.
fn glob_catalog() -> Arc<Catalog> {
    Arc::new(Catalog::new([ToolEntry::roster(
        "glob",
        "List matching paths.",
        schema(json!({ "pattern": { "type": "string" } }), json!([])),
    )]))
}

fn stub() -> Arc<Stub> {
    Arc::new(Stub::with([
        ("read_file", json!("fn main() {}\n")),
        ("grep", json!("src/main.rs:1:fn main() {}")),
        (
            "todos",
            json!([{ "id": 1, "text": "ship it", "done": false }]),
        ),
        (
            "dev_radius__resolve-library-id",
            json!({ "id": "/dev/radius" }),
        ),
    ]))
}

async fn execute_with(
    script: &str,
    limits: Limits,
    invoker: Arc<Stub>,
    catalog: Arc<Catalog>,
) -> (shuvarie_codemode::ScriptOutcome, Arc<Stub>) {
    let outcome = run(
        ScriptRequest {
            script: script.to_string(),
            catalog,
            store: None,
            limits,
        },
        invoker.clone(),
        Stop::new(),
    )
    .await;
    (outcome, invoker)
}

/// The stub with its canned replies, which the end-to-end tests need.
async fn execute_canned(script: &str) -> (shuvarie_codemode::ScriptOutcome, Arc<Stub>) {
    let invoker = stub();
    let outcome = run(
        ScriptRequest {
            script: script.to_string(),
            catalog: catalog(),
            store: None,
            limits: Limits::default(),
        },
        invoker.clone(),
        Stop::new(),
    )
    .await;
    (outcome, invoker)
}

fn value(outcome: &shuvarie_codemode::ScriptOutcome) -> Value {
    match &outcome.result {
        Ok(value) => value.value.clone().unwrap_or(Value::Null),
        Err(error) => panic!("script failed: {} {:?}", error.message, error.kind),
    }
}

fn error(outcome: &shuvarie_codemode::ScriptOutcome) -> &shuvarie_codemode::ScriptError {
    match &outcome.result {
        Ok(_) => panic!("script unexpectedly succeeded"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn a_script_calls_a_tool_and_returns_what_it_derived() {
    let (outcome, invoker) = execute_canned(
        r#"
        src = tools.read_file(path="src/main.rs")
        "has a main" if "fn main" in src else "no main"
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!("has a main"));
    assert_eq!(
        invoker.calls(),
        vec![ToolCall {
            host_name: "read_file".to_string(),
            args: json!({ "path": "src/main.rs" }),
        }]
    );
    assert_eq!(outcome.calls.len(), 1);
    assert_eq!(outcome.calls[0].qualified, "tools.read_file");
    assert!(outcome.calls[0].error.is_none());
    // The tool's output is a plain string, so `src` is one.
    assert_eq!(outcome.calls[0].args, r#"{"path":"src/main.rs"}"#);
}

#[tokio::test]
async fn arguments_and_results_cross_as_json() {
    let (outcome, invoker) = execute_canned(
        r#"
        todo = tools.todos()
        f"{len(todo)} todos, first is {todo[0]['text']}"
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!("1 todos, first is ship it"));
    // A tool called with no arguments sends an empty object, so the host's schema
    // still sees an object rather than nothing at all.
    assert_eq!(invoker.calls()[0].args, json!({}));
}

#[tokio::test]
async fn a_tool_takes_keyword_arguments_or_one_dict() {
    let (outcome, _) = execute_canned(
        r#"
        one = tools.grep(pattern="fn main")
        two = tools.grep(**{"pattern": "fn main"})
        three = tools.grep({"pattern": "fn main"})
        [one == two, two == three]
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!([true, true]));
}

#[tokio::test]
async fn a_positional_argument_that_is_not_a_dict_is_refused() {
    let (outcome, invoker) = execute_canned(r#"tools.grep("fn main")"#).await;

    let error = error(&outcome);
    assert!(
        error.message.contains("dict of arguments"),
        "{}",
        error.message
    );
    // Refused before it reaches the host.
    assert_eq!(invoker.calls().len(), 0);
    assert_eq!(error.line, Some(1));
}

#[tokio::test]
async fn renamed_mcp_tools_are_callable_and_dispatch_to_the_roster_name() {
    let (outcome, invoker) =
        execute_canned(r#"mcp.dev_radius.resolve_library_id(name="radius")"#).await;

    assert_eq!(value(&outcome), json!({ "id": "/dev/radius" }));
    assert_eq!(
        invoker.calls()[0].host_name,
        "dev_radius__resolve-library-id"
    );
    assert_eq!(
        outcome.calls[0].qualified,
        "mcp.dev_radius.resolve_library_id"
    );
}

#[tokio::test]
async fn text_items_and_console_output_are_collected() {
    let (outcome, _) = execute_canned(
        r#"
        console.log("starting")
        text("first")
        console.warn("careful", 2)
        print("printed")
        text({"done": True})
        42
        "#,
    )
    .await;

    let value = match &outcome.result {
        Ok(value) => value,
        Err(error) => panic!("{}", error.message),
    };
    assert_eq!(value.items, vec![json!("first"), json!({ "done": true })]);
    assert_eq!(value.value, Some(json!(42)));
    // `print` is the other way to the same place: a script that forgets
    // `console.log` still reports what it wanted to say.
    assert_eq!(outcome.logs, vec!["starting", "careful 2", "printed"]);
}

#[tokio::test]
async fn a_denied_call_is_catchable_and_recorded() {
    let (outcome, _) = execute_canned(
        r#"
        try:
            out = tools.deny()
        except ToolError as failure:
            out = str(failure)
        out
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!("permission denied: deny"));
    assert_eq!(outcome.calls.len(), 1);
    assert_eq!(
        outcome.calls[0].error.as_deref(),
        Some("permission denied: deny")
    );
}

#[tokio::test]
async fn an_uncaught_denial_is_the_scripts_failure() {
    let (outcome, _) = execute_canned("tools.deny()").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Eval);
    // The tool's own sentence, not a wrapper's: this is what the model has to act
    // on.
    assert_eq!(error.message, "permission denied: deny");
    // The call still happened, and the trace says how it went.
    assert_eq!(outcome.calls.len(), 1);
}

#[tokio::test]
async fn a_script_cannot_call_a_tool_the_catalog_does_not_hold() {
    let (outcome, _) = execute_canned("tools.not_a_tool()").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Eval);
    assert!(error.message.contains("not_a_tool"), "{}", error.message);
    assert_eq!(error.line, Some(1));
}

#[tokio::test]
async fn a_parse_error_points_at_the_line() {
    let (outcome, _) = execute_canned("x = 1\ny = )").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.line, Some(2));
}

#[tokio::test]
async fn exit_ends_the_script_with_its_value() {
    let (outcome, _) = execute_canned(r#"text("before"); exit({"stopped": True}); "after""#).await;
    assert_eq!(value(&outcome), json!({ "stopped": true }));
    assert_eq!(
        match &outcome.result {
            Ok(value) => value.items.clone(),
            Err(_) => Vec::new(),
        },
        vec![json!("before")]
    );
}

#[tokio::test]
async fn a_script_with_no_last_expression_has_no_value() {
    // Statements only: there is nothing to return, which is not a failure.
    let (outcome, _) = execute_canned("x = 1").await;
    match &outcome.result {
        Ok(value) => assert_eq!(value.value, None),
        Err(error) => panic!("{}", error.message),
    }
}

#[tokio::test]
async fn a_missing_dict_key_is_the_scripts_own_error() {
    // `dict.get` is the quiet way and a subscript is the loud one; the point of the
    // test is that the loud one comes back with a position rather than silence.
    let (quiet, _) = execute_canned(
        r#"
        todos = tools.todos()
        "silent" if todos[0].get("typo") is None else "loud"
        "#,
    )
    .await;
    assert_eq!(value(&quiet), json!("silent"));

    let (loud, _) = execute_canned("todos = tools.todos()\ntodos[0][\"typo\"]").await;
    let error = error(&loud);
    assert_eq!(error.kind, ErrorKind::Eval);
    assert!(error.message.contains("KeyError"), "{}", error.message);
    assert_eq!(error.line, Some(2));
}

#[tokio::test]
async fn the_operation_limit_stops_a_runaway_script() {
    let limits = Limits {
        max_operations: Some(10_000),
        ..Limits::default()
    };
    let (outcome, _) =
        execute_with("x = 0\nwhile True:\n    x += 1", limits, stub(), catalog()).await;

    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Limit);
    assert!(error.message.contains("operations"), "{}", error.message);
}

#[tokio::test]
async fn the_timeout_stops_a_runaway_script() {
    let limits = Limits {
        timeout: Some(Duration::from_millis(50)),
        ..Limits::default()
    };
    let (outcome, _) = execute_with("while True: pass", limits, stub(), catalog()).await;

    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Stopped);
    assert!(error.message.contains("budget"), "{}", error.message);
}

#[tokio::test]
async fn the_tool_call_limit_stops_the_script_even_when_the_script_catches_it() {
    let limits = Limits {
        max_tool_calls: Some(2),
        ..Limits::default()
    };
    let (outcome, invoker) = execute_with(
        r#"
        n = 0
        while n < 5:
            try:
                tools.todos()
            except ToolError as failure:
                console.log(f"caught: {failure}")
            n += 1
        n
        "#,
        limits,
        stub(),
        catalog(),
    )
    .await;

    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Limit);
    assert!(
        error.message.contains("tool-call limit"),
        "{}",
        error.message
    );
    // A caught limit is still a limit: the script does not get to keep going. The
    // third call is refused before it is dispatched, so the host never sees it and
    // the trace counts only the calls that happened.
    assert_eq!(invoker.calls().len(), 2);
    assert_eq!(outcome.calls.len(), 2);
}

#[tokio::test]
async fn a_cancel_stops_the_script() {
    let stop = Stop::new();
    let invoker = stub();
    let handle = tokio::spawn(run(
        ScriptRequest {
            script: "while True:\n    pass".to_string(),
            catalog: catalog(),
            store: None,
            limits: Limits::default(),
        },
        invoker,
        stop.clone(),
    ));

    tokio::time::sleep(Duration::from_millis(50)).await;
    stop.cancel();

    let outcome = handle.await.unwrap();
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Stopped);
    assert!(error.message.contains("cancelled"), "{}", error.message);
}

/// Two scripts can be in flight at once -- two tabs, one turn each -- and the
/// console proxy is process-wide, so this is the test that keeps a script's
/// `print` from landing in another's result.
#[tokio::test]
async fn two_scripts_at_once_keep_their_own_console_output() {
    let first = tokio::spawn(run(
        ScriptRequest {
            script:
                "import time\nprint(\"alpha\")\ntime.sleep(0.05)\nprint(\"alpha again\")\n\"alpha\""
                    .to_string(),
            catalog: catalog(),
            store: None,
            limits: Limits::default(),
        },
        stub(),
        Stop::new(),
    ));
    let second = tokio::spawn(run(
        ScriptRequest {
            script: "import time\ntime.sleep(0.02)\nprint(\"beta\")\n\"beta\"".to_string(),
            catalog: catalog(),
            store: None,
            limits: Limits::default(),
        },
        stub(),
        Stop::new(),
    ));

    let (first, second) = (first.await.unwrap(), second.await.unwrap());
    assert_eq!(
        first.logs,
        vec!["alpha", "alpha again"],
        "{:?}",
        first.result
    );
    assert_eq!(second.logs, vec!["beta"], "{:?}", second.result);
    assert_eq!(value(&first), json!("alpha"));
    assert_eq!(value(&second), json!("beta"));
}

#[tokio::test]
async fn the_store_survives_between_scripts() {
    let store = Arc::new(Store::new());
    let limits = Limits::default();

    let first = run(
        ScriptRequest {
            script: r#"store("seen", 41); 0"#.to_string(),
            catalog: catalog(),
            store: Some(Arc::clone(&store)),
            limits: limits.clone(),
        },
        stub(),
        Stop::new(),
    )
    .await;
    assert!(first.result.is_ok(), "{:?}", first.result);

    let second = run(
        ScriptRequest {
            script: r#"load("seen") + 1"#.to_string(),
            catalog: catalog(),
            store: Some(store),
            limits,
        },
        stub(),
        Stop::new(),
    )
    .await;
    assert_eq!(value(&second), json!(42));
}

#[tokio::test]
async fn a_key_that_was_never_stored_reads_as_none() {
    let outcome = run(
        ScriptRequest {
            script: r#""empty" if load("never") is None else "set""#.to_string(),
            catalog: catalog(),
            store: Some(Arc::new(Store::new())),
            limits: Limits::default(),
        },
        stub(),
        Stop::new(),
    )
    .await;
    assert_eq!(value(&outcome), json!("empty"));
}

#[tokio::test]
async fn the_store_can_be_turned_off() {
    let outcome = run(
        ScriptRequest {
            script: r#"store("k", 1)"#.to_string(),
            catalog: catalog(),
            store: Some(Arc::new(Store::new())),
            limits: Limits {
                store: false,
                ..Limits::default()
            },
        },
        stub(),
        Stop::new(),
    )
    .await;
    let error = error(&outcome);
    assert!(error.message.contains("store"), "{}", error.message);
}

#[tokio::test]
async fn an_empty_roster_runs_a_script_that_needs_no_tools() {
    let outcome = run(
        ScriptRequest {
            script: "1 + 1".to_string(),
            catalog: Arc::new(Catalog::default()),
            store: None,
            limits: Limits::default(),
        },
        stub(),
        Stop::new(),
    )
    .await;
    assert_eq!(value(&outcome), json!(2));
}

#[tokio::test]
async fn the_result_block_reads_back_the_whole_run() {
    let (outcome, _) = execute_canned(
        r#"
        src = tools.read_file(path="src/main.rs")
        console.log("read it")
        f"{len(src)} characters"
        "#,
    )
    .await;

    let rendered = block(&outcome, 20_000);
    assert!(rendered.starts_with("Script completed in "), "{rendered}");
    assert!(rendered.contains("(1 tool call)."), "{rendered}");
    assert!(rendered.contains("\n13 characters\n"), "{rendered}");
    assert!(
        rendered.contains("<console_output>\nread it\n</console_output>"),
        "{rendered}"
    );
    assert!(rendered.contains("1. tools.read_file — "), "{rendered}");
    assert!(rendered.contains(r#"{"path":"src/main.rs"}"#), "{rendered}");
}

/// The discovery globals exist so a script can reach a tool the description did
/// not inline, so what matters is that they answer from the runtime -- with the
/// catalog's names, and without spending a tool call to do it.
#[tokio::test]
async fn search_tools_answers_from_the_catalog() {
    let (outcome, invoker) = execute_canned(r#"search_tools("read")"#).await;

    assert_eq!(
        value(&outcome),
        json!({
            "matched": 1,
            "tools": [{ "name": "tools.read_file", "description": "Read a file." }],
        })
    );
    // Reaching for a tool is not calling one.
    assert_eq!(invoker.calls_made.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn search_tools_takes_a_limit_and_a_namespace() {
    let (outcome, _) = execute_canned(
        r#"
        wide = search_tools("", limit=2)
        scoped = search_tools("", namespace="mcp")
        [len(wide["tools"]), wide["matched"], len(scoped["tools"])]
        "#,
    )
    .await;

    // The limit is applied after matching, so `matched` still counts them all, and
    // a namespace filter sees only that namespace.
    assert_eq!(value(&outcome), json!([2, 5, 1]));
}

#[tokio::test]
async fn all_tools_is_the_roster_a_script_can_filter() {
    let (outcome, _) = execute_canned(
        r#"
        mcp_tools = [name for name in ALL_TOOLS if name.startswith("mcp.")]
        f"all: {len(ALL_TOOLS)}, mcp: {len(mcp_tools)}, one: {mcp_tools[0]}"
        "#,
    )
    .await;

    assert_eq!(
        value(&outcome),
        json!("all: 5, mcp: 1, one: mcp.dev_radius.resolve_library_id")
    );
}

#[tokio::test]
async fn describe_tool_spells_out_a_signature() {
    let (outcome, _) = execute_canned(r#"describe_tool("read_file")"#).await;

    let described = value(&outcome);
    let described = described.as_str().unwrap();
    assert_eq!(
        described,
        "tools.read_file(path: str) -> Any\nRead a file.\n\npath (required): str"
    );
}

#[tokio::test]
async fn describe_namespace_lists_a_server() {
    let (outcome, _) = execute_canned(r#"describe_namespace("mcp")"#).await;

    let described = value(&outcome);
    let described = described.as_str().unwrap();
    assert!(described.starts_with("mcp (1 tool)"), "{described}");
    assert!(
        described.contains("mcp.dev_radius.resolve_library_id(name: str | None = None) -> Any"),
        "{described}"
    );
}

/// A name nobody knows is a *lookup error*: the model gets a message with the
/// near-misses, and a script that wants to survive a bad name can catch it.
#[tokio::test]
async fn describe_tool_reports_an_unknown_name_and_a_script_can_catch_it() {
    let (outcome, _) = execute_canned(
        r#"
        try:
            out = describe_tool("read_fil")
        except LookupError as failure:
            out = str(failure)
        out
        "#,
    )
    .await;

    let message = value(&outcome);
    let message = message.as_str().unwrap();
    assert!(message.contains("did you mean"), "{message}");
    assert!(message.contains("tools.read_file"), "{message}");
}

/// The catalog the globals answer from is the one the request carried, so a tool
/// that is not in it is not offered by any route.
#[tokio::test]
async fn the_discovery_globals_only_see_this_scripts_catalog() {
    let invoker = stub();
    let outcome = run(
        ScriptRequest {
            script: r#"search_tools("write_file", limit=5)"#.to_string(),
            catalog: catalog(),
            store: None,
            limits: Limits::default(),
        },
        invoker,
        Stop::new(),
    )
    .await;

    assert_eq!(
        value(&outcome),
        json!({ "matched": 0, "tools": [] }),
        "a tool outside the catalog is unknown"
    );
}

/// A capped `glob` returns a note on its own line, and the filter that makes the
/// rest of the output usable drops exactly that line — so the count comes back as
/// a clean `100` with nothing to say it is partial.
///
/// This is the shape of a real miss: a session tallied per-crate file counts, three
/// crates came back as exactly 100, and the summary was wrong. The guard the
/// documents teach is checked here, because the prose is only true if `in` still
/// sees the note after the filter has run.
#[tokio::test]
async fn a_truncated_glob_counts_as_a_clean_number_unless_checked() {
    let mut capped = String::new();
    for index in 0..100 {
        capped.push_str(&format!("crates/core/src/file_{index}.rs\n"));
    }
    capped.push_str(
        "\n\n(Results are truncated: showing first 100 results. Consider using a more specific \
         path or pattern.)",
    );

    let invoker = stub_with_glob(capped);
    let outcome = run(
        ScriptRequest {
            script: r#"
found = tools.glob(pattern="crates/*/src/**")
files = [line for line in found.split("\n") if line.endswith(".rs")]
f"partial: {len(files)}" if "truncated" in found else f"{len(files)}"
"#
            .to_string(),
            catalog: glob_catalog(),
            store: None,
            limits: Limits::default(),
        },
        invoker,
        Stop::new(),
    )
    .await;

    // The filter alone would have said `100`; the guard is what makes it honest.
    assert_eq!(value(&outcome), json!("partial: 100"));
}

/// The same tally without the guard, to pin the trap itself: a filter that looks
/// for paths cannot distinguish "100 matches" from "100 of many".
#[tokio::test]
async fn an_unchecked_truncated_glob_lies_about_its_count() {
    let mut capped = String::new();
    for index in 0..100 {
        capped.push_str(&format!("crates/core/src/file_{index}.rs\n"));
    }
    capped.push_str("\n\n(Results are truncated: showing first 100 results.)");

    let outcome = run(
        ScriptRequest {
            script: r#"
len([line for line in tools.glob(pattern="crates/*/src/**").split("\n") if line.endswith(".rs")])
"#
            .to_string(),
            catalog: glob_catalog(),
            store: None,
            limits: Limits::default(),
        },
        stub_with_glob(capped),
        Stop::new(),
    )
    .await;

    assert_eq!(value(&outcome), json!(100), "the count carries no warning");
}

/// A tool name that collides with a Python keyword is the one case the mangling
/// exists for, and the fix is worth pinning end to end: the catalog prints the
/// name, and the printed name is callable.
#[tokio::test]
async fn a_tool_named_after_a_keyword_is_callable_under_its_mangled_name() {
    let invoker = Arc::new(Stub::with([("class", json!("a class tool"))]));
    let catalog = Arc::new(Catalog::new([ToolEntry::roster(
        "class",
        "Named after a keyword.",
        json!({}),
    )]));
    let qualified = catalog.tools()[0].qualified();
    assert_eq!(qualified, "tools._class");

    let outcome = run(
        ScriptRequest {
            script: format!("{qualified}()"),
            catalog,
            store: None,
            limits: Limits::default(),
        },
        invoker,
        Stop::new(),
    )
    .await;
    assert_eq!(value(&outcome), json!("a class tool"));
}

/// The mangling is only right if it is exactly Python's keywords: a missing one is
/// a tool the model cannot call at all. The list in `spec.rs` is therefore checked
/// against the interpreter that will actually run the script.
#[tokio::test]
async fn every_python_keyword_is_mangled() {
    let (outcome, _) = execute_canned("import keyword\nsorted(keyword.kwlist)").await;
    let keywords: Vec<String> = serde_json::from_value(value(&outcome)).unwrap();

    for keyword in &keywords {
        assert_ne!(
            callable_name(keyword),
            *keyword,
            "'{keyword}' is a Python keyword and stayed unmangled: a tool by that name \
             could not be called"
        );
    }
    // A soft keyword is a perfectly good attribute name, so it is left alone.
    assert_eq!(callable_name("match"), "match");
    assert_eq!(callable_name("type"), "type");
}
