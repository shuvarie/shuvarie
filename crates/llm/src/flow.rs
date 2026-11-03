//! The control surface over one rig agent run.
//!
//! A run is otherwise opaque to the caller: the core sees a stream of
//! [`StreamItem`](crate::StreamItem)s and, when that stream dies, has no handle
//! on the conversation rig had built. [`FlowControl`] is that handle. It
//! carries a cooperative cancel flag, the run's phase, and a [`Ledger`] of the
//! requests the provider actually saw and the messages the run committed.
//!
//! Cancellation is cooperative. [`LedgerHook`] checks the flag at rig's
//! boundaries (`on_run_start`, `on_completion_call`, the delta observers, and
//! `on_dispatch`) and stops the run there. The terminal error is then
//! `PromptError::Cancelled`, which the provider bridge maps to
//! [`StreamItem::Cancelled`](crate::StreamItem::Cancelled) — a normal end, not
//! a retryable provider failure. A wedged tool that never reaches a boundary
//! still needs the caller's abort as a last resort; the handle does not pretend
//! otherwise.
//!
//! The ledger is deliberately small in this phase. It records each
//! `on_completion_call` snapshot (the prompt and the history that hook saw),
//! every tool call opened and closed, and the messages rig committed at
//! `on_run_settled` — which rig provides on failure as well as on success. The
//! next phase turns that settled conversation into the store's projection
//! instead of reconstructing it from stream items.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use rig_agent::agent::hook::{CompletionCallEvent, OutcomeEvent};
use rig_agent::agent::{
    AgentHook, CompletionCallAction, DispatchAction, DispatchEvent, HookContext, ModelTurnAction,
    ModelTurnFinished, ObservationAction, OutcomeAction, ReasoningDelta, RunSettled, RunStart,
    RunStartAction, StepEventKind, TextDelta, ToolCallDelta,
};
use rig_core::error::{ErrorKind, ErrorReport};
use rig_core::message::Message;
use serde::{Deserialize, Serialize};

/// The reason a hook stops a run. Kept in one place so the bridge can recognize
/// a cooperative cancel without matching on prose from every call site.
pub const CANCEL_REASON: &str = "cancelled by the user";

const RUNNING: u8 = 0;
const FINISHING: u8 = 1;
const SETTLED: u8 = 2;

/// Where a controlled run is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlowPhase {
    /// The run is producing output and may be cancelled at the next boundary.
    Running,
    /// The final response has arrived (or the caller is committing); the run
    /// must be allowed to finish rather than aborted.
    Finishing,
    /// The run has settled and no longer holds any work.
    Settled,
}

impl FlowPhase {
    fn decode(value: u8) -> Self {
        match value {
            FINISHING => Self::Finishing,
            SETTLED => Self::Settled,
            _ => Self::Running,
        }
    }
}

/// One request the provider was asked to complete, as the hook saw it.
///
/// The history is the one passed to `on_completion_call`. Note that a
/// `HookStack` shows every hook the *unpatched* event and merges their
/// `RequestPatch`es afterwards, so a mechanical trim applied by another hook is
/// not reflected here.
#[derive(Debug, Clone)]
pub struct RequestSnapshot {
    /// One-based model-call index within the run.
    pub turn: usize,
    /// The request's own last message.
    pub prompt: Message,
    /// The history preceding it.
    pub history: Vec<Arc<Message>>,
}

/// A tool call the run opened, and whether a result closed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordedCall {
    /// The provider call id (rig's `CallId` rendered).
    pub call: String,
    /// The tool (or worker) the call named.
    pub name: String,
    /// Whether an outcome closed the call. A call still open when the run ends
    /// is the one a resume has to answer.
    pub answered: bool,
    /// The call's error status once answered.
    pub is_error: Option<bool>,
}

/// What a run recorded: the requests it made, the calls it opened, and the
/// conversation it committed.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    /// Requests in call order.
    pub requests: Vec<RequestSnapshot>,
    /// Tool calls in dispatch order.
    pub calls: Vec<RecordedCall>,
    /// Messages committed by the run, prompt included. `None` until the run
    /// settles; available on failure as well as on success.
    pub settled: Option<Vec<Arc<Message>>>,
}

/// A shared handle to one run's control state and ledger.
///
/// Cloning is cheap and every clone sees the same state, so the run loop can
/// hold one while the hook holds another.
#[derive(Clone, Default)]
pub struct FlowControl {
    inner: Arc<FlowInner>,
}

#[derive(Default)]
struct FlowInner {
    cancel: AtomicBool,
    phase: AtomicU8,
    ledger: Mutex<Ledger>,
}

impl FlowControl {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the run to stop at its next boundary. Idempotent and safe from any
    /// thread; a run already past its last boundary simply finishes.
    pub fn cancel(&self) {
        self.inner.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.inner.cancel.load(Ordering::SeqCst)
    }

    pub fn phase(&self) -> FlowPhase {
        FlowPhase::decode(self.inner.phase.load(Ordering::SeqCst))
    }

