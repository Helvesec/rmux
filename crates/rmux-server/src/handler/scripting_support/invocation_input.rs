//! One finished input shared by the commands of a source invocation.
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

#[derive(Debug, Clone)]
pub(super) struct FinishedInvocationInput(Arc<Mutex<Option<Vec<u8>>>>);

impl FinishedInvocationInput {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(Some(bytes))))
    }

    // Keep the reservation through creation: failed target or write validation
    // must leave the input available, and concurrent descendants cannot copy it.
    pub(super) async fn reserve(&self) -> MutexGuard<'_, Option<Vec<u8>>> {
        self.0.lock().await
    }
}

impl PartialEq for FinishedInvocationInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for FinishedInvocationInput {}
