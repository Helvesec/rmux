//! Request-local disposition of the outer file-backed source invocation.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Debug, Clone)]
pub(super) struct SourceInputQueueStatus {
    source_depth: usize,
    selection_failed: Arc<AtomicBool>,
}

impl SourceInputQueueStatus {
    pub(super) fn new() -> Self {
        Self {
            source_depth: 0,
            selection_failed: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn at_depth(mut self, source_depth: usize) -> Self {
        self.source_depth = source_depth;
        self
    }

    pub(super) fn record_selection_failure(&self, source_depth: usize) {
        // A nested source failure belongs to the contents of a successfully
        // selected outer file and must not abort the caller's CLI queue.
        if source_depth == self.source_depth {
            self.selection_failed.store(true, Ordering::Relaxed);
        }
    }

    pub(super) fn stop_queue(&self) -> bool {
        self.selection_failed.load(Ordering::Relaxed)
    }
}

impl PartialEq for SourceInputQueueStatus {
    fn eq(&self, other: &Self) -> bool {
        self.source_depth == other.source_depth
            && Arc::ptr_eq(&self.selection_failed, &other.selection_failed)
    }
}
impl Eq for SourceInputQueueStatus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracks_only_the_invoked_source_depth_and_is_request_local() {
        let result = SourceInputQueueStatus::new();
        let context = result.clone().at_depth(7);
        context.record_selection_failure(8);
        assert!(!result.stop_queue());
        context.record_selection_failure(7);
        assert!(result.stop_queue());
        assert!(!SourceInputQueueStatus::new().stop_queue());
    }
}
