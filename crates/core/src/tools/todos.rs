//! The session todo list: the `todo` agent tool plus its shared state.
//!
//! Todos are derived state — the list is never persisted on its own. It is
//! replayed from the persisted `todo` tool-call records whenever a session is
//! loaded, undone, redone, or resumed, and a fresh state is computed at every
//! send from the session's tool records.

use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shuvarie_llm::{Tool, ToolContext, ToolExecutionError, ToolOutput};

use crate::tool_record::ToolRecord;

const MAX_TODOS: usize = 100;
const MAX_TEXT_CHARS: usize = 500;
const TOOL_NAME: &str = "todo";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl TodoStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Done => "done",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" => Some(TodoStatus::InProgress),
            "done" => Some(TodoStatus::Done),
            _ => None,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            TodoStatus::Pending => " ",
            TodoStatus::InProgress => "~",
            TodoStatus::Done => "x",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub id: u64,
    pub text: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TodoOp {
    Add {
        text: String,
        status: TodoStatus,
    },
    Update {
        id: u64,
        text: Option<String>,
        status: Option<TodoStatus>,
    },
    Remove {
        id: u64,
    },
    List,
}

pub struct TodoOutcome {
    pub summary: String,
    pub items: Vec<TodoItem>,
}

/// Parse the tool arguments into a [`TodoOp`]. Lenient about missing optional
/// fields, strict about the ones the operation needs.
pub fn parse_op(args: &Value) -> Result<TodoOp, String> {
    let op = args
        .get("op")
        .and_then(Value::as_str)
        .ok_or("missing string argument 'op'")?;
    match op {
        "add" => {
            let text = clean_text(str_arg(args, "text")?)?;
            let status = status_arg(args)?.unwrap_or(TodoStatus::Pending);
            Ok(TodoOp::Add { text, status })
        }
        "update" => {
            let id = int_arg(args, "id")?;
            let text = match args.get("text") {
                Some(Value::String(raw)) => Some(clean_text(raw)?),
                Some(Value::Null) | None => None,
                Some(_) => return Err("argument 'text' must be a string".into()),
            };
            let status = status_arg(args)?;
            if text.is_none() && status.is_none() {
                return Err("update needs a 'text' or 'status' argument".into());
            }
            Ok(TodoOp::Update { id, text, status })
        }
        "remove" => Ok(TodoOp::Remove {
            id: int_arg(args, "id")?,
        }),
        "list" => Ok(TodoOp::List),
        other => Err(format!(
            "unknown op '{other}' (expected add, update, remove, or list)"
        )),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string argument '{key}'"))
}

fn int_arg(args: &Value, key: &str) -> Result<u64, String> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing integer argument '{key}'"))
}

fn status_arg(args: &Value) -> Result<Option<TodoStatus>, String> {
    match args.get("status") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(raw)) => TodoStatus::parse(raw).map(Some).ok_or_else(|| {
            format!("unknown status '{raw}' (expected pending, in_progress, or done)")
        }),
        Some(_) => Err("argument 'status' must be a string".into()),
    }
}

fn clean_text(raw: &str) -> Result<String, String> {
    let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("todo text cannot be empty".into());
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return Err(format!("todo text exceeds {MAX_TEXT_CHARS} chars"));
    }
    Ok(text)
}

/// Apply one operation to the item list. `Ok(None)` for no-op ops (`list`).
fn apply_op(
    items: &mut Vec<TodoItem>,
    next_id: &mut u64,
    op: &TodoOp,
) -> Result<Option<String>, String> {
    match op {
        TodoOp::Add { text, status } => {
            if items.len() >= MAX_TODOS {
                return Err(format!("the todo list is full ({MAX_TODOS} items)"));
            }
            let id = *next_id;
            *next_id += 1;
            items.push(TodoItem {
                id,
                text: text.clone(),
                status: *status,
            });
            Ok(Some(format!("Added #{id} \"{text}\"")))
        }
        TodoOp::Update { id, text, status } => {
            let item = items
                .iter_mut()
                .find(|i| i.id == *id)
                .ok_or_else(|| format!("unknown todo id {id}"))?;
            let mut parts: Vec<String> = Vec::new();
            if let Some(t) = text {
                item.text = t.clone();
                parts.push(format!("text \"{t}\""));
            }
            if let Some(s) = status {
                item.status = *s;
                parts.push(format!("status {}", s.as_str()));
            }
            Ok(Some(format!("Updated #{id} ({})", parts.join(", "))))
        }
        TodoOp::Remove { id } => {
            let before = items.len();
            items.retain(|i| i.id != *id);
            if items.len() == before {
                return Err(format!("unknown todo id {id}"));
            }
            Ok(Some(format!("Removed #{id}")))
        }
        TodoOp::List => Ok(None),
    }
}

