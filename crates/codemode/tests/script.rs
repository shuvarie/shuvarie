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
    block, run,
};

/// A roster whose calls are canned, so the tests are about the script engine
/// rather than about any tool.
#[derive(Default)]
struct Stub {
    /// Replies by roster name; a missing name is an error naming the tool,
    /// which is what a real invoker does with a name it does not know.
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

/// The stub roster: two roster tools and one MCP server's tool, including a
/// name that is not a Rhai identifier.
fn catalog() -> Arc<Catalog> {
    Arc::new(Catalog::new([
        ToolEntry::roster(
            "read_file",
            "Read a file.",
            json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
        ),
        ToolEntry::roster(
            "grep",
            "Search file contents.",
            json!({ "type": "object", "properties": { "pattern": { "type": "string" } } }),
        ),
        ToolEntry::roster("deny", "Always refused.", json!({})),
        ToolEntry::roster("todos", "The todo list.", json!({})),
        ToolEntry::mcp(
            "dev-radius",
            "dev_radius__resolve-library-id",
            "resolve-library-id",
            "Resolve a package.",
            json!({ "type": "object", "properties": { "name": { "type": "string" } } }),
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
        json!({ "type": "object", "properties": { "pattern": { "type": "string" } } }),
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
        Err(error) => panic!("script failed: {} {}", error.message, error.kind as u8),
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
        let src = tools::read_file(#{ path: "src/main.rs" });
        if src.contains("fn main") { "has a main" } else { "no main" }
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
    assert_eq!(outcome.calls[0].qualified, "tools::read_file");
    assert!(outcome.calls[0].error.is_none());
    // The tool's output is a plain string, so `src` is one.
    assert_eq!(outcome.calls[0].args, r#"{"path":"src/main.rs"}"#);
}

#[tokio::test]
async fn arguments_and_results_cross_as_json() {
    let (outcome, invoker) = execute_canned(
        r#"
        let todo = tools::todos(#{});
        `${todo.len()} todos, first is ${todo[0].text}`
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!("1 todos, first is ship it"));
    // The no-argument arity is the same call with an empty map.
    assert_eq!(invoker.calls()[0].args, json!({}));
}

#[tokio::test]
async fn a_tool_with_no_required_arguments_may_be_called_plainly() {
    let (outcome, _) = execute_canned("tools::todos().len()").await;
    assert_eq!(value(&outcome), json!(1));
}

#[tokio::test]
async fn renamed_mcp_tools_are_callable_and_dispatch_to_the_roster_name() {
    let (outcome, invoker) =
        execute_canned(r#"mcp::dev_radius::resolve_library_id(#{ name: "radius" })"#).await;

    assert_eq!(value(&outcome), json!({ "id": "/dev/radius" }));
    assert_eq!(
        invoker.calls()[0].host_name,
        "dev_radius__resolve-library-id"
    );
    assert_eq!(
        outcome.calls[0].qualified,
        "mcp::dev_radius::resolve_library_id"
    );
}

#[tokio::test]
async fn text_items_and_console_output_are_collected() {
    let (outcome, _) = execute_canned(
        r#"
        console.log("starting");
        text("first");
        console.warn("careful", 2);
        text(#{ done: true });
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
    assert_eq!(outcome.logs, vec!["starting", "careful 2"]);
}

#[tokio::test]
async fn a_denied_call_is_catchable_and_recorded() {
    let (outcome, _) = execute_canned(
        r#"
        let out = "unset";
        try { out = tools::deny(#{}) } catch (err) { out = `${err}`; }
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
    let (outcome, _) = execute_canned("tools::deny(#{})").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Eval);
    assert_eq!(error.message, "permission denied: deny");
    // The call still happened, and the trace says how it went.
    assert_eq!(outcome.calls.len(), 1);
}

#[tokio::test]
async fn a_script_cannot_call_a_tool_the_catalog_does_not_hold() {
    let (outcome, _) = execute_canned("tools::not_a_tool(#{})").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Eval);
    assert!(error.message.contains("not_a_tool"), "{}", error.message);
    assert_eq!(error.line, Some(1));
}

#[tokio::test]
async fn a_parse_error_points_at_the_line() {
    let (outcome, _) = execute_canned("let x = 1;\nlet y = ;").await;
    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Parse);
    assert_eq!(error.line, Some(2));
}

#[tokio::test]
async fn exit_ends_the_script_with_its_value() {
    let (outcome, _) =
        execute_canned("text(\"before\"); exit(#{ stopped: true }); \"after\"").await;
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
async fn a_missing_map_key_is_unit_rather_than_an_error() {
    // The card teaches against this; the point of the test is that a script can
    // rely on the engine, not on a guess about it. It is true of a *map*: any
    // other type raises for an unknown property.
    let (outcome, _) = execute_canned(
        r#"
        let todos = tools::todos(#{});
        if todos[0].typo == () { "silent" } else { "loud" }
        "#,
    )
    .await;

    assert_eq!(value(&outcome), json!("silent"));
}

#[tokio::test]
async fn the_operation_limit_stops_a_runaway_script() {
    let limits = Limits {
        max_operations: Some(10_000),
        ..Limits::default()
    };
    let (outcome, _) = execute_with(
        "let x = 0; while true { x += 1; }",
        limits,
        stub(),
        catalog(),
    )
    .await;

    let error = error(&outcome);
    assert_eq!(error.kind, ErrorKind::Limit);
    assert!(
        error.message.contains("operation limit"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn the_timeout_stops_a_runaway_script() {
    let limits = Limits {
        timeout: Some(Duration::from_millis(50)),
        ..Limits::default()
    };
    let (outcome, _) = execute_with(
        "let x = 0; while true { x += 1; }",
        limits,
        stub(),
        catalog(),
    )
    .await;

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
        let n = 0;
        while n < 5 {
            try { tools::todos(#{}) } catch (err) { console.log(`caught: ${err}`); }
            n += 1;
        }
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
    // A caught limit is still a limit: the script does not get to keep going.
    // The third call is refused before it is dispatched, so the host never sees
    // it and the trace counts only the calls that happened.
    assert_eq!(invoker.calls().len(), 2);
    assert_eq!(outcome.calls.len(), 2);
}

#[tokio::test]
async fn a_cancel_stops_the_script() {
    let stop = Stop::new();
    let invoker = stub();
    let handle = tokio::spawn(run(
        ScriptRequest {
            script: "let x = 0; while true { x += 1; }".to_string(),
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

#[tokio::test]
async fn the_store_survives_between_scripts() {
    let store = Arc::new(Store::new());
    let limits = Limits::default();

    let first = run(
        ScriptRequest {
            script: "store(\"seen\", 41); 0".to_string(),
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
            script: "load(\"seen\") + 1".to_string(),
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
async fn a_key_that_was_never_stored_reads_as_unit() {
    let outcome = run(
        ScriptRequest {
            script: "if load(\"never\") == () { \"empty\" } else { \"set\" }".to_string(),
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
            script: "store(\"k\", 1)".to_string(),
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
        let src = tools::read_file(#{ path: "src/main.rs" });
        console.log("read it");
        `${src.len} characters`
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
    assert!(rendered.contains("1. tools::read_file — "), "{rendered}");
    assert!(rendered.contains(r#"{"path":"src/main.rs"}"#), "{rendered}");
}

/// The discovery globals exist so a script can reach a tool the description did
/// not inline, so what matters is that they answer from the engine -- with the
/// catalog's names, and without spending a tool call to do it.
#[tokio::test]
async fn search_tools_answers_from_the_catalog() {
    let (outcome, invoker) = execute_canned(r#"search_tools("read")"#).await;

    assert_eq!(
        value(&outcome),
        json!({
            "matched": 1,
            "tools": [{ "name": "tools::read_file", "description": "Read a file." }],
        })
    );
    // Reaching for a tool is not calling one.
    assert_eq!(invoker.calls_made.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn search_tools_takes_a_limit_and_a_namespace() {
    let (outcome, _) = execute_canned(
        r#"
        let wide = search_tools("", #{ limit: 2 });
        let scoped = search_tools("", #{ namespace: "mcp" });
        [wide.tools.len, wide.matched, scoped.tools.len]
        "#,
    )
    .await;

    // The limit is applied after matching, so `matched` still counts them all,
    // and a namespace filter sees only that namespace.
    assert_eq!(value(&outcome), json!([2, 5, 1]));
}

#[tokio::test]
async fn all_tools_is_the_roster_a_script_can_filter() {
    let (outcome, _) = execute_canned(
        r#"
        let mcp = ALL_TOOLS.filter(|name| name.starts_with("mcp::"));
        `all: ${ALL_TOOLS.len}, mcp: ${mcp.len}, one: ${mcp[0]}`
        "#,
    )
    .await;

    assert_eq!(
        value(&outcome),
        json!("all: 5, mcp: 1, one: mcp::dev_radius::resolve_library_id")
    );
}

#[tokio::test]
async fn describe_tool_spells_out_a_signature() {
    let (outcome, _) = execute_canned(r#"describe_tool("read_file")"#).await;

    let described = value(&outcome);
    let described = described.as_str().unwrap();
    assert_eq!(
        described,
        "tools::read_file(path: String) -> Dynamic\nRead a file.\n\npath: String"
    );
}

#[tokio::test]
async fn describe_namespace_lists_a_server() {
    let (outcome, _) = execute_canned(r#"describe_namespace("mcp")"#).await;

    let described = value(&outcome);
    let described = described.as_str().unwrap();
    assert!(described.starts_with("mcp (1 tool)"), "{described}");
    assert!(
        described.contains("mcp::dev_radius::resolve_library_id(name?: String) -> Dynamic"),
        "{described}"
    );
}

/// A name nobody knows is a *runtime error*, not a `()`: the model gets a
/// message with the near-misses, and a script that wants to survive a bad name
/// can catch it.
#[tokio::test]
async fn describe_tool_reports_an_unknown_name_and_a_script_can_catch_it() {
    let (outcome, _) = execute_canned(
        r#"
        let out = "uncaught";
        try { out = describe_tool("read_fil"); } catch (err) { out = err.to_string(); }
        out
        "#,
    )
    .await;

    let message = value(&outcome);
    let message = message.as_str().unwrap();
    assert!(message.contains("did you mean"), "{message}");
    assert!(message.contains("tools::read_file"), "{message}");
}

/// The catalog the globals answer from is the one the request carried, so a
/// tool that is not in it is not offered by any route.
#[tokio::test]
async fn the_discovery_globals_only_see_this_scripts_catalog() {
    let invoker = stub();
    let outcome = run(
        ScriptRequest {
            script: r#"search_tools("write_file", #{ limit: 5 })"#.to_string(),
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
/// rest of the output usable drops exactly that line — so the count comes back
/// as a clean `100` with nothing to say it is partial.
///
/// This is the shape of a real miss: a session tallied per-crate file counts,
/// three crates came back as exactly 100, and the summary was wrong. The guard
/// the documents now teach is checked here, because the prose is only true if
/// `contains` still sees the note after the filter has run.
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
            script: r#"let found = tools::glob(#{ pattern: "crates/*/src/**" });
let files = found.split("\n").filter(|line| line.ends_with(".rs"));
if found.contains("truncated") { `partial: ${files.len}` } else { `${files.len}` }"#
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
            script:
                r#"tools::glob(#{ pattern: "crates/*/src/**" }).split("\n").filter(|line| line.ends_with(".rs")).len"#
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
