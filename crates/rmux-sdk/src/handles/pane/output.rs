use crate::events::streams::{PaneLineStream, PaneOutputStart, PaneOutputStream};
use crate::{
    CollectedPaneOutput, PaneRecoveryOptions, PaneRecoveryStream, PaneRenderStream,
    PaneSurfaceStream, Result, RmuxError,
};

use super::target::is_stale_pane_id_target_error;
use super::Pane;

impl Pane {
    /// Opens raw pane output that automatically repairs renderer state.
    ///
    /// The first item is a complete ANSI rebase. Resize, clear-history,
    /// parser expiry, lag, and process-generation changes emit later rebases
    /// in-band on the same subscription; callers never need to reopen it.
    pub async fn recover_output(&self) -> Result<PaneRecoveryStream> {
        self.recover_output_with(PaneRecoveryOptions::default())
            .await
    }

    /// Opens recoverable raw output with explicit cold-path options.
    pub async fn recover_output_with(
        &self,
        options: PaneRecoveryOptions,
    ) -> Result<PaneRecoveryStream> {
        let pane = self.begin_operation_handle();
        let target = pane.required_resolved_proto_target_ref().await?;
        crate::capabilities::require(
            &pane.transport,
            &[rmux_proto::CAPABILITY_SDK_PANE_RAW_RECOVERY],
        )
        .await?;
        match PaneRecoveryStream::open(pane.transport.clone(), target.clone(), options).await {
            Ok(stream) => Ok(stream),
            Err(error) if pane.is_stable_id() && is_stale_pane_id_target_error(&error, &target) => {
                let pane_id = pane
                    .stable_id
                    .expect("stable-id retry is guarded by is_stable_id");
                let retry_target = pane.resolved_proto_target_ref().await?.ok_or_else(|| {
                    RmuxError::pane_not_found(pane.target.session_name.clone(), pane_id)
                })?;
                PaneRecoveryStream::open(pane.transport.clone(), retry_target, options).await
            }
            Err(error) => Err(error),
        }
    }

    /// Opens an authoritative structured pane-surface stream.
    ///
    /// Every reset and patch is self-contained. The daemon renders one shared
    /// projection per pane rather than repeating terminal rendering for each
    /// subscriber.
    pub async fn surface_stream(&self) -> Result<PaneSurfaceStream> {
        let pane = self.begin_operation_handle();
        let target = pane.required_resolved_proto_target_ref().await?;
        crate::capabilities::require(
            &pane.transport,
            &[rmux_proto::CAPABILITY_SDK_PANE_SURFACE_STREAM],
        )
        .await?;
        match PaneSurfaceStream::open(pane.transport.clone(), target.clone()).await {
            Ok(stream) => Ok(stream),
            Err(error) if pane.is_stable_id() && is_stale_pane_id_target_error(&error, &target) => {
                let pane_id = pane
                    .stable_id
                    .expect("stable-id retry is guarded by is_stable_id");
                let retry_target = pane.resolved_proto_target_ref().await?.ok_or_else(|| {
                    RmuxError::pane_not_found(pane.target.session_name.clone(), pane_id)
                })?;
                PaneSurfaceStream::open(pane.transport.clone(), retry_target).await
            }
            Err(error) => Err(error),
        }
    }

    /// Subscribes to the live raw pane output starting now.
    ///
    /// Setup performs one `subscribe-pane-output` round trip and is
    /// fallible: a stale pane slot, a transport failure, or a refused
    /// daemon capability propagates as [`crate::RmuxError`].
    ///
    /// The returned [`PaneOutputStream`] preserves arbitrary bytes,
    /// pairs every chunk with the daemon's monotonic per-pane sequence,
    /// and surfaces any retained-output gaps as
    /// [`PaneOutputChunk::Lag`](crate::PaneOutputChunk::Lag) without ever
    /// converting raw bytes through `String::from_utf8_lossy`. Dropping
    /// the stream emits exactly one best-effort
    /// `unsubscribe-pane-output` request; if the unsubscribe is refused,
    /// late, or the transport is already gone the drop never closes the
    /// pane, its window/session/process, or the daemon itself.
    pub async fn output_stream(&self) -> Result<PaneOutputStream> {
        self.output_stream_starting_at(PaneOutputStart::Now).await
    }

