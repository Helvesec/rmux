use super::admission::Admission;
use super::failure::TransportFailure;
use super::TransportClient;

/// Aborts the ordered transport when an armed call is dropped while the
/// actor owns its exchange.
pub(super) struct OrderedResponseGuard {
    client: TransportClient,
    admission: Option<Admission>,
    armed: bool,
}

impl OrderedResponseGuard {
    /// Guards a call the actor owns as soon as its command is sent.
    pub(super) fn new(client: &TransportClient) -> Self {
        Self {
            client: client.clone(),
            admission: None,
            armed: false,
        }
    }

    /// Guards a call the actor owns only once it admits `admission`'s
    /// command; dropping the guard earlier withdraws that command instead.
    pub(super) fn for_admission(client: &TransportClient, admission: Admission) -> Self {
        Self {
            client: client.clone(),
            admission: Some(admission),
            armed: false,
        }
    }

    pub(super) fn arm(&mut self) {
        debug_assert!(!self.armed, "ordered response guard armed twice");
        self.armed = true;
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for OrderedResponseGuard {
    fn drop(&mut self) {
        if self.armed && !self.admission.as_ref().is_some_and(Admission::withdraw) {
            self.client
                .abort_with(TransportFailure::cancelled_ordered_request());
        }
    }
}
