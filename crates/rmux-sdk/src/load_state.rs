//! Terminal load-state waits.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

use crate::{Pane, PaneSnapshot, Result, RmuxError, WaitTimeoutError};

const DEFAULT_QUIET_FOR: Duration = Duration::from_millis(300);

/// Terminal load states supported by the SDK.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TerminalLoadState {
    /// No visible snapshot change has been observed for the configured quiet window.
    Quiet,
}

/// Awaitable terminal load-state wait.
#[derive(Debug)]
#[must_use = "terminal load-state waits do nothing unless awaited"]
pub struct TerminalLoadStateWait {
    pane: Pane,
    state: TerminalLoadState,
    quiet_for: Duration,
    timeout: Option<Duration>,
    poll_interval: Duration,
}

impl TerminalLoadStateWait {
    pub(crate) fn new(pane: Pane, state: TerminalLoadState) -> Self {
        Self {
            pane,
            state,
            quiet_for: DEFAULT_QUIET_FOR,
            timeout: None,
            poll_interval: crate::wait::TEXT_POLL_INTERVAL,
        }
    }

    pub(crate) fn quiet_for(pane: Pane, quiet_for: Duration) -> Self {
        Self {
            quiet_for,
            ..Self::new(pane, TerminalLoadState::Quiet)
        }
    }

    /// Overrides the overall timeout for this wait.
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Overrides the snapshot polling interval for this wait.
    pub const fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Overrides the quiet window used by [`TerminalLoadState::Quiet`].
    pub const fn stable_for(mut self, quiet_for: Duration) -> Self {
        self.quiet_for = quiet_for;
        self
    }

    async fn run(self) -> Result<PaneSnapshot> {
        match self.state {
            TerminalLoadState::Quiet => wait_until_quiet(self).await,
        }
    }
}

impl IntoFuture for TerminalLoadStateWait {
    type Output = Result<PaneSnapshot>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

impl Pane {
    /// Waits for a terminal load state.
    ///
    /// `Quiet` means the rendered snapshot stays unchanged for the wait's
    /// configured quiet window; no prompt-specific heuristics are inferred.
    pub fn wait_for_load_state(&self, state: TerminalLoadState) -> TerminalLoadStateWait {
        TerminalLoadStateWait::new(self.clone(), state)
    }

    /// Waits until the rendered terminal snapshot is stable for `duration`.
    pub fn wait_until_stable_for(&self, duration: Duration) -> TerminalLoadStateWait {
        TerminalLoadStateWait::quiet_for(self.clone(), duration)
    }

    /// Waits for the default terminal quiet window.
    pub fn expect_stable(&self) -> TerminalLoadStateWait {
        self.wait_for_load_state(TerminalLoadState::Quiet)
    }
}

async fn wait_until_quiet(wait: TerminalLoadStateWait) -> Result<PaneSnapshot> {
    let timeout = wait
        .timeout
        .or_else(|| crate::wait::resolved_wait_timeout(wait.pane.configured_default_timeout()));
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let mut last = crate::wait::snapshot_with_wait_deadline(
        &wait.pane,
        "wait for terminal load-state snapshot",
        timeout,
        deadline,
        None,
        || {
            format!(
                "terminal load state {:?} for {:?}",
                wait.state, wait.quiet_for
            )
        },
    )
    .await?;
    let mut stable_since = Instant::now();

    loop {
        if stable_since.elapsed() >= wait.quiet_for {
            return Ok(last);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                format!(
                    "terminal load state {:?} for {:?}",
                    wait.state, wait.quiet_for
                ),
                timeout.expect("deadline implies timeout"),
                last,
            )));
        }
        sleep_until_next_poll(deadline, wait.poll_interval).await;
        let snapshot = crate::wait::snapshot_with_wait_deadline(
            &wait.pane,
            "wait for terminal load-state snapshot",
            timeout,
            deadline,
            Some(&last),
            || {
                format!(
                    "terminal load state {:?} for {:?}",
                    wait.state, wait.quiet_for
                )
            },
        )
        .await?;
        if snapshot.revision == last.revision && snapshot.visible_text() == last.visible_text() {
            continue;
        }
        last = snapshot;
        stable_since = Instant::now();
    }
}

async fn sleep_until_next_poll(deadline: Option<Instant>, poll_interval: Duration) {
    let Some(deadline) = deadline else {
        tokio::time::sleep(poll_interval).await;
        return;
    };
    let now = Instant::now();
    if now < deadline {
        tokio::time::sleep(poll_interval.min(deadline - now)).await;
    }
}
