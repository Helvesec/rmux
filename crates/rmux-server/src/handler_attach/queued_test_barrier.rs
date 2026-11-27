//! Pauses a test's client at one causal point of the queue, once, so a test can make a binding
//! end, or a surface open, exactly there.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in crate::handler) enum Point {
    /// A frame was found ordinary and is about to be queued if its input still has to wait.
    QueueDecision,
    /// A replay has replayed a frame and is about to take its next step.
    ReplayStep,
    /// The input path handled a part of a live frame and is about to decide on the next one.
    NextPart,
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct Barrier {
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

impl Barrier {
    pub(in crate::handler) async fn reached(&self) {
        self.reached.notified().await;
    }

    pub(in crate::handler) fn resume(&self) {
        self.resume.notify_one();
    }
}

static ARMED: LazyLock<Mutex<HashMap<(Point, u32), Barrier>>> = LazyLock::new(Default::default);

/// Arms `point` for the client `attach_pid`; the next pass there waits for `resume`.
pub(in crate::handler) fn arm(point: Point, attach_pid: u32) -> Barrier {
    let barrier = Barrier {
        reached: Arc::new(Notify::new()),
        resume: Arc::new(Notify::new()),
    };
    ARMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert((point, attach_pid), barrier.clone());
    barrier
}

pub(in crate::handler) async fn pass(point: Point, attach_pid: u32) {
    let barrier = ARMED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(&(point, attach_pid));
    if let Some(barrier) = barrier {
        barrier.reached.notify_one();
        barrier.resume.notified().await;
    }
}
