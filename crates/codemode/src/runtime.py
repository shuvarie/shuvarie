"""The codemode runtime: one script's builtins, its tool namespaces, and the runner.

`script.rs` loads this file with `PyModule::from_code` once per run, hands it the
native side as `_HOST` (a `Host` pyclass instance), and calls `start()`. So
everything here is per-run state: the module object is fresh, `_RUN` is fresh,
and nothing survives a run except what went through `store()`.

Two things live here rather than in Rust, because Python is where they belong:
formatting a failure (only Python can walk a traceback) and interrupting a
runaway script (only Python can call `PyThreadState_SetAsyncExc` on itself
without `unsafe`).
"""

import ast
import builtins
import ctypes
import json
import sys
import textwrap
import threading
import time
import types

FILENAME = "<codemode>"
"""What the script is compiled as, so a traceback can tell its frames apart."""

_POLL = 0.02
"""How often the watchdog asks the host whether the run should stop."""

_REAL_SLEEP = time.sleep
_REAL_INPUT = builtins.input
_REGISTRY = threading.Lock()
"""The registry is built once, and two runs can be the first to ask for it."""

_NOTHING = object()
"""The script produced no value at all, as distinct from a value of `None`."""


class ToolError(RuntimeError):
    """A tool call failed. The message is the tool's own, so `except ToolError`
    reads it rather than a wrapper's. A `RuntimeError` for anything catching the
    broader type."""


class _Exit(BaseException):
    """`exit(value)`: unwinds the script with a result of its own."""

    def __init__(self, value=None):
        self.value = value


class _Limit(BaseException):
    """A configured limit was reached (`max-operations`)."""


class _Stop(BaseException):
    """The host stopped the run: cancelled, out of time, or out of tool calls."""


# --------------------------------------------------------------------------
# JSON at the boundary
# --------------------------------------------------------------------------


def _dumps(value):
    """A value as JSON, for the host.

    `repr` is the fallback rather than an error: a script may reasonably emit a
    set or a `datetime`, and a repr string is more useful to the model than a
    failure to serialize. `allow_nan=False` because `NaN`/`Infinity` are not
    JSON -- serde_json would reject them on the way back.
    """
    try:
        return json.dumps(value, default=repr, allow_nan=False, ensure_ascii=False)
    except (TypeError, ValueError):
        return json.dumps(repr(value), ensure_ascii=False)


def _as_text(value):
    """A value as console output shows it: text bare, everything else as JSON."""
    if isinstance(value, str):
        return value
    return _dumps(value)


# --------------------------------------------------------------------------
# The globals a script may call
# --------------------------------------------------------------------------


class _Console:
    """`console.log(...)` and friends: captured into the result, not printed."""

    def log(self, *values):
        _HOST.log(" ".join(_as_text(value) for value in values))

    info = warn = error = debug = log


def _text(value=None):
    """Emit a value into the result, beside the script's own."""
    _HOST.emit(_dumps(value))


def _exit(value=None):
    """End the script here, with `value` as its result."""
    raise _Exit(value)


def _store(key, value):
    """Keep a value for the next call. JSON-shaped, like everything else."""
    _HOST.store_set(key, _dumps(value))


def _load(key, default=None):
    """Read a value `store` kept. A key that was never stored is `default`."""
    text = _HOST.store_get(key)
    if text is None:
        return default
    return json.loads(text)


def _discover(request):
    return json.loads(_HOST.discover(_dumps(request)))


def _search_tools(query, limit=None, namespace=None):
    """Rank the roster against `query`; `matched` counts every hit, so a list
    that had to stop says so instead of looking complete."""
    request = {"op": "search", "query": query, "namespace": namespace}
    if limit is not None:
        request["limit"] = limit
    return _discover(request)


def _describe_tool(name):
    """Spell one tool out with its parameter descriptions."""
    try:
        return _HOST.discover(_dumps({"op": "describe", "name": name}))
    except RuntimeError as failure:
        raise LookupError(str(failure)) from None


def _describe_namespace(namespace, limit=None):
    """List a whole namespace (`mcp.dev_radius`)."""
    request = {"op": "namespace", "namespace": namespace, "limit": limit}
    try:
        return _HOST.discover(_dumps(request))
    except RuntimeError as failure:
        raise LookupError(str(failure)) from None


# --------------------------------------------------------------------------
# Tools, as namespaces of callables
# --------------------------------------------------------------------------


def _arguments(qualified, args, kwargs):
    """A tool's arguments: keywords, or one dict for names `(**{...})` reaches.

    The dict form exists because a JSON Schema argument name is not necessarily
    an identifier (`x-api-key`), and `f(**{"x-api-key": 1})` is the one Python
    spelling that survives that.
    """
    if not args:
        return dict(kwargs)
    if len(args) == 1 and not kwargs and isinstance(args[0], dict):
        return args[0]
    raise TypeError(
        f"{qualified} takes a dict of arguments or keyword arguments, "
        f"not {len(args)} positional value(s)"
    )