    /// Subscribes to the live raw pane output, anchoring the cursor at
    /// the requested start position.
    ///
    /// See [`Self::output_stream`] for setup, drop, and lag semantics.
    pub async fn output_stream_starting_at(
        &self,
        start: PaneOutputStart,
    ) -> Result<PaneOutputStream> {
        let pane = self.begin_operation_handle();
        let target = if start == PaneOutputStart::Oldest {
            // The Oldest handler owns the live lookup and its retained-output
            // fallback. Preserve the caller's slot or stable-id reference so
            // an exited pane can be found after leaving the live inventory;
            // its subscription response supplies the selected pane identity.
            pane.proto_target_ref()
        } else {
            pane.required_resolved_proto_target_ref().await?
        };
        crate::capabilities::require(&pane.transport, &[rmux_proto::CAPABILITY_SDK_PANE_BY_ID])
            .await?;
        match PaneOutputStream::open(pane.transport.clone(), target.clone(), start).await {
            Ok(stream) => Ok(stream),
            Err(error) if pane.is_stable_id() && is_stale_pane_id_target_error(&error, &target) => {
                let pane_id = pane
                    .stable_id
                    .expect("stable-id retry is guarded by is_stable_id");
                let retry_target = pane.resolved_proto_target_ref().await?.ok_or_else(|| {
                    RmuxError::pane_not_found(pane.target.session_name.clone(), pane_id)
                })?;
                PaneOutputStream::open(pane.transport.clone(), retry_target, start).await
            }
            Err(error) => Err(error),
        }
    }

    /// Collects bounded raw pane output bytes until the pane process exits.
    ///
    /// Collection starts at the live output cursor, retains at most
    /// `max_bytes`, and keeps waiting for pane exit even after the cap is
    /// reached. Returned bytes are raw pane-output bytes; lag notices are
    /// reported on the returned [`CollectedPaneOutput`] and are not spliced
    /// into the byte buffer.
    pub async fn collect_output_until_exit(&self, max_bytes: usize) -> Result<CollectedPaneOutput> {
        crate::extract::collect_output_until_exit(self, max_bytes).await
    }

    /// Collects bounded raw pane output from the requested stream start until
    /// the pane process exits.
    ///
    /// See [`Self::collect_output_until_exit`] for cap, lag, and byte
    /// preservation semantics.
    pub async fn collect_output_until_exit_starting_at(
        &self,
        start: PaneOutputStart,
        max_bytes: usize,
    ) -> Result<CollectedPaneOutput> {
        crate::extract::collect_output_until_exit_starting_at(self, start, max_bytes).await
    }

    /// Subscribes to the live pane output rendered into UTF-8 lines.
    ///
    /// Setup is fallible (see [`Self::output_stream`]). Beyond the raw
    /// stream the line stream applies two well-isolated transformations:
    /// it splits on the LF byte `b'\n'` and runs each completed line
    /// through `String::from_utf8_lossy`, replacing every byte that is
    /// not valid UTF-8 with the Unicode replacement character `U+FFFD`.
    /// Bytes between LFs stay buffered until the next LF arrives, and a
    /// daemon-side lag drops the in-flight partial line; both
    /// transformations are documented in detail on the
    /// [`crate::events::streams`] module. Drop semantics match
    /// [`Self::output_stream`].
    pub async fn line_stream(&self) -> Result<PaneLineStream> {
        self.line_stream_starting_at(PaneOutputStart::Now).await
    }

    /// Subscribes to rendered output lines, anchoring the cursor at the
    /// requested start position.
    pub async fn line_stream_starting_at(&self, start: PaneOutputStart) -> Result<PaneLineStream> {
        let inner = self.output_stream_starting_at(start).await?;
        Ok(PaneLineStream::wrap(inner))
    }

    /// Opens a render stream backed by the daemon's shared surface projection.
    ///
    /// The daemon filters non-visual output before materializing a surface and
    /// shares that work across viewers. The SDK retains the legacy raw
    /// subscription only to preserve detailed [`crate::PaneLagNotice`] values.
    pub async fn render_stream(&self) -> Result<PaneRenderStream> {
        PaneRenderStream::open(self.clone()).await
    }
}
