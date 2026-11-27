use super::{mpsc, Arc, AtomicBool, ControlClientIdentity, ControlCommandResponseEvent, Ordering};

#[derive(Debug, Clone)]
pub(crate) struct ControlCommandResponseSink {
    pub(super) identity: ControlClientIdentity,
    pub(super) sender: mpsc::Sender<ControlCommandResponseEvent>,
    pub(super) closing: Arc<AtomicBool>,
    pub(super) individual_guards: bool,
    pub(super) typed_status: bool,
}

impl ControlCommandResponseSink {
    pub(crate) fn new(
        identity: ControlClientIdentity,
        sender: mpsc::Sender<ControlCommandResponseEvent>,
        closing: Arc<AtomicBool>,
        individual_guards: bool,
        typed_status: bool,
    ) -> Self {
        Self {
            identity,
            sender,
            closing,
            individual_guards,
            typed_status,
        }
    }

    pub(super) fn try_send(&self, event: ControlCommandResponseEvent) -> bool {
        if self.closing.load(Ordering::SeqCst) {
            return false;
        }
        if self.sender.try_send(event).is_ok() {
            return true;
        }
        self.closing.store(true, Ordering::SeqCst);
        false
    }
}
