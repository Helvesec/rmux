//! One input shared by the commands of a source invocation.
use crate::listener::source_input::SourceStdin;
use std::sync::Arc;
use tokio::sync::{Mutex, MutexGuard};

#[derive(Debug)]
pub(super) enum InvocationInput {
    Finished(Vec<u8>),
    OnDemand(SourceStdin),
}

#[derive(Debug, Clone)]
pub(super) struct SharedInvocationInput(Arc<Mutex<Option<InvocationInput>>>);

impl SharedInvocationInput {
    pub(super) fn new(input: InvocationInput) -> Self {
        Self(Arc::new(Mutex::new(Some(input))))
    }

    // Keep the reservation through creation: failed target or write validation
    // must leave the input available, and concurrent descendants cannot copy it.
    pub(super) async fn reserve(&self) -> MutexGuard<'_, Option<InvocationInput>> {
        self.0.lock().await
    }
}

impl PartialEq for SharedInvocationInput {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SharedInvocationInput {}
