use std::time::Duration;

use tokio::time::Instant;

pub(super) fn is_pending_escape(input: &[u8]) -> bool {
    matches!(input, b"\x1b" | b"\x1b[" | b"\x1bO")
}

#[derive(Debug, Default)]
pub(super) struct PendingEscapeFlush {
    deadline: Option<Instant>,
}

impl PendingEscapeFlush {
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn clear(&mut self) {
        self.deadline = None;
    }

    pub(super) fn sync(&mut self, pending_input: &[u8], escape_time: Duration) {
        if !is_pending_escape(pending_input) {
            self.clear();
            return;
        }

        if self.deadline.is_none() {
            self.deadline = Some(Instant::now() + escape_time);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::PendingEscapeFlush;

    #[test]
    fn pending_escape_arms_once_and_clears_on_other_input() {
        let mut flush = PendingEscapeFlush::default();

        flush.sync(b"\x1b", Duration::from_millis(500));
        let first = flush.deadline().expect("escape should arm a deadline");
        flush.sync(b"\x1b", Duration::from_millis(1));

        assert_eq!(flush.deadline(), Some(first));
        flush.sync(b"\x1b[", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1bO", Duration::from_millis(500));
        assert!(flush.deadline().is_some());
        flush.sync(b"\x1b[12", Duration::from_millis(500));
        assert!(flush.deadline().is_none());
    }
}