/// The number of done items and the total.
pub fn done_total(items: &[TodoItem]) -> (usize, usize) {
    (
        items
            .iter()
            .filter(|i| i.status == TodoStatus::Done)
            .count(),
        items.len(),
    )
}

/// The list section of the tool output: a counts header plus one row per
/// item. Doubles as the display format the TUI parses back into items.
pub fn format_list(items: &[TodoItem]) -> String {
    if items.is_empty() {
        return "Todos (none)".to_string();
    }
    let (done, total) = done_total(items);
    let mut out = format!("Todos ({done}/{total} done)");
    for item in items {
        out.push_str(&format!(
            "\n  #{} [{}] {}",
            item.id,
            item.status.glyph(),
            item.text
        ));
    }
    out
}

fn format_outcome(outcome: &TodoOutcome) -> String {
    let list = format_list(&outcome.items);
    if outcome.summary.is_empty() {
        list
    } else {
        format!("{}\n\n{list}", outcome.summary)
    }
}

/// Parse a rendered todo list back into items. Returns `None` when the output
/// carries no item rows (empty list, or a failed call's error text).
pub fn parse_items(output: &str) -> Option<Vec<TodoItem>> {
    let items: Vec<TodoItem> = output.lines().filter_map(parse_item_line).collect();
    (!items.is_empty()).then_some(items)
}

fn parse_item_line(line: &str) -> Option<TodoItem> {
    let line = line.trim_start();
    let rest = line.strip_prefix('#')?;
    let (id_str, rest) = rest.split_once(' ')?;
    let id = id_str.parse().ok()?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('[')?;
    let (glyph, rest) = rest.split_once(']')?;
    let status = match glyph {
        "x" => TodoStatus::Done,
        "~" => TodoStatus::InProgress,
        " " => TodoStatus::Pending,
        _ => return None,
    };
    Some(TodoItem {
        id,
        text: rest.trim_start().to_string(),
        status,
    })
}

/// Rebuild the todo list by replaying persisted `todo` tool calls in order.
/// Failed calls and worker-tagged records are skipped.
pub fn replay(records: &[ToolRecord]) -> Vec<TodoItem> {
    let mut items: Vec<TodoItem> = Vec::new();
    let mut next_id = 1u64;
    for record in records {
        if record.name != TOOL_NAME || !record.ok || record.worker.is_some() {
            continue;
        }
        let Ok(args) = serde_json::from_str::<Value>(&record.args_json) else {
            continue;
        };
        let Ok(op) = parse_op(&args) else {
            continue;
        };
        let _ = apply_op(&mut items, &mut next_id, &op);
    }
    items
}

/// Shared todo list state for the running session: mutated by the `todo` tool
/// during a turn, rebuilt from the session's tool records at every send.
#[derive(Debug, Clone)]
pub struct TodoState {
    inner: Arc<Mutex<TodoInner>>,
}

#[derive(Debug)]
struct TodoInner {
    items: Vec<TodoItem>,
    next_id: u64,
}

impl TodoInner {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            next_id: 1,
        }
    }
}

impl Default for TodoState {
    fn default() -> Self {
        Self::new()
    }
}

impl TodoState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TodoInner::new())),
        }
    }

    pub fn from_records(records: &[ToolRecord]) -> Self {
        let items = replay(records);
        let next_id = items.iter().map(|i| i.id + 1).max().unwrap_or(1);
        Self {
            inner: Arc::new(Mutex::new(TodoInner { items, next_id })),
        }
    }

    pub fn apply(&self, op: TodoOp) -> Result<TodoOutcome, String> {
        let mut inner = self.inner.lock().unwrap();
        let mut items = inner.items.clone();
        let summary = apply_op(&mut items, &mut inner.next_id, &op)?;
        inner.items = items.clone();
        let summary = summary.unwrap_or_default();
        Ok(TodoOutcome { summary, items })
    }

    pub fn items(&self) -> Vec<TodoItem> {
        self.inner.lock().unwrap().items.clone()
    }
}

pub struct Todo {
    state: TodoState,
}

impl Todo {
    pub fn new(state: TodoState) -> Self {
        Self { state }
    }
}

