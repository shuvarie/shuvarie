use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use rig_agent::agent::hook::{HookContext, ToolResultEvent};
use rig_agent::agent::{AgentHook, ToolResultAction};
use tokio::sync::mpsc::Sender;

use crate::file_change::{FileChange, ShellStreams};
use crate::stream::StreamItem;

pub use rig_agent::tool::{DynamicTool, Tool, ToolContext, ToolSet};
pub use rig_core::completion::ToolDefinition;
pub use rig_core::tool::{PortableDynamicTool, ToolErrorKind, ToolExecutionError, ToolOutput};

/// Wrap a typed [`Tool`] as a [`DynamicTool`]. The tool reports host-only
/// [`FileChange`]s via [`ToolContext::insert_result`]; a [`FileChangeHook`]
/// attached to the run captures them and forwards them to the stream,
/// correlated by the tool call's `internal_call_id`.
pub fn into_dynamic<T>(name: impl Into<String>, tool: T) -> DynamicTool
where
    T: Tool<Args = serde_json::Value, Output = ToolOutput, Error = ToolExecutionError> + 'static,
{
    let tool = Arc::new(tool);
    DynamicTool::new(
        name,
        tool.description(),
        tool.parameters(),
        move |ctx, args| {
            let tool = Arc::clone(&tool);
            Box::pin(async move { tool.call(ctx, args).await })
        },
    )
}

/// Captures host-only result metadata that tools attach to their
/// [`ToolContext`] — [`FileChange`]s from the file tools and
/// [`ShellStreams`] from `run_shell` — keyed by the tool call's
/// `internal_call_id` so the stream can correlate them with the
/// corresponding `ToolResult`. The structured failure disposition of the
/// raw result is captured alongside so `ok` does not depend on sniffing
/// the result text.
///
/// When wired with an early-finish channel, the hook additionally surfaces
/// each tool result the moment its call completes — rig only surfaces the
/// buffered `ToolResult` stream items once the whole batch settles, which
/// would otherwise leave every block of the batch spinning behind the
/// slowest call. The surfaced calls are recorded so the stream can drop
/// the later buffered duplicates.
#[derive(Clone, Default)]
pub struct FileChangeHook {
    inner: Arc<Mutex<HookState>>,
}

#[derive(Default)]
struct HookState {
    changes: HashMap<String, CapturedResult>,
    surfaced: HashSet<String>,
    finish: Option<EarlyFinish>,
}

#[derive(Clone)]
struct EarlyFinish {
    /// Worker-tool names of the main agent — their calls surface as
    /// `WorkerResult` instead of `ToolResult`.
    worker_names: HashSet<String>,
    /// The owning worker agent's name for a worker-agent hook: its own tool
    /// calls surface as `ToolResult { worker: Some(name) }`.
    worker: Option<String>,
    tx: Sender<StreamItem>,
}

#[derive(Debug, Default)]
pub struct CapturedResult {
    pub file_change: Option<FileChange>,
    pub shell: Option<ShellStreams>,
    pub failed: bool,
}

impl FileChangeHook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the result metadata recorded for a tool call, if any.
    pub fn take(&self, internal_call_id: &str) -> CapturedResult {
        self.inner
            .lock()
            .unwrap()
            .changes
            .remove(internal_call_id)
            .unwrap_or_default()
    }

    /// Surface each tool result to `tx` the moment its call completes,
    /// instead of waiting for rig's per-batch buffering to flush the
    /// surfaced `ToolResult` items after the whole batch settles. The send
    /// is awaited so items appended through the same channel (the worker
    /// activity channel) keep their relative order.
    ///
    /// `worker_names` marks worker-tool calls in the main agent — surfaced
    /// as `WorkerResult`; `worker` names the owning agent for a
    /// worker-agent hook, so its own tool calls surface as
    /// `ToolResult { worker }`.
    pub fn with_early_finish(
        self,
        worker_names: HashSet<String>,
        worker: Option<String>,
        tx: Sender<StreamItem>,
    ) -> Self {
        self.inner.lock().unwrap().finish = Some(EarlyFinish {
            worker_names,
            worker,
            tx,
        });
        self
    }

    /// Whether a call already surfaced through the early-finish channel.
    pub fn surfaced_early(&self, internal_call_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .surfaced
            .contains(internal_call_id)
    }
}

/// Build the stream item surfaced at hook time — the same output text and
/// `ok` verdict the buffered `ToolResult` mapping derives later, so early
/// and late surfacing agree byte for byte.
fn early_stream_item(
    finish: &EarlyFinish,
    event: &ToolResultEvent<'_>,
    captured: &CapturedResult,
) -> StreamItem {
    let mut output = String::new();
    for content in event.presentation.as_content() {
        if let Some(text) = content.as_text() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(text);
        }
    }
    let mut ok = !captured.failed;
    if output.is_empty() {
        ok = false;
        output = String::from("(no output)");
    }
    let name = event.tool_name.to_string();
    let call_id = event.internal_call_id.to_string();
    match &finish.worker {
        Some(worker) => StreamItem::ToolResult {
            name,
            output,
            ok,
            worker: Some(worker.clone()),
            file_change: captured.file_change.clone(),
            streams: captured.shell.clone(),
            call_id,
        },
        None if finish.worker_names.contains(event.tool_name) => StreamItem::WorkerResult {
            name,
            output,
            ok,
            call_id,
        },
        None => StreamItem::ToolResult {
            name,
            output,
            ok,
            worker: None,
            file_change: captured.file_change.clone(),
            streams: captured.shell.clone(),
            call_id,
        },
    }
}

impl AgentHook for FileChangeHook {
    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl futures_util::Future<Output = ToolResultAction> + Send {
        let mut captured = CapturedResult {
            failed: event.raw_result.is_error(),
            ..CapturedResult::default()
        };
        if let Some(change) = event.tool_context.result::<FileChange>() {
            captured.file_change = Some(change.clone());
        }
        if let Some(shell) = event.tool_context.result::<ShellStreams>() {
            captured.shell = Some(shell.clone());
        }
        let mut inner = self.inner.lock().unwrap();
        let (tx, item) = match inner.finish.clone() {
            Some(finish) => {
                let item = early_stream_item(&finish, &event, &captured);
                inner.surfaced.insert(event.internal_call_id.to_string());
                (Some(finish.tx), Some(item))
            }
            None => {
                if captured.file_change.is_some() || captured.shell.is_some() || captured.failed {
                    inner
                        .changes
                        .insert(event.internal_call_id.to_string(), captured);
                }
                (None, None)
            }
        };
        drop(inner);
        async move {
            if let (Some(tx), Some(item)) = (tx, item) {
                let _ = tx.send(item).await;
            }
            ToolResultAction::Keep
        }
    }
}