    /// Mark the run as past its cancellable work. Called when the final
    /// response arrives, before the caller's commit sequence.
    pub fn mark_finishing(&self) {
        let _ = self.inner.phase.compare_exchange(
            RUNNING,
            FINISHING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub fn is_settled(&self) -> bool {
        self.phase() == FlowPhase::Settled
    }

    /// A snapshot of the ledger. Cheap enough to take at a terminal boundary.
    pub fn snapshot(&self) -> Ledger {
        self.lock().clone()
    }

    fn lock(&self) -> MutexGuard<'_, Ledger> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn record_request(&self, turn: usize, prompt: Message, history: Vec<Arc<Message>>) {
        self.lock().requests.push(RequestSnapshot {
            turn,
            prompt,
            history,
        });
    }

    fn open_call(&self, call: String, name: String) {
        let mut ledger = self.lock();
        if ledger.calls.iter().any(|record| record.call == call) {
            return;
        }
        ledger.calls.push(RecordedCall {
            call,
            name,
            answered: false,
            is_error: None,
        });
    }

    fn close_call(&self, call: &str, is_error: bool) {
        let mut ledger = self.lock();
        if let Some(record) = ledger.calls.iter_mut().find(|record| record.call == call) {
            record.answered = true;
            record.is_error = Some(is_error);
        }
    }

    fn settle(&self, messages: Option<Vec<Arc<Message>>>) {
        self.lock().settled = messages;
        self.inner.phase.store(SETTLED, Ordering::SeqCst);
    }
}

/// The rig hook that feeds a [`FlowControl`].
pub struct LedgerHook {
    control: FlowControl,
}

impl LedgerHook {
    pub fn new(control: FlowControl) -> Self {
        Self { control }
    }
}

impl AgentHook for LedgerHook {
    fn on_run_start(
        &self,
        _ctx: &HookContext,
        _event: RunStart<'_>,
    ) -> impl std::future::Future<Output = RunStartAction> + Send {
        let control = self.control.clone();
        async move {
            if control.is_cancelled() {
                RunStartAction::Stop(CANCEL_REASON.to_string())
            } else {
                RunStartAction::Continue
            }
        }
    }

    fn on_completion_call(
        &self,
        _ctx: &HookContext,
        event: CompletionCallEvent<'_>,
    ) -> impl std::future::Future<Output = CompletionCallAction> + Send {
        let control = self.control.clone();
        let turn = event.turn;
        let prompt = event.prompt.clone();
        let history = event.history.to_vec();
        async move {
            control.record_request(turn, prompt, history);
            if control.is_cancelled() {
                CompletionCallAction::Stop(CANCEL_REASON.to_string())
            } else {
                CompletionCallAction::Continue
            }
        }
    }

    fn on_model_turn_finished(
        &self,
        _ctx: &HookContext,
        _event: ModelTurnFinished<'_>,
    ) -> impl std::future::Future<Output = ModelTurnAction> + Send {
        let control = self.control.clone();
        async move {
            if control.is_cancelled() {
                ModelTurnAction::Stop(CANCEL_REASON.to_string())
            } else {
                ModelTurnAction::Continue
            }
        }
    }

