//! The ordered, bounded sequence of accepted `ESC k` window-rename requests.
//!
//! A rename cannot be applied where it is accepted: acceptance happens inside
//! the pane's own output publication, under the output-sender and transcript
//! locks, while applying one needs the handler state, its mutation primitives
//! and its hooks. The requests therefore cross that boundary through this
//! single sequence, which owns three properties the per-pane alert coalescer
//! cannot provide.
//!
//! **Order.** Every accepted request from every pane enters one FIFO under one
//! mutex, at the point where the pipeline already serialises publications, so
//! three requests observed as `A1, B1, A2` are applied as `A1, B1, A2` even
//! though `A` and `B` are different panes of the same window. Grouping them by
//! pane could only ever produce `A1, A2, B1` or `B1, A1, A2`. No physical
//! chronology between distinct PTYs is claimed: what is preserved is the order
//! this pipeline itself observed.
//!
//! **Bound and backpressure.** A producer is admitted only while the sequence
//! is under both limits, and it takes that admission *before* it publishes and
//! before it holds any output or transcript lock — a producer that has to wait
//! therefore waits on its own reader, never with a lock held and never after
//! it has already produced effects it cannot store. Nothing is dropped. The
//! sequence can exceed its limit only by the requests of feeds already
//! admitted, each of which is itself bounded by the reader's own read limits
//! and the parser's `input-buffer-size`; that overshoot is the design's stated
//! bound, not a claim of an exact cap.
//!
//! **Owner.** Each request carries the `WindowId` its pane belonged to when the
//! request was accepted, so a request that was accepted in one window can never
//! rename the window a later transfer moved that pane into.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use rmux_core::{PaneId, WindowId};
use rmux_proto::SessionName;

/// Requests accepted by one feed, before the pane runtime identity is attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AcceptedWindowRename {
    /// The window that owned the pane when its output was parsed.
    pub(crate) owner: WindowId,
    /// The requested name, exactly as the pane emitted it and before cleaning.
    pub(crate) name: String,
}

/// One accepted request waiting to be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QueuedWindowRename {
    pub(crate) session_name: SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: Option<u64>,
    pub(crate) owner: WindowId,
    pub(crate) name: String,
}

impl QueuedWindowRename {
    fn charged_bytes(&self) -> usize {
        self.name.len()
    }
}

/// How much may wait between acceptance and application.
///
/// Both are needed: a flood of empty requests is bounded by the count alone,
/// and a few very long names by the bytes alone. The byte charge is the name a
/// feed actually accepted, so a name begun in an earlier feed and finished in
/// this one is charged in full, once.
const MAX_PENDING_REQUESTS: usize = 256;
const MAX_PENDING_BYTES: usize = 256 * 1024;

#[derive(Debug, Default)]
struct QueueState {
    pending: VecDeque<QueuedWindowRename>,
    pending_bytes: usize,
    /// Feeds admitted and not yet finished. Counted against the request limit
    /// so an unbounded number of producers cannot pass admission at once.
    admitted: usize,
}

#[derive(Debug)]
pub(crate) struct PaneRenameQueue {
    state: Mutex<QueueState>,
    space: Condvar,
    space_async: tokio::sync::Notify,
    max_requests: usize,
    max_bytes: usize,
}

/// A producer's right to run one feed that may accept requests.
///
/// Dropping it returns the admission, so a feed that accepts nothing, fails,
/// or panics cannot leak capacity.
#[derive(Debug)]
pub(crate) struct PaneRenameAdmission<'a> {
    queue: &'a PaneRenameQueue,
}

impl Drop for PaneRenameAdmission<'_> {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.admitted = state.admitted.saturating_sub(1);
        drop(state);
        self.queue.wake();
    }
}

/// What the sequence currently holds.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneRenameQueueStats {
    pub(crate) pending: usize,
    pub(crate) pending_bytes: usize,
    pub(crate) admitted: usize,
}

impl Default for PaneRenameQueue {
    fn default() -> Self {
        Self::with_capacity(MAX_PENDING_REQUESTS, MAX_PENDING_BYTES)
    }
}

