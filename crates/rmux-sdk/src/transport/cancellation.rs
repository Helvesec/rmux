use super::failure::TransportFailure;
use super::TransportClient;

pub(super) struct OrderedResponseGuard {
    client: TransportClient,
    armed: bool,
}

impl OrderedResponseGuard {
    pub(super) fn new(client: &TransportClient) -> Self {
        Self {
            client: client.clone(),
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
        if self.armed {
            self.client
                .abort_with(TransportFailure::cancelled_ordered_request());
        }
    }
}
