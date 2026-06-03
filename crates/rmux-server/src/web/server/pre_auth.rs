use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Clone)]
pub(super) struct PreAuthQueue {
    inner: Arc<Mutex<PreAuthQueueState>>,
    capacity: usize,
}

#[derive(Default)]
struct PreAuthQueueState {
    next_id: u64,
    entries: VecDeque<PreAuthEntry>,
}

struct PreAuthEntry {
    id: u64,
}

pub(super) struct PreAuthGuard {
    queue: PreAuthQueue,
    id: u64,
}

impl PreAuthQueue {
    pub(super) fn new(capacity: usize) -> Self {
        debug_assert!(capacity > 0, "pre-auth queue capacity must be non-zero");
        Self {
            inner: Arc::new(Mutex::new(PreAuthQueueState::default())),
            capacity,
        }
    }

    pub(super) fn try_register(&self) -> Option<PreAuthGuard> {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        if state.entries.len() >= self.capacity {
            return None;
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.entries.push_back(PreAuthEntry { id });
        Some(PreAuthGuard {
            queue: self.clone(),
            id,
        })
    }

    fn remove(&self, id: u64) {
        let mut state = self.inner.lock().expect("pre-auth queue lock poisoned");
        if let Some(index) = state.entries.iter().position(|entry| entry.id == id) {
            state.entries.remove(index);
        }
    }

    #[cfg(test)]
    pub(super) fn pending_count(&self) -> usize {
        self.inner
            .lock()
            .expect("pre-auth queue lock poisoned")
            .entries
            .len()
    }
}

impl Drop for PreAuthGuard {
    fn drop(&mut self) {
        self.queue.remove(self.id);
    }
}
