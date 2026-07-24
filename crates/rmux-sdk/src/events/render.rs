//! Snapshot render stream backed by the daemon's shared surface projection.

use std::time::Duration;

use crate::{
    Pane, PaneLagNotice, PaneOutputChunk, PaneOutputStream, PaneSnapshot, PaneStreamLifecycleEvent,
    PaneSurfaceEvent, PaneSurfaceFrame, PaneSurfaceStream, Result, RmuxError,
};
use tokio::time::Instant;

const DEFAULT_RENDER_DEBOUNCE: Duration = Duration::from_millis(16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputGenerationPhase {
    Live,
    AwaitingReset,
    Respawned { output_boundary: u64 },
}

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

/// Event-driven render stream for one pane.
///
/// Visual updates come from [`Pane::surface_stream`], whose renderer and
/// fingerprint are shared once per pane inside the daemon. The legacy output
/// subscription remains only to preserve this API's detailed lag notices; it
/// no longer triggers a fresh snapshot RPC for every output burst.
pub struct PaneRenderStream {
    pane: Pane,
    output: PaneOutputStream,
    surface: PaneSurfaceStream,
    debounce: Duration,
    last_snapshot: PaneSnapshot,
    pending_lag: Option<PaneLagNotice>,
    pending_snapshot: Option<PaneSnapshot>,
    pending_output_boundary: Option<u64>,
    render_deadline: Option<Instant>,
    output_closed: bool,
    output_generation: OutputGenerationPhase,
    surface_epoch: u64,
    surface_closed: bool,
}

impl PaneRenderStream {
    pub(crate) async fn open(pane: Pane) -> Result<Self> {
        let (operation_pane, _) = pane.begin_pinned_operation_handle().await?;
        let output = operation_pane.output_stream().await?;
        let mut surface = operation_pane.surface_stream().await?;
        let baseline = initial_surface_frame(&mut surface).await?;
        Ok(Self {
            pane: operation_pane.into_reusable_handle(),
            output,
            surface,
            debounce: DEFAULT_RENDER_DEBOUNCE,
            last_snapshot: baseline.snapshot.grid,
            pending_lag: None,
            pending_snapshot: None,
            pending_output_boundary: None,
            render_deadline: None,
            output_closed: false,
            output_generation: OutputGenerationPhase::Live,
            surface_epoch: baseline.epoch,
            surface_closed: false,
        })
    }

    /// Overrides the debounce used before capturing snapshots after output.
    #[must_use]
    pub const fn with_debounce(mut self, debounce: Duration) -> Self {
        self.debounce = debounce;
        self
    }

    /// Returns the next render update, or `None` once the underlying output
    /// surface reports a typed terminal end.
    pub async fn next(&mut self) -> Result<Option<RenderUpdate>> {
        loop {
            self.reopen_output_after_respawn().await?;
            if self
                .render_deadline
                .is_some_and(|deadline| deadline <= Instant::now())
            {
                return self.emit_ready_pending().await;
            }
            if self.surface_closed {
                return self.emit_ready_pending().await;
            }
            if self.output_closed {
                let event = self.surface.next().await?;
                self.observe_surface(event);
                continue;
            }

            // Output cursor responses precede their correlated surface wake.
            // Consume them first so a concurrent terminal surface event cannot
            // discard the final lag notice; an expired debounce still wins.
            if let Some(deadline) = self.render_deadline {
                tokio::select! {
                    biased;
                    _ = tokio::time::sleep_until(deadline) => {
                        return self.emit_ready_pending().await;
                    }
                    chunk = self.output.next() => {
                        self.observe_output(chunk?);
                    }
                    event = self.surface.next() => {
                        self.observe_surface(event?);
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    chunk = self.output.next() => {
                        self.observe_output(chunk?);
                    }
                    event = self.surface.next() => {
                        self.observe_surface(event?);
                    }
                }
            }
        }
    }

    fn observe_surface(&mut self, event: Option<PaneSurfaceEvent>) {
        match event {
            Some(PaneSurfaceEvent::Reset(frame)) => {
                if frame.epoch > self.surface_epoch {
                    self.surface_epoch = frame.epoch;
                    if self.output_generation == OutputGenerationPhase::AwaitingReset {
                        self.output_generation = OutputGenerationPhase::Respawned {
                            output_boundary: frame.next_output_sequence,
                        };
                    }
                }
                self.observe_surface_frame(frame);
            }
            Some(PaneSurfaceEvent::Patch(frame)) => self.observe_surface_frame(frame),
            Some(PaneSurfaceEvent::Lifecycle(PaneStreamLifecycleEvent::ProcessExited {
                ..
            })) => {
                self.output_generation = OutputGenerationPhase::AwaitingReset;
            }
            Some(PaneSurfaceEvent::End(_)) | None => {
                self.surface_closed = true;
            }
        }
    }

    fn observe_surface_frame(&mut self, frame: PaneSurfaceFrame) {
        let next_output_sequence = frame.next_output_sequence;
        let snapshot = frame.snapshot.grid;
        if self.last_snapshot.revision == snapshot.revision
            || self
                .pending_snapshot
                .as_ref()
                .is_some_and(|pending| pending.revision >= snapshot.revision)
        {
            return;
        }
        self.pending_snapshot = Some(snapshot);
        self.pending_output_boundary = Some(next_output_sequence);
        self.render_deadline
            .get_or_insert_with(|| Instant::now() + self.debounce);
    }

    async fn reopen_output_after_respawn(&mut self) -> Result<()> {
        if !self.output_closed
            || !matches!(
                self.output_generation,
                OutputGenerationPhase::Respawned { .. }
            )
        {
            return Ok(());
        }

        // Consume one authoritative exit -> reset transition before opening.
        // If the pane disappears concurrently, return that operation error
        // once instead of retrying forever against a dead logical pane.
        let output = self.pane.output_stream().await?;
        self.output = output;
        self.output_closed = false;
        self.output_generation = OutputGenerationPhase::Live;
        Ok(())
    }

    fn observe_output(&mut self, chunk: Option<PaneOutputChunk>) {
        match chunk {
            Some(PaneOutputChunk::Lag(lag)) => self.pending_lag = Some(lag),
            Some(PaneOutputChunk::Bytes { sequence, bytes }) => {
                if bytes.is_empty() {
                    self.output_closed = true;
                    self.output_generation = match self.output_generation {
                        OutputGenerationPhase::Respawned { output_boundary }
                            if sequence < output_boundary =>
                        {
                            self.output_generation
                        }
                        OutputGenerationPhase::Respawned { .. } => OutputGenerationPhase::Live,
                        phase => phase,
                    };
                }
            }
            None => self.output_closed = true,
        }
    }

    fn emit_pending(&mut self) -> Option<RenderUpdate> {
        self.render_deadline = None;
        self.pending_output_boundary = None;
        let snapshot = match self.pending_snapshot.take() {
            Some(snapshot) => {
                self.last_snapshot = snapshot.clone();
                snapshot
            }
            None => {
                let lag = self.pending_lag.take()?;
                return Some(RenderUpdate {
                    snapshot: self.last_snapshot.clone(),
                    lag: Some(lag),
                });
            }
        };
        Some(RenderUpdate {
            snapshot,
            lag: self.pending_lag.take(),
        })
    }

    async fn emit_ready_pending(&mut self) -> Result<Option<RenderUpdate>> {
        let Some(boundary) = self.pending_output_boundary else {
            return Ok(self.emit_pending());
        };
        let mut stale_empty_polls = 0_u8;
        while !self.output_closed && self.output.next_sequence() < boundary {
            let before = self.output.next_sequence();
            let chunks = self.output.poll_once().await?;
            for chunk in chunks {
                self.observe_output(Some(chunk));
            }
            if self.output.next_sequence() <= before {
                stale_empty_polls = stale_empty_polls.saturating_add(1);
                if stale_empty_polls > 1 {
                    return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                        "pane surface advanced beyond its correlated output cursor".to_owned(),
                    )));
                }
            } else {
                stale_empty_polls = 0;
            }
        }
        Ok(self.emit_pending())
    }
}