    fn on_text_delta(
        &self,
        _ctx: &HookContext,
        _event: TextDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + Send {
        let control = self.control.clone();
        async move {
            if control.is_cancelled() {
                ObservationAction::Stop(CANCEL_REASON.to_string())
            } else {
                ObservationAction::Continue
            }
        }
    }

    fn on_reasoning_delta(
        &self,
        _ctx: &HookContext,
        _event: ReasoningDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + Send {
        let control = self.control.clone();
        async move {
            if control.is_cancelled() {
                ObservationAction::Stop(CANCEL_REASON.to_string())
            } else {
                ObservationAction::Continue
            }
        }
    }

    fn on_tool_call_delta(
        &self,
        _ctx: &HookContext,
        _event: ToolCallDelta<'_>,
    ) -> impl std::future::Future<Output = ObservationAction> + Send {
        let control = self.control.clone();
        async move {
            if control.is_cancelled() {
                ObservationAction::Stop(CANCEL_REASON.to_string())
            } else {
                ObservationAction::Continue
            }
        }
    }

    fn on_dispatch(
        &self,
        _ctx: &HookContext,
        event: DispatchEvent<'_>,
    ) -> impl std::future::Future<Output = DispatchAction> + Send {
        let control = self.control.clone();
        let call = event.call_id.map(ToString::to_string);
        let name = event.tool_name().map(str::to_string);
        async move {
            if let (Some(call), Some(name)) = (call, name) {
                control.open_call(call, name);
            }
            if control.is_cancelled() {
                DispatchAction::Deny(ErrorReport::new(ErrorKind::Cancelled, CANCEL_REASON))
            } else {
                DispatchAction::Proceed
            }
        }
    }

    fn on_outcome(
        &self,
        _ctx: &HookContext,
        event: OutcomeEvent<'_>,
    ) -> impl std::future::Future<Output = OutcomeAction> + Send {
        let control = self.control.clone();
        let call = event.call_id.map(ToString::to_string);
        let is_error = event.tool_result().map(|result| result.is_error());
        async move {
            if let (Some(call), Some(is_error)) = (call, is_error) {
                control.close_call(&call, is_error);
            }
            OutcomeAction::Proceed
        }
    }

    fn on_run_settled(
        &self,
        _ctx: &HookContext,
        event: RunSettled<'_>,
    ) -> impl std::future::Future<Output = ()> + Send {
        let control = self.control.clone();
        let messages = event.messages.map(|messages| messages.to_vec());
        async move {
            control.settle(messages);
        }
    }

    /// Every kind matters to the ledger except the families this crate never
    /// dispatches (embeddings, reranks, retrieval, custom effects).
    fn observes(&self, kind: StepEventKind) -> bool {
        !matches!(
            kind,
            StepEventKind::EmbedDispatch
                | StepEventKind::RerankDispatch
                | StepEventKind::RetrieveDispatch
                | StepEventKind::CustomDispatch
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cancel_is_sticky_and_visible_to_every_clone() {
        let control = FlowControl::new();
        let clone = control.clone();
        assert!(!control.is_cancelled());
        assert_eq!(control.phase(), FlowPhase::Running);
        clone.cancel();
        assert!(control.is_cancelled());
        assert!(clone.is_cancelled());
    }

    #[test]
    fn finishing_wins_over_running_and_settle_wins_over_both() {
        let control = FlowControl::new();
        control.mark_finishing();
        assert_eq!(control.phase(), FlowPhase::Finishing);
        control.mark_finishing();
        assert_eq!(control.phase(), FlowPhase::Finishing);
        control.settle(Some(Vec::new()));
        assert_eq!(control.phase(), FlowPhase::Settled);
        assert!(control.is_settled());
        control.mark_finishing();
        assert_eq!(control.phase(), FlowPhase::Settled);
    }

    #[test]
    fn snapshot_sees_requests_calls_and_settled_messages() {
        let control = FlowControl::new();
        control.record_request(1, Message::user("hello"), Vec::new());
        control.open_call("c1".into(), "read_file".into());
        control.close_call("c1", true);
        control.settle(Some(vec![Arc::new(Message::user("hello"))]));
        let ledger = control.snapshot();
        assert_eq!(ledger.requests.len(), 1);
        assert_eq!(ledger.requests[0].turn, 1);
        assert_eq!(ledger.calls.len(), 1);
        assert!(ledger.calls[0].answered);
        assert_eq!(ledger.calls[0].is_error, Some(true));
        assert_eq!(ledger.settled.expect("settled messages").len(), 1);
    }

    #[test]
    fn duplicate_open_calls_are_recorded_once() {
        let control = FlowControl::new();
        control.open_call("c1".into(), "read_file".into());
        control.open_call("c1".into(), "read_file".into());
        assert_eq!(control.snapshot().calls.len(), 1);
    }

    #[test]
    fn a_close_for_an_unknown_call_is_a_no_op() {
        let control = FlowControl::new();
        control.close_call("missing", true);
        assert!(control.snapshot().calls.is_empty());
    }

    #[test]
    fn ledger_serializes_a_tool_call_name() {
        // A guard against dropping the name/args when the hook records a call:
        // the name is what a resume needs to pair the call with a result.
        let call = RecordedCall {
            call: "c1".into(),
            name: "read_file".into(),
            answered: false,
            is_error: None,
        };
        assert_eq!(
            serde_json::to_value(&call).expect("serializable"),
            json!({
                "call": "c1",
                "name": "read_file",
                "answered": false,
                "is_error": null,
            })
        );
    }

    #[tokio::test]
    async fn a_real_run_records_its_request_and_settled_messages() {
        // The hook is only useful if rig actually calls it: drive a mock run
        // end to end and read the ledger back.
        use futures_util::StreamExt as _;
        use rig_agent::agent::AgentBuilder;
        use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

        let model = MockCompletionModel::from_stream_turns([vec![
            MockStreamEvent::text("hello"),
            MockStreamEvent::final_response_with_default_usage(),
        ]]);
        let control = FlowControl::new();
        let agent = AgentBuilder::new(model)
            .add_hook(LedgerHook::new(control.clone()))
            .build();

        let mut stream = agent.prompt(Message::user("go")).stream();
        let mut items = 0;
        while let Some(item) = stream.next().await {
            items += 1;
            assert!(item.is_ok(), "the run should succeed: {item:?}");
        }
        assert!(items > 0, "the run produced stream items");

        let ledger = control.snapshot();
        assert_eq!(ledger.requests.len(), 1, "one completion call");
        assert_eq!(ledger.requests[0].turn, 1);
        assert!(
            ledger.settled.is_some(),
            "the run's committed messages are recorded"
        );
        assert_eq!(control.phase(), FlowPhase::Settled);
    }
}
