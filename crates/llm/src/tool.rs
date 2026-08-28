use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rig::agent::hook::{HookContext, ToolResultEvent};
use rig::agent::{AgentHook, ToolResultAction};

use crate::file_change::FileChange;

pub use rig::completion::ToolDefinition;
pub use rig::tool::{
    DynamicTool, PortableDynamicTool, Tool, ToolContext, ToolErrorKind, ToolExecutionError,
    ToolOutput, ToolSet,
};

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

/// Captures host-only [`FileChange`]s that tools attach to their
/// [`ToolContext`], keyed by the tool call's `internal_call_id` so the stream
/// can correlate them with the corresponding `ToolResult`.
#[derive(Clone, Default)]
pub struct FileChangeHook {
    changes: Arc<Mutex<HashMap<String, FileChange>>>,
}

impl FileChangeHook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the `FileChange` recorded for a tool call, if any.
    pub fn take(&self, internal_call_id: &str) -> Option<FileChange> {
        self.changes.lock().unwrap().remove(internal_call_id)
    }
}

impl AgentHook for FileChangeHook {
    fn on_tool_result(
        &self,
        _ctx: &HookContext,
        event: ToolResultEvent<'_>,
    ) -> impl futures_util::Future<Output = ToolResultAction> + Send {
        if let Some(change) = event.tool_context.result::<FileChange>() {
            self.changes
                .lock()
                .unwrap()
                .insert(event.internal_call_id.to_string(), change.clone());
        }
        async { ToolResultAction::Keep }
    }
}
