use std::time::Duration;

use tokio::time::Instant;

/// One absolute deadline shared by every RPC in a public SDK operation.
///
/// A scoped deadline is present even when it is unbounded. This distinction
/// lets an explicit `Duration::MAX` override a finite reusable-handle default.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OperationDeadline {
    requested: Option<Duration>,
    expires_at: Option<Instant>,
}

impl OperationDeadline {
    pub(crate) fn from_timeout(timeout: Option<Duration>) -> Self {
        let (requested, expires_at) = match timeout {
            Some(Duration::MAX) | None => (None, None),
            Some(timeout) => match Instant::now().checked_add(timeout) {
                Some(expires_at) => (Some(timeout), Some(expires_at)),
                None => (None, None),
            },
        };
        Self {
            requested,
            expires_at,
        }
    }

    pub(crate) const fn requested_timeout(self) -> Option<Duration> {
        self.requested
    }

    pub(crate) fn remaining_timeout(self) -> Option<Duration> {
        self.requested.map(|_| {
            self.expires_at
                .map(|expires_at| expires_at.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::MAX)
        })
    }
}
