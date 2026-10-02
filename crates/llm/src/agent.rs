use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

use crate::TokenUsage;

use crate::ProviderClient;
use crate::stream::StreamItem;
use crate::tool::{Tool, ToolContext, ToolExecutionError, ToolOutput};

/// Host-only per-dispatch metadata the worker tool attaches to its
/// `ToolContext`: the spawn id it minted for the run. The main agent's hooks
/// read it at tool-result time to tag the surfaced worker results
/// ([`StreamItem::WorkerResult`]) with the spawn, so consumers can group the
/// run's activity items ([`StreamItem::ToolStart`] and
/// [`StreamItem::ToolResult`], tagged with the same id) onto the right spawn
/// when one worker is spawned several times in a batch.
///
/// A [`ContextValue`] keyed by the type name, traveling as host-only result
/// metadata on the [`ToolContext`], which the hooks read at tool-result time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, rig_core::ContextValue)]
pub struct SpawnTag(pub u64);

/// The process-wide spawn id counter: every worker run mints one id, unique
/// across the session — even across concurrent batches of clones of one
/// worker.
static SPAWN_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_spawn_id() -> u64 {
    SPAWN_SEQ.fetch_add(1, Ordering::Relaxed)
}

pub struct WorkerAgent {
    name: String,
    description: String,
    preamble: String,
    client: ProviderClient,
    model: String,
    tools: Vec<crate::DynamicTool>,
    activity_tx: mpsc::Sender<StreamItem>,
    activity_rx: Option<mpsc::Receiver<StreamItem>>,
    usage: Arc<std::sync::Mutex<TokenUsage>>,
    max_turns: usize,
    context_budget: Option<crate::context_hook::ContextBudget>,
}

pub struct WorkerRequest {
    pub client: ProviderClient,
    pub name: String,
    /// The spawn id the tool minted for this run: every activity item of the
    /// run carries it, so consumers group the run's output onto the right
    /// spawn when one worker is spawned concurrently.
    pub spawn: u64,
    pub model: String,
    pub preamble: String,
    pub task: String,
    pub tools: Vec<crate::DynamicTool>,
    pub activity_tx: mpsc::Sender<StreamItem>,
    pub usage: Arc<std::sync::Mutex<TokenUsage>>,
    pub max_turns: usize,
    pub context_budget: Option<crate::context_hook::ContextBudget>,
}

impl WorkerAgent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: &str,
        description: &str,
        preamble: &str,
        client: ProviderClient,
        model: &str,
        tools: Vec<crate::DynamicTool>,
        usage: Arc<std::sync::Mutex<TokenUsage>>,
        max_turns: usize,
        context_budget: Option<crate::context_hook::ContextBudget>,
    ) -> Self {
        let (activity_tx, activity_rx) = mpsc::channel(64);
        Self {
            name: name.to_string(),
            description: description.to_string(),
            preamble: preamble.to_string(),
            client,
            model: model.to_string(),
            tools,
            activity_tx,
            activity_rx: Some(activity_rx),
            usage,
            max_turns,
            context_budget,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The worker's system prompt, as passed to each request.
    pub fn preamble(&self) -> &str {
        &self.preamble
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub fn take_activity_receiver(&mut self) -> Option<mpsc::Receiver<StreamItem>> {
        self.activity_rx.take()
    }

    pub fn usage_shared(&self) -> Arc<std::sync::Mutex<TokenUsage>> {
        Arc::clone(&self.usage)
    }
}

impl Clone for WorkerAgent {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            preamble: self.preamble.clone(),
            client: self.client.clone(),
            model: self.model.clone(),
            tools: self.tools.clone(),
            activity_tx: self.activity_tx.clone(),
            activity_rx: None,
            usage: Arc::clone(&self.usage),
            max_turns: self.max_turns,
            context_budget: self.context_budget.clone(),
        }
    }
}

impl Tool for WorkerAgent {
    const NAME: &'static str = "worker";

    type Args = Value;
    type Output = ToolOutput;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task for the worker to carry out"
                }
            },
            "required": ["task"]
        })
    }

    async fn call(
        &self,
        ctx: &mut ToolContext,
        args: Value,
    ) -> Result<ToolOutput, ToolExecutionError> {
        let task = args
            .get("task")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if task.is_empty() {
            return Err(ToolExecutionError::invalid_args(format!(
                "worker '{}' missing string argument 'task'",
                self.name
            )));
        }
        // Mint this run's spawn id and hand it out both ways: as the request
        // field (the run's activity items are tagged with it) and as host-only
        // metadata the main agent's hook reads at result time (so the surfaced
        // worker result carries it too).
        let spawn = next_spawn_id();
        if ctx.insert_result(SpawnTag(spawn)).is_err() {
            return Err(ToolExecutionError::other(
                "worker spawn could not be recorded",
            ));
        }
        let request = WorkerRequest {
            client: self.client.clone(),
            name: self.name.clone(),
            spawn,
            model: self.model.clone(),
            preamble: self.preamble.clone(),
            task,
            tools: self.tools.clone(),
            activity_tx: self.activity_tx.clone(),
            usage: Arc::clone(&self.usage),
            max_turns: self.max_turns,
            context_budget: self.context_budget.clone(),
        };
        self.client
            .run_worker(&request)
            .await
            .map(ToolOutput::text)
            .map_err(ToolExecutionError::other)
    }
}
