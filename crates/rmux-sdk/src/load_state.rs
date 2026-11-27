//! Terminal load-state waits.

use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use tokio::time::Instant;

use crate::{Pane, PaneId, PaneSnapshot, Result, RmuxError, WaitTimeoutError};

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
    /// A pane whose process exits and leaves the inventory is final: the wait
    /// returns at once with the empty, revision-`0` snapshot of a pane that is
    /// no longer listed. A pane closed during the wait, which leaves no exit
    /// status, never settles and fails it with [`RmuxError::PaneNotFound`].
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
    let timeout = crate::wait::resolved_wait_timeout_override(
        wait.timeout,
        wait.pane.configured_default_timeout(),
    );
    let deadline = crate::wait::wait_deadline(timeout);
    let pane = wait.pane.begin_operation_handle_with_timeout(wait.timeout);
    let (pane, pane_id) = crate::wait::with_wait_deadline(
        LOAD_STATE_SNAPSHOT_OPERATION,
        timeout,
        deadline,
        pane.pin_to_current_identity(),
    )
    .await?;
    let observer = QuietObserver {
        wait: &wait,
        pane: &pane,
        pane_id,
        timeout,
        deadline,
    };
    let mut last = match observer.observe(None).await? {
        QuietObservation::Rendered(snapshot) => snapshot,
        QuietObservation::Exited => return Ok(PaneSnapshot::default()),
    };
    let mut stable_since = Instant::now();

    loop {
        if stable_since.elapsed() >= wait.quiet_for {
            return Ok(last);
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(RmuxError::wait_timeout(WaitTimeoutError::new(
                observer.description(),
                timeout.expect("deadline implies timeout"),
                last,
            )));
        }
        sleep_until_next_poll(deadline, wait.poll_interval).await;
        let snapshot = match observer.observe(Some(&last)).await? {
            QuietObservation::Rendered(snapshot) => snapshot,
            QuietObservation::Exited => return Ok(PaneSnapshot::default()),
        };
        if snapshot.revision == last.revision && snapshot.visible_text() == last.visible_text() {
            continue;
        }
        last = snapshot;
        stable_since = Instant::now();
    }
}

const LOAD_STATE_SNAPSHOT_OPERATION: &str = "wait for terminal load-state snapshot";

enum QuietObservation {
    Rendered(PaneSnapshot),
    /// The observed process exited and its pane left the inventory.
    Exited,
}

struct QuietObserver<'a> {
    wait: &'a TerminalLoadStateWait,
    pane: &'a Pane,
    /// Identity pinned when the wait began; `None` for a slot already vacant.
    pane_id: Option<PaneId>,
    timeout: Option<Duration>,
    deadline: Option<Instant>,
}

impl QuietObserver<'_> {
    fn description(&self) -> String {
        format!(
            "terminal load state {:?} for {:?}",
            self.wait.state, self.wait.quiet_for
        )
    }

    /// Captures the pane once. A pane that vanished after the wait pinned it
    /// either exited, which ends the wait, or was closed without an exit
    /// status, which fails it: a closed pane never settles.
    async fn observe(&self, last: Option<&PaneSnapshot>) -> Result<QuietObservation> {
        let listed = crate::wait::bounded_wait_snapshot(
            LOAD_STATE_SNAPSHOT_OPERATION,
            self.timeout,
            self.deadline,
            last,
            || self.description(),
            self.pane.listed_snapshot(),
        )
        .await?;
        if let Some(snapshot) = listed {
            return Ok(QuietObservation::Rendered(snapshot));
        }
        let Some(pane_id) = self.pane_id else {
            // A slot that was already vacant keeps the documented empty snapshot.
            return Ok(QuietObservation::Rendered(PaneSnapshot::default()));
        };
        let exited = crate::wait::bounded_wait_snapshot(
            LOAD_STATE_SNAPSHOT_OPERATION,
            self.timeout,
            self.deadline,
            last,
            || self.description(),
            self.pane.vanished_pane_exited(pane_id),
        )
        .await?;
        if exited {
            return Ok(QuietObservation::Exited);
        }
        Err(RmuxError::pane_not_found(
            self.pane.target().session_name.clone(),
            pane_id,
        ))
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
