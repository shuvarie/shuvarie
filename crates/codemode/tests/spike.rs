//! Phase 0 of the codemode build order: the Rhai semantics the design leans on,
//! pinned as assertions so a dependency bump cannot change them silently.
//!
//! Each test names the design claim it holds up. Where the claim is about
//! something Rhai does *not* do (no module resolver, no implicit conversions),
//! the test asserts the failure, because the dialect card teaches against it.
//!
//! What the spike turned up, none of which the design assumed:
//!
//! - `call`, `eval`, `curry`, `spawn`, `type`, `switch`, `throw`, `in`, ... are
//!   reserved words, so a tool by one of those names needs a mangled callable
//!   name; a `-` in a name does not even fail to parse, it parses as arithmetic.
//! - A tool call registered the naive way is *not* elided or folded by the
//!   optimizer at any level, but the naive metadata (pure, cacheable) is the
//!   opposite of what a tool call is, so the marking stays.
//! - `on_progress` cannot raise: returning a value stops the script with
//!   `ErrorTerminated`, which `Engine::eval` does *not* map to a value (unlike
//!   the built-in `exit`, which it does). The abort is the host's to report.
//! - Property access on a map is a key lookup, so `m.len` is `()` and
//!   `m.len()` is the length -- a silent `()` where JavaScript would be a
//!   number or a throw.
//! - String and array methods do not agree with each other, or with
//!   JavaScript: `trim`/`replace`/`truncate`/`remove`/`clear` and array
//!   `push`/`sort`/`reverse` mutate in place and return `()`, while
//!   `to_upper`/`split`/`sub_string`/`index_of`/`map`/`extract` return a new
//!   value. A map is not iterable at all -- `.keys()` first -- and a string has
//!   no `to_int`.
//! - Applying the wrong operator is not an error: `"a" - "b"` is `"a"`.
//! - `try`/`catch` is a statement whose value is always `()`, so a fallback has
//!   to be assigned to a variable declared before it. `throw` is a statement
//!   too and cannot appear in an expression.
//! - Not every runtime error carries a position: an arithmetic failure has
//!   none, while a missing function does.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use rhai::{Dynamic, Engine, EvalAltResult, FuncRegistration, Module, OptimizationLevel, Position};
use serde_json::{Value, json};

use shuvarie_codemode::to_runtime_error;