impl PaneRenameQueue {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_capacity(max_requests: usize, max_bytes: usize) -> Self {
        Self {
            state: Mutex::new(QueueState::default()),
            space: Condvar::new(),
            space_async: tokio::sync::Notify::new(),
            max_requests: max_requests.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, QueueState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn wake(&self) {
        self.space.notify_all();
        self.space_async.notify_waiters();
    }

    fn has_room(&self, state: &QueueState) -> bool {
        state.pending.len() + state.admitted < self.max_requests
            && state.pending_bytes < self.max_bytes
    }

    /// Takes an admission if the sequence has room, without waiting.
    #[cfg(any(unix, test))]
    pub(crate) fn try_admit(&self) -> Option<PaneRenameAdmission<'_>> {
        let mut state = self.lock();
        if !self.has_room(&state) {
            return None;
        }
        state.admitted += 1;
        Some(PaneRenameAdmission { queue: self })
    }

    /// Waits for room without blocking the runtime, for the async reader.
    ///
    /// The waiter registers its interest before it tests the sequence, so a
    /// release between the two cannot be missed.
    #[cfg(any(unix, test))]
    pub(crate) async fn admit(&self) -> PaneRenameAdmission<'_> {
        loop {
            let notified = self.space_async.notified();
            if let Some(admission) = self.try_admit() {
                return admission;
            }
            notified.await;
        }
    }

    /// Waits for room on a blocking reader thread, for the platforms whose
    /// pane reader is not a future.
    #[cfg_attr(unix, allow(dead_code))]
    pub(crate) fn admit_blocking(&self) -> PaneRenameAdmission<'_> {
        let mut state = self.lock();
        while !self.has_room(&state) {
            state = self
                .space
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.admitted += 1;
        PaneRenameAdmission { queue: self }
    }

    /// Appends one pane's accepted requests, in the order it accepted them.
    ///
    /// This never waits: the producer was admitted before it began, so the
    /// requests it did accept are always stored rather than dropped.
    pub(crate) fn push(
        &self,
        session_name: &SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
        accepted: Vec<AcceptedWindowRename>,
    ) {
        if accepted.is_empty() {
            return;
        }
        let mut state = self.lock();
        for AcceptedWindowRename { owner, name } in accepted {
            let request = QueuedWindowRename {
                session_name: session_name.clone(),
                pane_id,
                generation,
                owner,
                name,
            };
            state.pending_bytes = state.pending_bytes.saturating_add(request.charged_bytes());
            state.pending.push_back(request);
        }
    }

    /// Takes the whole sequence, in order, and returns its capacity.
    pub(crate) fn drain(&self) -> Vec<QueuedWindowRename> {
        let mut state = self.lock();
        let drained = state.pending.drain(..).collect::<Vec<_>>();
        state.pending_bytes = 0;
        drop(state);
        if drained.is_empty() {
            return drained;
        }
        self.wake();
        drained
    }

    /// Takes the prefix of the sequence up to and including the last request
    /// of one pane generation.
    ///
    /// A pane that reaches EOF must not have its own requests applied while
    /// requests another pane accepted earlier still wait: that would apply the
    /// sequence out of the order it was observed in. Taking the prefix through
    /// the exiting generation carries those earlier requests with it and
    /// leaves everything accepted afterwards for the ordinary flush.
    pub(crate) fn drain_through(
        &self,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Vec<QueuedWindowRename> {
        let mut state = self.lock();
        let Some(last) = state
            .pending
            .iter()
            .rposition(|request| request.pane_id == pane_id && request.generation == generation)
        else {
            return Vec::new();
        };
        let drained = state.pending.drain(..=last).collect::<Vec<_>>();
        let released = drained
            .iter()
            .map(QueuedWindowRename::charged_bytes)
            .sum::<usize>();
        state.pending_bytes = state.pending_bytes.saturating_sub(released);
        drop(state);
        self.wake();
        drained
    }

    /// Resolves once the sequence holds no request.
    ///
    /// Every path that empties the sequence wakes its waiters, and the waiter
    /// registers its interest before it tests the sequence, as `admit` does,
    /// so a drain between the two cannot be missed. A test waits on this
    /// instead of guessing how long the flush that takes the sequence may be
    /// delayed on a loaded host.
    #[cfg(test)]
    pub(crate) async fn wait_until_empty(&self) {
        loop {
            let notified = self.space_async.notified();
            if self.lock().pending.is_empty() {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> PaneRenameQueueStats {
        let state = self.lock();
        PaneRenameQueueStats {
            pending: state.pending.len(),
            pending_bytes: state.pending_bytes,
            admitted: state.admitted,
        }
    }
}

#[cfg(test)]
#[path = "pane_rename_queue_tests.rs"]
mod pane_rename_queue_tests;
