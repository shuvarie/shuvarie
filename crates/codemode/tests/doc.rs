//! Docs as code: every fenced snippet in the dialect card is executed here.
//!
//! The card is how the model learns Rhai, so a snippet that stops parsing is a
//! broken feature, not a typo -- and the traps are described in prose precisely
//! because they are the mistakes a reader would otherwise make. The rule that
//! makes this test possible: fenced snippets run, inline code spans do not, so
//! a *counter*-example is written inline and a *pattern* is fenced.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shuvarie_codemode::doc;
use shuvarie_codemode::script::InvokeFuture;
use shuvarie_codemode::{
    Catalog, Limits, ScriptRequest, Stop, ToolCall, ToolEntry, ToolInvoker, run,
};

/// Every ```` ```rhai ```` block in a document, in order.
fn snippets(document: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut lines = document.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() != "```rhai" {
            continue;
        }
        let mut snippet = String::new();
        for line in lines.by_ref() {
            if line.trim() == "```" {
                break;
            }
            snippet.push_str(line);
            snippet.push('\n');
        }
        found.push(snippet);
    }
    found
}

/// A roster whose tools return text, like the real ones do.
struct Stub {
    replies: HashMap<String, Value>,
    calls: Mutex<Vec<String>>,
}

impl ToolInvoker for Stub {
    fn invoke(&self, call: ToolCall) -> InvokeFuture<'_> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(call.host_name.clone());
            Ok(self
                .replies
                .get(&call.host_name)
                .cloned()
                .unwrap_or(Value::Null))
        })
    }
}

fn roster() -> Arc<Stub> {
    let replies = [
        ("read_file", json!("fn main() {}\n")),
        (
            "grep",
            json!("src/main.rs:1:fn main() {}\nsrc/lib.rs:2:fn helper() {}"),
        ),
        ("glob", json!("src/main.rs\nsrc/lib.rs\n")),
        (
            "todos",
            json!([{ "id": 1, "text": "ship it", "done": false }]),
        ),
    ];
    Arc::new(Stub {
        replies: replies
            .into_iter()
            .map(|(name, reply)| (name.to_string(), reply))
            .collect(),
        calls: Mutex::new(Vec::new()),
    })
}

fn catalog() -> Arc<Catalog> {
    Arc::new(Catalog::new([
        ToolEntry::roster("read_file", "Read a file.", json!({})),
        ToolEntry::roster("grep", "Search contents.", json!({})),
        ToolEntry::roster("glob", "List matching paths.", json!({})),
        ToolEntry::roster("todos", "The todo list.", json!({})),
    ]))
}

/// Run a snippet the way the tool would, and report the engine's own verdict.
async fn execute(snippet: &str) -> Result<(), String> {
    let outcome = run(
        ScriptRequest {
            script: snippet.to_string(),
            catalog: catalog(),
            store: Some(Arc::new(shuvarie_codemode::Store::new())),
            limits: Limits::default(),
        },
        roster(),
        Stop::new(),
    )
    .await;

    match outcome.result {
        Ok(_) => Ok(()),
        Err(error) => Err(format!("{}: {}", error.message, snippet)),
    }
}

#[tokio::test]
async fn the_dialect_card_has_executable_snippets() {
    let snippets = snippets(doc::CARD);
    assert!(
        snippets.len() >= 4,
        "the card should keep its worked examples: found {}",
        snippets.len()
    );
    // The globals a script needs are only taught if the card shows them, and a
    // snippet is the only way the card can show something that is known to run.
    for global in ["search_tools", "ALL_TOOLS", "describe_tool"] {
        assert!(
            doc::CARD.contains(global),
            "the card no longer mentions {global}"
        );
    }
    assert!(
        snippets
            .iter()
            .any(|snippet| snippet.contains("search_tools")),
        "finding a tool should be a snippet, not just prose"
    );
}

#[tokio::test]
async fn every_snippet_in_the_dialect_card_runs() {
    for snippet in snippets(doc::CARD) {
        if let Err(error) = execute(&snippet).await {
            panic!("a snippet in the dialect card does not run:\n{error}");
        }
    }
}

/// The built-in skill is the same kind of document as the card -- prose that
/// teaches a language by example -- so it is held to the same rule.
#[tokio::test]
async fn every_snippet_in_the_builtin_skill_runs() {
    let snippets = snippets(shuvarie_codemode::skill::BODY);
    assert!(
        snippets.len() >= 8,
        "the skill should carry worked examples: found {}",
        snippets.len()
    );
    for snippet in snippets {
        if let Err(error) = execute(&snippet).await {
            panic!("a snippet in the built-in skill does not run:\n{error}");
        }
    }
}
