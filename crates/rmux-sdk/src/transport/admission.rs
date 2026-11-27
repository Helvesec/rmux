use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const QUEUED: u8 = 0;
const ADMITTED: u8 = 1;
const WITHDRAWN: u8 = 2;

/// Decides, exactly once, whether the actor or the caller owns one command.
///
/// `mpsc::Sender::send` returns as soon as the command sits in the bounded
/// command queue's buffer. Such a command has neither a FIFO response slot
/// nor bytes on the wire: it belongs to the actor only once the actor admits
/// it. Until then the caller may withdraw it, and the actor discards a
/// withdrawn command unwritten, so the withdrawal cannot misalign the FIFO.
#[derive(Clone, Debug, Default)]
pub(super) struct Admission(Arc<AtomicU8>);

impl Admission {
    /// Actor side: takes the command into the FIFO unless it was withdrawn.
    pub(super) fn admit(&self) -> bool {
        self.settle(ADMITTED)
    }

    /// Caller side: withdraws the command unless the actor already admitted it.
    pub(super) fn withdraw(&self) -> bool {
        self.settle(WITHDRAWN)
    }

    fn settle(&self, outcome: u8) -> bool {
        self.0
            .compare_exchange(QUEUED, outcome, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::Admission;

    #[test]
    fn admission_settles_once_for_whichever_side_comes_first() {
        let admitted = Admission::default();
        assert!(admitted.admit());
        assert!(
            !admitted.withdraw(),
            "an admitted command cannot be withdrawn"
        );

        let withdrawn = Admission::default();
        assert!(withdrawn.withdraw());
        assert!(!withdrawn.admit(), "a withdrawn command cannot be admitted");
    }
}
