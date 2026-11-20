use std::time::Duration;

use rmux_bootstrap::deadline::StartupDeadline;
use tokio::time::Instant;

/// One absolute deadline shared by every RPC in a public SDK operation.
/// A present, unbounded scope overrides the reusable handle's finite default.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OperationDeadline(StartupDeadline);

impl OperationDeadline {
    pub(crate) fn from_timeout(timeout: Option<Duration>) -> Self {
        let now = Instant::now().into_std();
        // Preserve the SDK's unbounded interpretation of overflowing finite values.
        let timeout = timeout.filter(|timeout| now.checked_add(*timeout).is_some());
        Self(StartupDeadline::from_timeout_at(timeout, now))
    }

    pub(crate) fn requested_timeout(self) -> Option<Duration> {
        self.0.requested_timeout()
    }

    pub(crate) fn remaining_timeout(self) -> Option<Duration> {
        self.0.remaining_timeout_at(Instant::now().into_std())
    }
}