def _tool(entry):
    """One callable tool: the crossing itself, and nothing else."""
    host = entry["host"]
    qualified = entry["qualified"]

    def call(*args, **kwargs):
        payload = _arguments(qualified, args, kwargs)
        try:
            answer = _HOST.call(qualified, host, _dumps(payload))
        except RuntimeError as failure:
            raise ToolError(str(failure)) from None
        return json.loads(answer)

    call.__name__ = entry["callable"]
    call.__qualname__ = qualified
    description = " ".join(entry.get("description", "").split())
    call.__doc__ = f"{qualified} - {description}" if description else qualified
    return call


def _namespace_tree(entries):
    """The catalog as nested namespaces: `tools.read_file`, `mcp.dev.search`."""
    root = {}
    for entry in entries:
        node = root
        for part in entry["namespace"]:
            node = node.setdefault(part, {})
        node[entry["callable"]] = _tool(entry)
    return {name: _freeze(child) for name, child in root.items()}


def _freeze(node):
    if callable(node):
        return node
    namespace = types.SimpleNamespace()
    for name, child in node.items():
        setattr(namespace, name, _freeze(child))
    return namespace


def _globals(manifest):
    """The script's global namespace: the tools, then the rest of the surface."""
    names = dict(_namespace_tree(manifest["tools"]))
    names.update(
        {
            "ALL_TOOLS": list(manifest["all_tools"]),
            "text": _text,
            "console": _Console(),
            "exit": _exit,
            # Named in the script's own namespace because catching a tool failure is
            # something a script has to be able to write down.
            "ToolError": ToolError,
            "search_tools": _search_tools,
            "describe_tool": _describe_tool,
            "describe_namespace": _describe_namespace,
        }
    )
    if manifest["store"]:
        names["store"] = _store
        names["load"] = _load
    return names


# --------------------------------------------------------------------------
# The runner
# --------------------------------------------------------------------------


class _Stream:
    """`print`, and anything else writing to `sys.stdout`, becomes console output.

    A proxy rather than a per-run swap: `sys.stdout` is process-global and runs are
    not sequential -- two sessions can run a script at the same time -- so swapping
    it per run would let one script's `print` land in another's result (and leave a
    dead stream behind when they finish out of order). The proxy is installed once
    and dispatches on the *thread*: the thread running a script has a sink, and
    every other thread gets the real stream back, which is what the host's own
    output needs.
    """

    def __init__(self, registry, which):
        self._registry = registry
        self._which = which
        self._local = threading.local()
        self.encoding = "utf-8"

    def _sink(self):
        entry = self._registry["threads"].get(threading.get_ident())
        return None if entry is None else entry["log"]

    def _buffer(self):
        if not hasattr(self._local, "buffer"):
            self._local.buffer = ""
        return self._local.buffer

    def write(self, text):
        sink = self._sink()
        if sink is None:
            self._registry[self._which].write(text)
            return len(text)
        pending = self._buffer() + text
        while "\n" in pending:
            line, pending = pending.split("\n", 1)
            sink(line)
        self._local.buffer = pending
        return len(text)

    def flush(self):
        sink = self._sink()
        if sink is None:
            return self._registry[self._which].flush()
        pending = self._buffer()
        if pending:
            sink(pending)
            self._local.buffer = ""

    def isatty(self):
        return False

    def writable(self):
        return True


def _streams():
    """The console proxy and the table of which thread belongs to which run.

    Process-wide, but never run-specific: an entry is looked up by thread, and
    holds that run's own host, stop exception and sink. The guards below are
    installed once, so they must not close over one run's globals -- a second run
    would then ask the first run's host whether to stop and raise the first run's
    exception class, which the second run's `except` cannot catch.
    """
    with _REGISTRY:
        registry = getattr(sys, "_shuvarie_console", None)
        if registry is None:
            registry = {"threads": {}, "out": sys.stdout, "err": sys.stderr}
            sys.stdout = _Stream(registry, "out")
            sys.stderr = _Stream(registry, "err")
            sys._shuvarie_console = registry
            _install_guards()
        return registry


def _stop_now(entry):
    """Raise this run's `_Stop` if its host has asked the run to end."""
    reason = entry["host"].stop_reason()
    if reason is not None:
        raise entry["stop"](reason)


