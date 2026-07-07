//! Wait helpers for daemon-backed pane handles.

use crate::{ArmedWait, PaneExitState, Result, VisibleTextExpectation};

use super::Pane;

impl Pane {
    /// Waits until the pane emits the requested raw byte sequence.
    ///
    /// Dropping the returned future before it completes sends a best-effort
    /// daemon cancellation request. Drop cleanup only removes the wait record;
    /// it never closes panes, sessions, processes, or the daemon.
    pub async fn wait_for(&self, bytes: impl AsRef<[u8]>) -> Result<()> {
        crate::wait::wait_for_bytes(self, bytes.as_ref().to_vec()).await
    }

    /// Arms a daemon-backed wait for future raw pane output bytes.
    ///
    /// The returned [`ArmedWait`] is created only after the SDK has sent the
    /// daemon wait request with a live-tail cursor, so it cannot match retained
    /// history from before this call. Await the handle after triggering the
    /// output that should satisfy the wait.
    pub async fn wait_for_next(&self, bytes: impl AsRef<[u8]>) -> Result<ArmedWait> {
        crate::wait::wait_for_next_bytes(self, bytes.as_ref().to_vec()).await
    }

    /// Waits until the pane's rendered snapshot text contains non-empty `text`.
    ///
    /// This is a client-side text wait over fresh [`Self::snapshot`]
    /// captures. It observes the rendered grid text already present at the
    /// time of the first snapshot and keeps polling until the configured SDK
    /// operation timeout expires. Unlike [`Self::wait_for`], this method does
    /// not subscribe to raw pane output and does not send SDK byte-wait
    /// cancellation requests.
    pub async fn wait_for_text(&self, text: impl AsRef<str>) -> Result<()> {
        crate::wait::wait_for_text(self, text.as_ref().to_owned()).await
    }

    /// Arms a daemon-backed wait for future pane output containing `text`.
    ///
    /// This matches the UTF-8 bytes of `text` in raw output emitted after the
    /// wait is armed. It does not inspect existing snapshots or retained output
    /// history.
    pub async fn wait_for_text_next(&self, text: impl AsRef<str>) -> Result<ArmedWait> {
        crate::wait::wait_for_text_next(self, text.as_ref().to_owned()).await
    }

    /// Starts a visible-screen text expectation builder.
    ///
    /// Unlike raw output waits, visible waits poll rendered
    /// [`crate::PaneSnapshot`] text. They observe the current screen after
    /// terminal control sequences, clears, wrapping, and redraws have been applied.
    pub fn expect_visible_text(&self) -> VisibleTextExpectation<'_> {
        VisibleTextExpectation::new(self)
    }

    /// Waits until the pane process exits or the pane slot becomes stale.
    ///
    /// The wait polls daemon sticky pane metadata through [`Self::info`].
    /// It does not subscribe to raw output and does not send SDK byte-wait
    /// cancellation requests. `Ok(None)` means the pane was already stale, or
    /// vanished before the daemon could retain exit details for this slot.
    pub async fn wait_exit(&self) -> Result<Option<PaneExitState>> {
        crate::wait::wait_exit(self).await
    }

    /// Alias for [`Self::wait_exit`].
    pub async fn wait_for_exit(&self) -> Result<Option<PaneExitState>> {
        self.wait_exit().await
    }
}
