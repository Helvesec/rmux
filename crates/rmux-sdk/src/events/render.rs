//! Minimal snapshot render stream built from raw pane output.

use std::time::Duration;

use crate::{Pane, PaneLagNotice, PaneOutputChunk, PaneOutputStream, PaneSnapshot, Result};

const DEFAULT_RENDER_DEBOUNCE: Duration = Duration::from_millis(16);

/// Snapshot update emitted by [`PaneRenderStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderUpdate {
    snapshot: PaneSnapshot,
    lag: Option<PaneLagNotice>,
}

impl RenderUpdate {
    /// Returns the snapshot captured for this render update.
    #[must_use]
    pub const fn snapshot(&self) -> &PaneSnapshot {
        &self.snapshot
    }

    /// Returns the lag notice that preceded this snapshot, when output lag was
    /// observed.
    #[must_use]
    pub const fn lag(&self) -> Option<&PaneLagNotice> {
        self.lag.as_ref()
    }

    /// Consumes the update and returns its snapshot.
    #[must_use]
    pub fn into_snapshot(self) -> PaneSnapshot {
        self.snapshot
    }
}

/// Minimal event-driven render stream for one pane.
///
/// This render stream is intentionally built from [`Pane::output_stream`]:
/// output wakes the stream, a short debounce coalesces bursts, then the SDK
/// captures a fresh snapshot and emits it only when the snapshot revision
/// changed. It avoids blind fixed-rate refresh loops without claiming a
/// daemon-native revision stream.
pub struct PaneRenderStream {
    pane: Pane,
    output: PaneOutputStream,
    debounce: Duration,
    last_revision: Option<u64>,
    pending_lag: Option<PaneLagNotice>,
}

impl PaneRenderStream {
    pub(crate) async fn open(pane: Pane) -> Result<Self> {
        let output = pane.output_stream().await?;
        let baseline = pane.snapshot().await?;
        Ok(Self {
            pane,
            output,
            debounce: DEFAULT_RENDER_DEBOUNCE,
            last_revision: Some(baseline.revision),
            pending_lag: None,
        })
    }

    /// Overrides the debounce used before capturing snapshots after output.
    #[must_use]
    pub const fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Returns the next render update, or `None` once the underlying output
    /// subscription closes.
    pub async fn next(&mut self) -> Result<Option<RenderUpdate>> {
        loop {
            let Some(chunk) = self.output.next().await? else {
                return Ok(None);
            };
            if let PaneOutputChunk::Lag(lag) = chunk {
                self.pending_lag = Some(lag);
            }

            if !self.debounce.is_zero() {
                tokio::time::sleep(self.debounce).await;
            }

            let snapshot = self.pane.snapshot().await?;
            if self.last_revision == Some(snapshot.revision) {
                continue;
            }
            self.last_revision = Some(snapshot.revision);
            return Ok(Some(RenderUpdate {
                snapshot,
                lag: self.pending_lag.take(),
            }));
        }
    }
}

impl std::fmt::Debug for PaneRenderStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneRenderStream")
            .field("pane", &self.pane)
            .field("debounce", &self.debounce)
            .field("last_revision", &self.last_revision)
            .field("pending_lag", &self.pending_lag)
            .finish_non_exhaustive()
    }
}