/// `tools::read_file(...)` and `mcp::dev_radius::search(...)`: static modules
/// resolve namespace-qualified, with no `import` and no module resolver.
#[test]
fn static_modules_resolve_without_import() {
    let mut tools = Module::new();
    tools.set_native_fn("read_file", |path: rhai::ImmutableString| {
        Ok::<_, Box<EvalAltResult>>(format!("contents of {path}"))
    });

    // Sub-modules are the MCP servers: one namespace per server, registered
    // either as a sub-module or with a `::` path in the name.
    let mut dev_radius = Module::new();
    dev_radius.set_native_fn("search", |query: rhai::ImmutableString| {
        Ok::<_, Box<EvalAltResult>>(query.len() as i64)
    });
    let mut mcp = Module::new();
    mcp.set_sub_module("dev_radius", dev_radius);

    let mut engine = Engine::new();
    engine.register_static_module("tools", tools.into());
    engine.register_static_module("mcp", mcp.into());

    assert_eq!(
        engine
            .eval::<String>(r#"tools::read_file("src/main.rs")"#)
            .unwrap(),
        "contents of src/main.rs"
    );
    assert_eq!(
        engine
            .eval::<i64>(r#"mcp::dev_radius::search("orders")"#)
            .unwrap(),
        6
    );
}

/// A module-level variable (`ALL_TOOLS`, `models`) is readable as
/// `module::NAME` from the script.
#[test]
fn module_variables_are_readable_qualified() {
    let mut meta = Module::new();
    meta.set_var(
        "ALL_TOOLS",
        Dynamic::from_array(vec![Dynamic::from("read_file")]),
    );

    let mut engine = Engine::new();
    engine.register_static_module("meta", meta.into());

    assert_eq!(engine.eval::<i64>("meta::ALL_TOOLS.len").unwrap(), 1);
    assert_eq!(
        engine.eval::<String>(r#"meta::ALL_TOOLS[0]"#).unwrap(),
        "read_file"
    );
}

/// `import` resolves nothing: the design's capability story is that a script
/// cannot pull in code of its own choosing.
#[test]
fn import_resolves_nothing() {
    let engine = Engine::new();
    let err = engine
        .eval::<Dynamic>(r#"import "other" as other; 1"#)
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorModuleNotFound(..)),
        "unexpected error: {err}"
    );
}

/// A tool name is not automatically a callable name. Mangling is the catalog's
/// job, and it has to be exhaustive, because the model cannot detect any of
/// this from the name it was given.
#[test]
fn tool_names_are_not_always_callable_names() {
    let mut module = Module::new();
    for name in ["_call", "call_", "type", "read_file", "resolve_library_id"] {
        let body = name.to_string();
        module.set_native_fn(name, move || Ok::<_, Box<EvalAltResult>>(body.clone()));
    }

    let mut engine = Engine::new();
    engine.register_static_module("tools", module.into());

    // Reserved words cannot be called at all -- not even qualified.
    assert!(engine.compile("tools::call()").is_err());
    assert!(engine.compile("tools::eval()").is_err());
    assert!(engine.compile("tools::switch()").is_err());
    assert!(engine.compile("tools::match()").is_err());
    assert!(engine.compile("tools::in()").is_err());

    // ... but a word that merely *looks* like a keyword can be fine, so the
    // rule has to be the exact list rather than a guess.
    assert_eq!(engine.eval::<String>("tools::type()").unwrap(), "type");

    // What the mangling rule produces instead: a `_` prefix, which is callable.
    assert_eq!(engine.eval::<String>("tools::_call()").unwrap(), "_call");
    assert_eq!(engine.eval::<String>("tools::call_()").unwrap(), "call_");

    // A bare `_` is not an identifier either, so a nameless tool needs a real
    // fallback name rather than a `_` prefix.
    assert!(engine.compile("tools::_()").is_err());

    // `-` is the sharp one: the name *parses*, as subtraction, and fails only
    // at run time on the undefined variables it became.
    let ast = engine
        .compile("tools::resolve-library-id()")
        .expect("a dashed name parses as arithmetic");
    assert!(engine.eval_ast::<Dynamic>(&ast).is_err());
}

/// A tool call must be registered **impure and volatile**, and must run exactly
/// once per evaluation: never at compile time (that would fire the call outside
/// the turn's permission flow), never elided as an unused effect, never folded
/// or reused for a second evaluation.
///
/// Rhai 1.26 does none of those even for a naively registered function, so this
/// test would pass without the marking. It asserts the guarantee we rely on
/// rather than Rhai's current optimizer: the naive metadata (pure, cacheable)
/// is the opposite of what a tool call is, so it stays marked.
#[test]
fn tool_calls_run_exactly_once_per_evaluation() {
    for level in [
        OptimizationLevel::None,
        OptimizationLevel::Simple,
        OptimizationLevel::Full,
    ] {
        let calls = Arc::new(AtomicU64::new(0));
        let mut module = Module::new();
        let counter = Arc::clone(&calls);
        let _ = FuncRegistration::new("double")
            .in_internal_namespace()
            .with_purity(false)
            .with_volatility(true)
            .set_into_module(&mut module, move |x: i64| {
                counter.fetch_add(1, Ordering::Relaxed);
                Ok::<_, Box<EvalAltResult>>(x * 2)
            });

        let mut engine = Engine::new();
        engine.set_optimization_level(level);
        engine.register_static_module("tools", module.into());

        let ast = engine.compile("tools::double(41) + 1").unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            0,
            "{level:?}: ran at compile"
        );

        assert_eq!(engine.eval_ast::<i64>(&ast).unwrap(), 83);
        assert_eq!(engine.eval_ast::<i64>(&ast).unwrap(), 83);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "{level:?}: a call was cached, elided or folded"
        );

        // An unused call is still an effect.
        engine.eval::<i64>("tools::double(1); 0").unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            3,
            "{level:?}: an unused call was elided"
        );
    }
}

/// `max_operations` bounds a runaway loop even when nothing is cancelled.
#[test]
fn operation_limit_stops_a_runaway_script() {
    let mut engine = Engine::new();
    engine.set_max_operations(10_000);

    let err = engine
        .eval::<Dynamic>("let x = 0; while true { x += 1; }")
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTooManyOperations(..)),
        "unexpected error: {err}"
    );
}