def _sleep(seconds=0.0):
    """`time.sleep`, except that a stop is delivered while it waits.

    `PyThreadState_SetAsyncExc` interrupts *bytecode*: it does nothing to a thread
    parked in `time.sleep`, which is how a script that waits (a poll interval, a
    retry backoff, a `while time.time() < deadline` loop) becomes a turn that never
    ends -- the tool call stays pending, so the interface stays running and the next
    message is queued behind it. Sleeping in slices instead, and checking the host
    between them, is what makes the budget and an interrupt work on a blocked
    script. Threads that are not running a script keep the real one.
    """
    entry = _streams()["threads"].get(threading.get_ident())
    if entry is None:
        return _REAL_SLEEP(seconds)
    try:
        remaining = float(seconds)
    except (TypeError, ValueError):
        return _REAL_SLEEP(seconds)
    while remaining > 0:
        _stop_now(entry)
        slice_ = _POLL if remaining > _POLL else remaining
        _REAL_SLEEP(slice_)
        remaining -= slice_
    _stop_now(entry)


def _input(*args, **kwargs):
    """`input()` is not a script's to call: there is nobody to answer, and `sys.stdin`
    is the terminal the interface is reading its keys from."""
    if threading.get_ident() not in _streams()["threads"]:
        return _REAL_INPUT(*args, **kwargs)
    raise RuntimeError(
        "a codemode script is not interactive: `input()` has nobody to answer it "
        "and would read the interface's own keys. Take the value from a tool instead."
    )


def _install_guards():
    """Replace the two blocking builtins with thread-dispatched versions."""
    if getattr(sys, "_shuvarie_guards", False):
        return
    sys._shuvarie_guards = True
    time.sleep = _sleep
    builtins.input = _input


def _watch(script_thread, done):
    """Interrupt the script thread once the host says the run should stop.

    A thread of its own because the host state can only be read from Python, and
    because the script thread is the one that has to be interrupted. The
    exception is `_Stop`, which `BaseException`-derived, so a script's own
    `except Exception` cannot swallow the interrupt.
    """
    set_async = ctypes.pythonapi.PyThreadState_SetAsyncExc
    set_async.argtypes = [ctypes.c_ulong, ctypes.py_object]
    set_async.restype = ctypes.c_int
    while not done.wait(_POLL):
        if _HOST.stop_reason() is not None:
            break
    # Re-raised until the thread is actually done: a script that catches
    # `BaseException` in a loop must not win.
    while not done.is_set():
        if set_async(ctypes.c_ulong(script_thread), _Stop) == 0:
            # No such thread: it finished between the check and the call.
            break
        # The runtime's own waiting is never a script's: `time.sleep` is guarded
        # now, and this thread is not the one the guards answer.
        _REAL_SLEEP(_POLL)


def _counter(limit):
    """`max-operations`: count the script's own lines, per line event.

    Installed only when the limit is configured -- a trace hook costs a few times
    the speed of the code it watches, and the default is no limit at all.
    """

    count = 0

    def trace(frame, event, arg):
        nonlocal count
        if event == "call":
            return trace if frame.f_code.co_filename == FILENAME else None
        if event == "line":
            count += 1
            if count > limit:
                raise _Limit(f"the script reached its limit of {limit} operations")
        return trace

    return trace


def _split_last_expression(tree):
    """The script's value is its last expression, as in a REPL."""
    if tree.body and isinstance(tree.body[-1], ast.Expr):
        return tree.body.pop().value
    return _NOTHING


def _locate(failure):
    """The innermost frame of the script itself, and what it was doing."""
    found = (None, None)
    traceback = failure.__traceback__
    while traceback is not None:
        code = traceback.tb_frame.f_code
        if code.co_filename == FILENAME:
            found = (traceback.tb_lineno, code.co_name)
        traceback = traceback.tb_next
    return found


def _describe(failure):
    """A failure as the result block reads it: message, line, column.

    One frame is worth reporting -- the innermost one the script wrote -- and the
    position belongs to the runner's envelope rather than to the message.
    """
    if isinstance(failure, _Stop):
        # Raised between bytecodes, so it can land in the runtime as easily as in
        # the script; the host has the reason and says so itself.
        return "the script was stopped", None, None
    if isinstance(failure, SyntaxError):
        return f"SyntaxError: {failure.msg}", failure.lineno, failure.offset

    line, function = _locate(failure)
    text = str(failure)
    if isinstance(failure, ToolError):
        # Already a whole sentence, written by the host.
        message = text
    else:
        message = f"{type(failure).__name__}: {text}" if text else type(failure).__name__
    if function and function != "<module>":
        message += f" (in `{function}`)"
    return message, line, None


def _succeeded(value):
    if value is _NOTHING:
        return json.dumps({"ok": True, "value": None})
    return json.dumps({"ok": True, "value": _dumps(value)})


