//! Test-only observation of the real Windows worker and its demand wait.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Default)]
pub(crate) struct WindowsReaderProbe {
    running: AtomicUsize,
    parked: AtomicUsize,
}

impl WindowsReaderProbe {
    pub(crate) fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    pub(crate) fn parked(&self) -> usize {
        self.parked.load(Ordering::SeqCst)
    }

    pub(crate) fn enter(self: &Arc<Self>, parked: bool) -> ReaderScope {
        let counter = if parked { &self.parked } else { &self.running };
        counter.fetch_add(1, Ordering::SeqCst);
        ReaderScope(Arc::clone(self), parked)
    }
}

pub(crate) struct ReaderScope(Arc<WindowsReaderProbe>, bool);

impl Drop for ReaderScope {
    fn drop(&mut self) {
        let counter = if self.1 {
            &self.0.parked
        } else {
            &self.0.running
        };
        counter.fetch_sub(1, Ordering::SeqCst);
    }
}