/// `on_progress` is the cancellation hook: it fires often, and returning a
/// value stops the script -- the only way out, since the callback cannot raise.
/// The abort arrives as `ErrorTerminated`, so the host checks its own flag and
/// reports the abort itself.
#[test]
fn on_progress_is_the_cancel_hook() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let ticks = Arc::new(AtomicU64::new(0));

    let flag = Arc::clone(&cancelled);
    let counter = Arc::clone(&ticks);
    let mut engine = Engine::new();
    engine.on_progress(move |_| {
        counter.fetch_add(1, Ordering::Relaxed);
        flag.load(Ordering::Relaxed).then_some(Dynamic::UNIT)
    });

    // Not cancelled: the script runs to completion and yields its value.
    let value = engine
        .eval::<i64>("let x = 0; while x < 10 { x += 1; } x")
        .unwrap();
    assert_eq!(value, 10);

    // Cancelled: the stop is an error, not a value -- unlike `exit()`, whose
    // `Exit` variant `Engine::eval` does map to one.
    cancelled.store(true, Ordering::Relaxed);
    let err = engine
        .eval::<Dynamic>("let x = 0; while true { x += 1; } x")
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorTerminated(..)),
        "unexpected error: {err}"
    );
    assert!(ticks.load(Ordering::Relaxed) > 0);
}

/// The script's value: a top-level `return`, or the last expression, or the
/// built-in `exit(value)` -- which `Engine::eval` maps to the return value
/// rather than an error, so no host-side `exit` is needed.
#[test]
fn top_level_return_and_exit_are_the_value() {
    let engine = Engine::new();

    assert_eq!(engine.eval::<i64>("if true { return 41; } 0").unwrap(), 41);
    assert_eq!(engine.eval::<i64>("let x = 1; x + 1").unwrap(), 2);
    assert_eq!(
        engine
            .eval::<i64>("let x = { let y = 2; y * 3 }; x")
            .unwrap(),
        6
    );

    let value = engine.eval::<Dynamic>("exit(#{ ok: true })").unwrap();
    assert_eq!(
        rhai::serde::from_dynamic::<Value>(&value).unwrap(),
        json!({ "ok": true })
    );
}

/// JSON is the crossing type, so the conversion has to be total and stable:
/// null has no Rhai equivalent (it becomes `()`, which is also what a missing
/// map key reads as), and integers stay integers.
#[test]
fn serde_round_trips_json() {
    let value = json!({
        "null": null,
        "int": 7,
        "neg": -7,
        "float": 1.5,
        "string": "hi",
        "bool": true,
        "array": [1, "two", null],
        "object": { "nested": { "k": "v" } },
    });

    let dynamic = rhai::serde::to_dynamic(&value).unwrap();
    assert!(dynamic.is_map());

    let null =
        rhai::serde::from_dynamic::<Value>(&rhai::serde::to_dynamic(json!(null)).unwrap()).unwrap();
    assert!(null.is_null());

    let back = rhai::serde::from_dynamic::<Value>(&dynamic).unwrap();
    assert_eq!(back, value);

    // Arguments arrive as a scope value, which is how a tool's JSON arguments
    // reach a script.
    let engine = Engine::new();
    let mut scope = rhai::Scope::new();
    scope.push_dynamic("args", rhai::serde::to_dynamic(json!({ "n": 41 })).unwrap());
    assert_eq!(
        engine
            .eval_with_scope::<i64>(&mut scope, "args.n + 1")
            .unwrap(),
        42
    );
}

/// `console` output is captured, not streamed: `on_print`/`on_debug` are where
/// the result block's `<console_output>` comes from.
#[test]
fn console_output_is_captured() {
    let captured: Arc<Mutex<Vec<String>>> = Arc::default();

    let mut engine = Engine::new();
    let sink = Arc::clone(&captured);
    engine.on_print(move |line| sink.lock().unwrap().push(format!("log: {line}")));
    let sink = Arc::clone(&captured);
    engine.on_debug(move |line, _source, pos| {
        sink.lock()
            .unwrap()
            .push(format!("debug: {line} at {pos:?}"))
    });

    engine
        .eval::<i64>("print(\"hello\"); debug(\"world\"); 0")
        .unwrap();

    let captured = captured.lock().unwrap();
    assert_eq!(captured.len(), 2, "{captured:?}");
    assert_eq!(captured[0], "log: hello");
    assert!(captured[1].starts_with("debug: \"world\" at"));
}