impl Tool for Todo {
    const NAME: &'static str = "todo";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Maintain the session todo list that is surfaced to the user in the chat pane and the \
         sidebar. Operations: add a todo (text, optional status), update one by id (new text \
         and/or status), remove one by id, or list all. Statuses: pending, in_progress, done. \
         Use it for multi-step work: add a todo per step, keep exactly one in_progress while \
         you work on it, and mark each done as soon as it is finished. Returns the full list \
         after every mutation."
            .to_string()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["add", "update", "remove", "list"], "description": "Operation to perform" },
                "text": { "type": "string", "description": "Todo text (add, or update to retitle)" },
                "id": { "type": "integer", "minimum": 1, "description": "Todo id (update/remove)" },
                "status": { "type": "string", "enum": ["pending", "in_progress", "done"], "description": "Status to set (add: defaults to pending; update: optional)" }
            },
            "required": ["op"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let op = parse_op(&args).map_err(ToolExecutionError::other)?;
        let outcome = self.state.apply(op).map_err(ToolExecutionError::other)?;
        Ok(ToolOutput::text(format_outcome(&outcome)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_record::ToolRecord;

    fn op_value(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    fn add(text: &str) -> TodoOp {
        TodoOp::Add {
            text: text.into(),
            status: TodoStatus::Pending,
        }
    }

    #[test]
    fn parse_op_add_defaults_to_pending() {
        let op = parse_op(&op_value(r#"{"op":"add","text":" write  tests "}"#)).unwrap();
        assert_eq!(op, add("write tests"));
    }

    #[test]
    fn parse_op_add_with_status() {
        let op = parse_op(&op_value(
            r#"{"op":"add","text":"x","status":"in_progress"}"#,
        ))
        .unwrap();
        assert_eq!(
            op,
            TodoOp::Add {
                text: "x".into(),
                status: TodoStatus::InProgress
            }
        );
    }

    #[test]
    fn parse_op_update_requires_a_field() {
        assert!(parse_op(&op_value(r#"{"op":"update","id":2}"#)).is_err());
        let op = parse_op(&op_value(r#"{"op":"update","id":2,"status":"done"}"#)).unwrap();
        assert_eq!(
            op,
            TodoOp::Update {
                id: 2,
                text: None,
                status: Some(TodoStatus::Done)
            }
        );
    }

    #[test]
    fn parse_op_rejects_unknown() {
        assert!(parse_op(&op_value(r#"{"op":"nope"}"#)).is_err());
        assert!(parse_op(&op_value(r#"{"text":"x"}"#)).is_err());
        assert!(parse_op(&op_value(r#"{"op":"add"}"#)).is_err());
    }

    #[test]
    fn apply_round_trip() {
        let state = TodoState::new();
        let out = state.apply(add("one")).unwrap();
        assert_eq!(out.summary, "Added #1 \"one\"");
        assert_eq!(out.items.len(), 1);
        let out = state
            .apply(TodoOp::Update {
                id: 1,
                text: None,
                status: Some(TodoStatus::Done),
            })
            .unwrap();
        assert_eq!(out.items[0].status, TodoStatus::Done);
        assert_eq!(done_total(&out.items), (1, 1));
        let out = state.apply(TodoOp::Remove { id: 1 }).unwrap();
        assert!(out.items.is_empty());
        assert!(state.apply(TodoOp::Remove { id: 1 }).is_err());
    }

    #[test]
    fn ids_stay_monotonic_after_remove() {
        let state = TodoState::new();
        state.apply(add("a")).unwrap();
        state.apply(TodoOp::Remove { id: 1 }).unwrap();
        let out = state.apply(add("b")).unwrap();
        assert_eq!(out.items[0].id, 2);
    }

    #[test]
    fn format_then_parse_round_trips() {
        let state = TodoState::new();
        state
            .apply(TodoOp::Add {
                text: "set up schema".into(),
                status: TodoStatus::Done,
            })
            .unwrap();
        state
            .apply(TodoOp::Add {
                text: "write migration".into(),
                status: TodoStatus::InProgress,
            })
            .unwrap();
        state.apply(add("update UI")).unwrap();
        let items = state.items();
        let parsed = parse_items(&format_list(&items)).unwrap();
        assert_eq!(parsed, items);
    }

    #[test]
    fn parse_items_returns_none_for_other_output() {
        assert!(parse_items("unknown todo id 7").is_none());
        assert!(parse_items("Todos (none)").is_none());
    }

    fn record(name: &str, args_json: &str, ok: bool, worker: Option<String>) -> ToolRecord {
        ToolRecord {
            name: name.into(),
            args_json: args_json.into(),
            output: String::new(),
            stderr: String::new(),
            ok,
            worker,
            message_id: 1,
            message_seq: 0,
            file_change: None,
            original_content: None,
            new_content: None,
            duration_ms: 0,
        }
    }

    #[test]
    fn replay_applies_successful_calls_in_order() {
        let records = vec![
            record("todo", r#"{"op":"add","text":"a"}"#, true, None),
            record("read_file", r#"{"path":"x"}"#, true, None),
            record(
                "todo",
                r#"{"op":"update","id":1,"status":"done"}"#,
                true,
                None,
            ),
            record("todo", r#"{"op":"add","text":"bad"}"#, false, None),
            record(
                "todo",
                r#"{"op":"add","text":"worker"}"#,
                true,
                Some("edit_files".into()),
            ),
        ];
        let state = TodoState::from_records(&records);
        let items = state.items();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, 1);
        assert_eq!(items[0].status, TodoStatus::Done);
        assert_eq!(state.inner.lock().unwrap().next_id, 2);
    }
}
