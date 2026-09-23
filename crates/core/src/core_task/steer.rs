use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

/// Shared steering-preemption state between the run loop and the active
/// stream task. The run loop sets `ARMED` when a prompt is steered while this
/// stream is busy; the stream task flips it to `FINALIZING` when it cuts the
/// stream at an action boundary, so a concurrent `CancelStream` will not
/// abort it mid-persist. SeqCst ordering makes the two-sided race (cancel vs.
/// cut) linearizable: whichever side observes the other's state acts on it.
#[derive(Debug, Clone, Default)]
pub(crate) struct SteerSignal(Arc<AtomicU8>);

const STEER_IDLE: u8 = 0;
const STEER_ARMED: u8 = 1;
const STEER_FINALIZING: u8 = 2;

impl SteerSignal {
    /// Arm preemption. A no-op while the stream task is already finalizing a
    /// cut, so a steer racing the cut cannot un-guard `CancelStream`.
    pub fn arm(&self) {
        if self.0.load(Ordering::SeqCst) != STEER_FINALIZING {
            self.0.store(STEER_ARMED, Ordering::SeqCst);
        }
    }

    /// Disarm preemption. A no-op while the stream task is finalizing (its
    /// outcome processing resets the signal afterwards).
    pub fn disarm(&self) {
        if self.0.load(Ordering::SeqCst) != STEER_FINALIZING {
            self.0.store(STEER_IDLE, Ordering::SeqCst);
        }
    }

    /// Unconditionally back to idle — only valid once the stream task has
    /// ended (outcome processing or a fresh turn replacing it).
    pub fn reset(&self) {
        self.0.store(STEER_IDLE, Ordering::SeqCst);
    }

    pub fn is_armed(&self) -> bool {
        self.0.load(Ordering::SeqCst) == STEER_ARMED
    }

    /// The stream task claims the cut. Succeeds exactly once, from `ARMED`.
    pub fn begin_preempt(&self) -> bool {
        self.0
            .compare_exchange(
                STEER_ARMED,
                STEER_FINALIZING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    pub fn is_finalizing(&self) -> bool {
        self.0.load(Ordering::SeqCst) == STEER_FINALIZING
    }
}