/// The Rhai footguns the dialect card has to teach, asserted as they actually
/// behave, so the prose cannot drift from the interpreter.
#[test]
fn footguns_the_dialect_card_must_teach() {
    let engine = Engine::new();

    // Integer arithmetic stays integer; division by zero is an error.
    assert_eq!(engine.eval::<i64>("7 / 2").unwrap(), 3);
    assert_eq!(engine.eval::<i64>("7 % 2").unwrap(), 1);
    assert!(engine.eval::<i64>("1 / 0").is_err());

    // Property access on a map is a key lookup, so a missing key is `()`
    // -- silently, and the same for `.missing`, `["missing"]`, and `.get(..)`.
    assert!(
        engine
            .eval::<Dynamic>(r#"let m = #{ a: 1 }; m.missing"#)
            .unwrap()
            .is_unit()
    );
    assert!(
        engine
            .eval::<Dynamic>(r#"let m = #{ a: 1 }; m["missing"]"#)
            .unwrap()
            .is_unit()
    );
    assert!(
        engine
            .eval::<Dynamic>(r#"let m = #{ a: 1 }; m.get("missing")"#)
            .unwrap()
            .is_unit()
    );
    // ... which makes `len` a trap: the property form is a key lookup.
    assert!(
        engine
            .eval::<Dynamic>("let m = #{ a: 1 }; m.len")
            .unwrap()
            .is_unit()
    );
    assert_eq!(engine.eval::<i64>("let m = #{ a: 1 }; m.len()").unwrap(), 1);
    // On an array both forms are the length.
    assert_eq!(engine.eval::<i64>("[1, 2].len").unwrap(), 2);
    assert_eq!(engine.eval::<i64>("[1, 2].len()").unwrap(), 2);

    // There is no ternary, no truthiness, and no `let _`.
    assert!(engine.eval::<i64>("true ? 1 : 2").is_err());
    assert!(engine.eval::<i64>("if () { 1 } else { 2 }").is_err());
    assert!(engine.compile("let _ = 1;").is_err());
    // `()` is only equatable, not falsy.
    assert!(engine.eval::<bool>("() == ()").unwrap());
    assert!(engine.eval::<bool>("!()").is_err());

    // A wrong operator is silently a no-op rather than an error.
    assert_eq!(engine.eval::<String>(r#""a" - "b""#).unwrap(), "a");
    // ... but `+` does convert, string on either side.
    assert_eq!(engine.eval::<String>(r#""x" + 1"#).unwrap(), "x1");
    assert_eq!(engine.eval::<String>(r#"1 + "x""#).unwrap(), "1x");
    assert_eq!(engine.eval::<String>(r#"`x` + ()"#).unwrap(), "x");

    // What replaces them: `if` as an expression, `switch`, backticks,
    // `to_string`, `try`/`catch`, and the array/map helpers.
    assert_eq!(engine.eval::<i64>("if true { 1 } else { 2 }").unwrap(), 1);
    assert_eq!(
        engine
            .eval::<String>(r#"let x = 2; switch x { 1 => "one", 2 => "two", _ => "other" }"#)
            .unwrap(),
        "two"
    );
    assert_eq!(
        engine
            .eval::<String>(r#"let n = 41; `n is ${n + 1}`"#)
            .unwrap(),
        "n is 42"
    );
    assert_eq!(
        engine.eval::<String>(r#"41.to_string() + "x""#).unwrap(),
        "41x"
    );

    // `try`/`catch` is a **statement** whose value is always `()`, and `throw`
    // is a statement too, so the recoverable-error idiom is a pre-declared
    // variable that both arms assign to.
    assert!(
        engine
            .eval::<Dynamic>(r#"try { throw "boom"; } catch (e) { "caught" }"#)
            .unwrap()
            .is_unit()
    );
    assert!(engine.eval::<Dynamic>(r#"let v = throw "boom";"#).is_err());
    assert_eq!(
        engine
            .eval::<String>(
                r#"
                fn risky() { throw "boom"; }
                let out = "unset";
                try { out = risky(); } catch (e) { out = `fell back: ${e}`; }
                out
                "#
            )
            .unwrap(),
        "fell back: boom"
    );
    assert_eq!(
        engine
            .eval::<i64>(r#"[1, 2, 3].filter(|x| x > 1).len"#)
            .unwrap(),
        2
    );
    assert_eq!(
        engine
            .eval::<Dynamic>(r#"[1, 2, 3].map(|x| x * 2)"#)
            .unwrap()
            .to_string(),
        "[2, 4, 6]"
    );
    assert_eq!(
        engine.eval::<String>(r#""a,b".split(",")[1]"#).unwrap(),
        "b"
    );
    assert_eq!(
        engine
            .eval::<i64>(
                "let hits = [#{ n: 1 }, #{ n: 2 }]; let total = 0; for h in hits { total += h.n; } total"
            )
            .unwrap(),
        3
    );
    assert_eq!(engine.eval::<String>("type_of([])").unwrap(), "array");
    assert_eq!(engine.eval::<String>("type_of(#{})").unwrap(), "map");
    assert_eq!(engine.eval::<String>("type_of(())").unwrap(), "()");
}

/// The bridge, in miniature: a tool call is a registered closure that sends a
/// request and **blocks** on the reply while the host performs the real call on
/// another thread. Nothing Rhai-aware crosses the channel -- only JSON.
#[test]
fn tool_calls_block_on_a_reply_across_threads() {
    let (request_tx, request_rx) = mpsc::channel::<(String, Value, mpsc::Sender<Value>)>();

    // The host side: one thread standing in for the async task that performs
    // the real `Tool::call`.
    let host = std::thread::spawn(move || {
        let mut calls = Vec::new();
        while let Ok((name, args, reply)) = request_rx.recv() {
            calls.push(name.clone());
            let _ = reply.send(json!({ "tool": name, "args": args }));
        }
        calls
    });

    let mut tools = Module::new();
    let _ = FuncRegistration::new("invoke")
        .in_internal_namespace()
        .with_purity(false)
        .with_volatility(true)
        .set_into_module(
            &mut tools,
            move |name: rhai::ImmutableString, args: Dynamic| {
                let args = rhai::serde::from_dynamic::<Value>(&args).map_err(to_runtime_error)?;
                let (reply_tx, reply_rx) = mpsc::channel();
                request_tx
                    .send((name.to_string(), args, reply_tx))
                    .map_err(to_runtime_error)?;
                let reply = reply_rx.recv().map_err(to_runtime_error)?;
                rhai::serde::to_dynamic(reply)
            },
        );

    let mut engine = Engine::new();
    engine.register_static_module("tools", tools.into());

    let value = engine
        .eval::<Dynamic>(
            r#"
            let a = tools::invoke("read_file", #{ path: "src/main.rs" });
            let b = tools::invoke("grep", #{ pattern: a.tool });
            b
            "#,
        )
        .unwrap();

    assert_eq!(
        rhai::serde::from_dynamic::<Value>(&value).unwrap(),
        json!({ "tool": "grep", "args": { "pattern": "read_file" } })
    );

    drop(engine);
    assert_eq!(host.join().unwrap(), vec!["read_file", "grep"]);
}

/// The async half of the bridge: the script runs on a dedicated, non-runtime
/// thread, so it may block on a tokio oneshot; the async side sends the reply
/// and receives the script's own result back the same way.
#[tokio::test]
async fn tokio_oneshot_bridges_a_blocked_script_thread() {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel::<Value>();
    let (result_tx, result_rx) = tokio::sync::oneshot::channel::<Value>();

    let script = std::thread::spawn(move || {
        let reply = reply_rx.blocking_recv().expect("host replied");
        result_tx.send(reply).ok();
    });

    reply_tx.send(json!({ "answer": 42 })).unwrap();
    assert_eq!(result_rx.await.unwrap(), json!({ "answer": 42 }));
    script.join().unwrap();
}

/// A script closure reaches the host as a `FnPtr` and can be called back
/// through the native call context -- the hook a later `wait_all` fan-out
/// needs, and the reason `no_closure` must stay off.
#[test]
fn fn_ptr_callbacks_cross_both_ways() {
    let mut engine = Engine::new();
    engine.register_fn(
        "apply",
        |ctx: rhai::NativeCallContext, f: rhai::FnPtr, x: i64| {
            f.call_within_context::<i64>(&ctx, (x,))
        },
    );

    // Script-defined functions and capturing closures both survive the trip.
    assert_eq!(
        engine
            .eval::<i64>("fn double(x) { x * 2 } apply(|n| double(n) + 1, 20)")
            .unwrap(),
        41
    );
    assert_eq!(
        engine
            .eval::<i64>("let base = 40; apply(|n| base + n, 2)")
            .unwrap(),
        42
    );
}

/// Errors carry a position, which is what the result block reports instead of
/// making the model hunt through its own script. A script error and a parse
/// error are different types, so both are read here.
#[test]
fn errors_carry_positions() {
    let engine = Engine::new();

    // Calling something that does not exist is the common script bug, and it
    // does carry a position.
    let err = engine.eval::<Dynamic>("let x = 1;\nnope()").unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
        "unexpected error: {err}"
    );
    assert_eq!(err.position().line(), Some(2));

    // Not every runtime error does: an arithmetic failure reports `none`, so
    // the result block has to render a missing position rather than assume one.
    let err = engine.eval::<Dynamic>("let x = 1;\n1 / 0").unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorArithmetic(..)),
        "unexpected error: {err}"
    );
    assert_eq!(err.position().line(), None);

    let parse = engine.compile("let x = ;").unwrap_err();
    assert_eq!(parse.position().line(), Some(1));

    // A `Result`-shaped native error the host raises itself has no position,
    // and its `Display` is prefixed -- the payload is the message, so that is
    // what a result block renders and what a script's `catch (e)` sees.
    let raised = to_runtime_error("permission denied");
    assert_eq!(raised.position(), Position::NONE);
    assert_eq!(raised.to_string(), "Runtime error: permission denied");

    // A script's `catch (err)` receives the payload, not the prefixed
    // rendering, which is what makes a denied permission readable in-script.
    let mut engine = Engine::new();
    engine.register_fn("deny", || -> Result<(), Box<EvalAltResult>> {
        Err(to_runtime_error("permission denied"))
    });
    assert_eq!(
        engine
            .eval::<String>(r#"let e = ""; try { deny(); } catch (err) { e = err.to_string(); } e"#)
            .unwrap(),
        "permission denied"
    );
}

/// A script's value as JSON, so every kind of assertion reads the same way.
fn eval(engine: &Engine, script: &str) -> Value {
    let value = engine.eval::<Dynamic>(script).unwrap();
    rhai::serde::from_dynamic::<Value>(&value).unwrap()
}

/// Which string, array, and map methods mutate their receiver and return `()`,
/// and which return a new value -- the fact the dialect card and the built-in
/// skill both teach, because a model fluent in JavaScript expects every method
/// to hand back a value.
///
/// Both documents' *fenced* examples are executed by `tests/doc.rs`, but a prose
/// list of method names is not, so the claims are pinned here: a Rhai bump that
/// flips one of these fails this test rather than quietly teaching the model a
/// call that returns `()` where it expected a string.
#[test]
fn mutating_methods_return_nothing_and_the_rest_return_a_value() {
    let engine = Engine::new();

    // Mutating: the receiver changes, and the call is worth nothing.
    assert_eq!(eval(&engine, r#"let s = "  x  "; s.trim(); s"#), json!("x"));
    assert_eq!(
        eval(&engine, r#"let s = "a-b"; s.replace("-", "_"); s"#),
        json!("a_b")
    );
    assert_eq!(
        eval(&engine, r#"let s = "abcd"; s.truncate(2); s"#),
        json!("ab")
    );
    assert_eq!(
        eval(&engine, r#"let s = "abcd"; s.remove("b"); s"#),
        json!("acd")
    );
    assert_eq!(eval(&engine, r#"let s = "abcd"; s.clear(); s"#), json!(""));
    assert_eq!(
        eval(&engine, r#"type_of(" abc ".trim())"#),
        json!("()"),
        "trim returns nothing, so it cannot be chained into `split`"
    );
    assert_eq!(
        eval(&engine, r#"let a = [1]; type_of(a.push(2))"#),
        json!("()")
    );
    assert_eq!(eval(&engine, r#"let a = [2, 1]; a.sort(); a[0]"#), json!(1));
    assert_eq!(
        eval(&engine, r#"let a = [1, 2]; a.reverse(); a[0]"#),
        json!(2)
    );

    // Returning: the receiver is untouched and the call yields the new value.
    assert_eq!(
        eval(
            &engine,
            r#"let s = "ab"; let u = s.to_upper(); `${s}/${u}`"#
        ),
        json!("ab/AB")
    );
    assert_eq!(
        eval(&engine, r#"type_of("a,b".split(","))"#),
        json!("array")
    );
    assert_eq!(eval(&engine, r#""abcd".sub_string(0, 2)"#), json!("ab"));
    assert_eq!(eval(&engine, r#""abc".index_of("b")"#), json!(1));
    assert_eq!(
        eval(&engine, r#"type_of([1, 2].map(|v| v + 1))"#),
        json!("array")
    );
    assert_eq!(eval(&engine, r#"[1, 2, 3].extract(0, 2).len"#), json!(2));
}

/// Three habits that cost a live agent a rewrite each, pinned because the skill
/// teaches against them: a stored closure is not callable by name, a function
/// receives copies so mutating a parameter reaches nobody, and Rhai's borrow
/// checker turns "mutate a captured variable inside a closure" into a data-race
/// error rather than a silent wrong answer.
#[test]
fn closures_are_called_with_dot_call_and_functions_receive_copies() {
    let engine = Engine::new();

    // The call form: `.call(...)`, not `f(...)`.
    assert_eq!(eval(&engine, r#"let f = |x| x + 1; f.call(2)"#), json!(3));
    let err = engine
        .eval::<Dynamic>(r#"let f = |x| x + 1; f(2)"#)
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
        "a variable holding a closure is not a callable name: {err}"
    );

    // Arguments are values: a map mutated inside a function stays mutated only
    // in there, which reads as "the map never got built" at the call site.
    assert_eq!(
        eval(
            &engine,
            r#"fn bump(m, k) { m[k] = 1; } let m = #{}; bump(m, "a"); m.len()"#
        ),
        json!(0)
    );

    // Building a map in a `for` loop is the idiom the skill recommends.
    assert_eq!(
        eval(
            &engine,
            r#"let m = #{}; for v in ["a", "b", "a"] { m[v] = 1; } m.len()"#
        ),
        json!(2)
    );
}

/// The catch-variable and one-ended-trim habits, pinned because the card and the
/// skill teach *against* them.
#[test]
fn a_catch_variable_must_be_named_and_one_ended_trims_do_not_exist() {
    let engine = Engine::new();

    // `catch (_)` is the natural JavaScript shorthand for "I do not need it".
    assert!(matches!(
        *engine.compile("try { } catch (_) { }").unwrap_err().0,
        rhai::ParseErrorType::VariableExpected
    ));
    // Naming it is what both documents show.
    assert!(engine.compile("try { } catch (err) { }").is_ok());

    for script in [
        r#"let s = "abc"; s.trim_end_matches("c")"#,
        r#"let s = "abc"; s.strip_suffix("c")"#,
        r#"let s = "abc"; s.trim_end()"#,
    ] {
        let err = engine.eval::<Dynamic>(script).unwrap_err();
        assert!(
            matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
            "`{script}` should not resolve: {err}"
        );
    }

    // The replacements the documents give: `trim()` for both ends, `sub_string`
    // for one.
    assert_eq!(eval(&engine, r#"let s = "  x  "; s.trim(); s"#), json!("x"));
    assert_eq!(
        eval(&engine, r#"let s = "abc"; s.sub_string(0, s.len - 1)"#),
        json!("ab")
    );
}

/// The container accessors, which are inconsistent between arrays, strings and
/// maps -- and the reason the documents spell out `.len` versus `.len()`.
#[test]
fn container_accessors_are_not_uniform() {
    let engine = Engine::new();

    // Arrays and strings take both forms; a map only the method, because
    // property access on a map is a key lookup and a key could be called `len`.
    assert_eq!(eval(&engine, "[1, 2].len"), json!(2));
    assert_eq!(eval(&engine, "[1, 2].len()"), json!(2));
    assert_eq!(eval(&engine, r#""ab".len"#), json!(2));
    assert_eq!(eval(&engine, r#""ab".len()"#), json!(2));
    assert_eq!(eval(&engine, "#{ a: 1 }.len()"), json!(1));
    assert_eq!(
        eval(&engine, "type_of(#{ a: 1 }.len)"),
        json!("()"),
        "`m.len` is a missing key, not the length"
    );

    assert_eq!(eval(&engine, "[].is_empty"), json!(true));
    assert_eq!(eval(&engine, "type_of(#{ a: 1 }.keys())"), json!("array"));
    // `remove` hands back what it removed, unlike the string methods.
    assert_eq!(
        eval(
            &engine,
            r#"let m = #{ a: 1 }; let gone = m.remove("a"); gone"#
        ),
        json!(1)
    );
    // Keys iterate; the entries do not destructure.
    assert_eq!(
        eval(
            &engine,
            r#"let out = []; for k in #{ a: 1, b: 2 }.keys() { out.push(k); } out"#
        ),
        json!(["a", "b"])
    );
    let err = engine
        .eval::<Dynamic>("for (v, k) in #{ a: 1 } { }")
        .unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorParsing(..)),
        "a map is not directly iterable: {err}"
    );
}

/// A string does not convert with `to_int`: the card teaches `.to_string()` and
/// `.to_int()` as a pair, and only the first one exists.
#[test]
fn a_string_has_no_to_int() {
    let engine = Engine::new();
    assert_eq!(eval(&engine, "(12).to_string()"), json!("12"));

    let err = engine.eval::<Dynamic>(r#""12".to_int()"#).unwrap_err();
    assert!(
        matches!(*err, EvalAltResult::ErrorFunctionNotFound(..)),
        "unexpected error: {err}"
    );
}

/// Whether a data race sits at the bottom of a chain of call frames.
fn innermost_is_data_race(error: &EvalAltResult) -> bool {
    match error {
        EvalAltResult::ErrorDataRace(..) => true,
        EvalAltResult::ErrorInFunctionCall(_, _, cause, ..) => innermost_is_data_race(cause),
        _ => false,
    }
}

/// A method call on a variable, whose own closure also touches that variable, is
/// a data race: the method's result holds the value while the closure runs.
///
/// Pinned because it is easy to hit while aggregating — `m.keys().reduce(|a, k|
/// if m[k] > m[a] { k } else { a }, "")` reads naturally and dies at runtime —
/// and because the two escapes the documents give have to keep working.
#[test]
fn a_closure_cannot_touch_the_value_it_is_iterating() {
    let engine = Engine::new();

    for method in ["keys", "values"] {
        let script = format!("let m = #{{ a: 1, b: 5, c: 2 }}; m.{method}().map(|_v| m.len()).len");
        let err = engine.eval::<Dynamic>(&script).unwrap_err();
        // The race arrives wrapped in the call frames (`map`, then the closure),
        // so it has to be found down the chain -- the same walk `classify` does
        // to name it in the result block.
        assert!(
            innermost_is_data_race(&err),
            "`{method}` should hold the map: {err}"
        );
        // ...and the frames are why the result block has to unwrap: the race's
        // own variable name is empty here, so it can only describe the shape.
        assert_eq!(err.position().line(), Some(1));
    }

    // A plain array reduce that reads its own accumulator is fine: nothing
    // outlives the closure.
    assert_eq!(
        eval(
            &engine,
            r#"let a = [3, 5]; a.reduce(|acc, v| if v > acc { v } else { acc }, 0)"#
        ),
        json!(5)
    );

    // Escape 1: bind the receiver first, so the temporary is gone by the time
    // the closure runs.
    assert_eq!(
        eval(
            &engine,
            r#"let m = #{ a: 1, b: 5, c: 2 };
               let ks = m.keys();
               ks.reduce(|a, k| if m[k] > m[a] { k } else { a }, "a")"#
        ),
        json!("b")
    );

    // Escape 2: no closure at all.
    assert_eq!(
        eval(
            &engine,
            r#"let m = #{ a: 1, b: 5, c: 2 };
               let total = 0;
               for k in m.keys() { total += m[k]; }
               total"#
        ),
        json!(8)
    );
}
