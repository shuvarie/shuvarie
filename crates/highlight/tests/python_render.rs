//! Rendering a Python script must terminate, whatever the script looks like.
//!
//! A `codemode` block highlights its script with syntect's Python grammar, and
//! the block shows a *truncated* script when it is long — which can cut a
//! triple-quoted string, a bracket, or a line in half. A grammar left in an open
//! context must still finish its own line: the TUI renders on the thread that
//! reads events, so a parse that does not terminate is a frozen interface, not a
//! slow frame.

#[test]
fn truncated_and_malformed_scripts_still_render() {
    let scripts = [
        "print(sum(range(10)))\n",
        // Truncated inside a triple-quoted string: the shape a cut-off block
        // produces when the model wrote a docstring.
        "x = 1\ny = \"\"\"unterminated docstring\nprint(1)\n",
        "x = 1\ny = '''unterminated\n\n",
        // Truncated mid-expression.
        "print(sum(range(",
        "paths = [\n    'a',\n    'b',\n",
        // The `__future__`-era oddities that confuse line-oriented grammars.
        "f\"{x}\" if x else f'{y!r}'\n",
    ];
    for script in scripts {
        let started = std::time::Instant::now();
        let lines = shuvarie_highlight::syntax::highlight_code("python", script);
        assert!(!lines.is_empty(), "{script:?} rendered nothing");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{script:?} took {:?} to render",
            started.elapsed()
        );
    }
}

/// The block caps a long script, so the truncated tail is exactly what reaches the
/// highlighter: the parse has to be cheap as well as finite.
#[test]
fn a_long_script_renders_linearly() {
    let script: String = (0..400)
        .map(|index| format!("value_{index} = tools.read_file(path=\"f{index}.rs\")\n"))
        .collect();
    let started = std::time::Instant::now();
    let lines = shuvarie_highlight::syntax::highlight_code("python", &script);
    assert!(lines.len() >= 400, "{}", lines.len());
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "400 lines took {:?}",
        started.elapsed()
    );
}