impl std::fmt::Debug for PaneRenderStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let phase = if self.render_deadline.is_some() {
            "debouncing"
        } else if self.surface_closed {
            "closed"
        } else {
            "waiting"
        };
        formatter
            .debug_struct("PaneRenderStream")
            .field("pane", &self.pane)
            .field("phase", &phase)
            .field("debounce", &self.debounce)
            .field("last_revision", &self.last_snapshot.revision)
            .field("pending_lag", &self.pending_lag)
            .field("pending_snapshot", &self.pending_snapshot.is_some())
            .field("render_deadline", &self.render_deadline)
            .field("output_closed", &self.output_closed)
            .field("output_generation", &self.output_generation)
            .field("surface_epoch", &self.surface_epoch)
            .field("surface_closed", &self.surface_closed)
            .finish_non_exhaustive()
    }
}

async fn initial_surface_frame(surface: &mut PaneSurfaceStream) -> Result<PaneSurfaceFrame> {
    loop {
        match surface.next().await? {
            Some(PaneSurfaceEvent::Reset(frame)) => return Ok(frame),
            Some(PaneSurfaceEvent::Lifecycle(_)) => {}
            Some(PaneSurfaceEvent::End(reason)) => {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
                    "pane surface stream ended during render setup: {reason:?}"
                ))));
            }
            Some(PaneSurfaceEvent::Patch(_)) => {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "pane surface stream emitted a patch before its initial reset".to_owned(),
                )));
            }
            None => {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "pane surface stream closed before its initial reset".to_owned(),
                )));
            }
        }
    }
}