def _failed(kind, failure):
    message, line, column = _describe(failure)
    return json.dumps(
        {"ok": False, "kind": kind, "message": message, "line": line, "column": column}
    )


def _execute(script, globals_dict):
    """Compile, run, and report. The envelope is all that crosses back."""
    try:
        tree = compile(script, FILENAME, "exec", ast.PyCF_ONLY_AST, dont_inherit=True)
    except SyntaxError as failure:
        return _failed("parse", failure)

    value = _split_last_expression(tree)
    try:
        exec(compile(tree, FILENAME, "exec", dont_inherit=True), globals_dict)
        if value is _NOTHING:
            return _succeeded(_NOTHING)
        result = eval(compile(ast.Expression(value), FILENAME, "eval", dont_inherit=True), globals_dict)
        return _succeeded(result)
    except _Exit as stopped:
        return _succeeded(stopped.value)
    except SystemExit as stopped:
        # `sys.exit(n)` from a script means the same thing as `exit(n)`.
        return _succeeded(stopped.code)
    except _Limit as failure:
        return _failed("limit", failure)
    except _Stop as failure:
        return _failed("stopped", failure)
    except BaseException as failure:
        return _failed("error", failure)


def start(script, manifest_json, options_json):
    """Run one script against one host, and answer with the result envelope.

    The envelope is JSON text rather than a tuple so that a value the script
    produced never has to cross as a Python object: everything is JSON at this
    boundary, which is what keeps the conversion rules in one place.
    """
    # This run's answer, and nothing shared: two runs are two modules, and the only
    # thing between them is the process-wide console registry.
    holder = []
    registry = _streams()
    try:
        return _run(script, manifest_json, options_json, registry, holder)
    except BaseException as failure:
        # An interrupt is delivered *between bytecodes*, so it can land in this
        # function's own tail as easily as in the script -- and an answer that was
        # already decided is the one that has to come back. The watchdog is joined
        # by the time this runs, so nothing can be raised twice.
        return holder[0] if holder else _failed("stopped", failure)


def _run(script, manifest_json, options_json, registry, holder):
    """`start` with the setup, the script, and the cleanup in one place."""
    manifest = json.loads(manifest_json)
    options = json.loads(options_json)
    limit = options.get("max_operations") or 0
    # A script usually arrives as the value of a JSON string, indented to line up
    # with the JSON around it. Python would call that an unexpected indent, so the
    # common leading whitespace goes -- and only that, because a reported error
    # position has to be the line the script's author can count to.
    script = textwrap.dedent(script)

    globals_dict = {"__name__": "__main__"}
    globals_dict.update(_globals(manifest))

    # This thread's console output goes to the result, not to the terminal, and
    # this thread's guards answer to this run -- so the whole record is registered
    # rather than just the sink.
    threads = registry["threads"]
    threads[threading.get_ident()] = {"host": _HOST, "stop": _Stop, "log": _HOST.log}
    done = threading.Event()
    # This thread's own id, read here rather than in the watchdog: the watchdog
    # can only interrupt a thread it can name, and naming itself is the one answer
    # that never works.
    watchdog = threading.Thread(
        target=_watch,
        args=(threading.get_ident(), done),
        name="codemode-watchdog",
        daemon=True,
    )
    watchdog.start()
    if limit:
        sys.settrace(_counter(limit))
    try:
        # Recorded before the cleanup, because the cleanup is where an interrupt
        # can still land: the answer is decided at this line or not at all.
        holder.append(_execute(script, globals_dict))
    finally:
        # Each step stands alone: a stop delivered mid-cleanup must not skip the
        # ones that matter (this thread stops being a script's, the watchdog is
        # told to finish) -- and every step here is the runtime's own, so it uses
        # the real sleep and never the guarded one.
        _guard(lambda: threads.pop(threading.get_ident(), None))
        _guard(done.set)
        _guard(sys.stdout.flush)
        _guard(sys.stderr.flush)
        # `join` would hold the GIL the watchdog needs to notice `done`, so wait by
        # sleeping, which lets it run. Once this returns, nothing can be raised.
        for _ in range(100):
            if not watchdog.is_alive():
                break
            _guard(lambda: _REAL_SLEEP(0.005))
        _guard(lambda: sys.settrace(None))
        # The guards stay installed process-wide, so this module -- and its globals,
        # which hold `_HOST` -- outlives the run. Dropping the host here is what
        # keeps one script from pinning its run (and, with it, the whole tool roster)
        # for the life of the process.
        _guard(lambda: globals().pop("_HOST", None))
    return holder[0]


def _guard(step):
    """Run one piece of the runtime's own bookkeeping, whatever happens."""
    try:
        step()
    except BaseException:
        pass
