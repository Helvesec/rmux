use crate::{InfoSnapshot, PaneId, PaneSnapshot, PaneTextMatch, Result};

use super::capture_pane::PaneCaptureBuilder;
use super::info::{
    current_pane_entry, current_pane_ref_for_id, pane_info_snapshot_for_absent_slot,
    pane_info_snapshot_for_id, pane_info_snapshot_for_slot,
};
use super::snapshot::pane_snapshot;
use super::Pane;

impl Pane {
    /// Returns the live daemon pane identity for this slot, when it is
    /// currently listed.
    ///
    /// Returns `Ok(None)` (rather than an error) for a stale slot, mirroring
    /// the [`Window`](super::super::Window)-handle stale-slot semantics.
    pub async fn id(&self) -> Result<Option<PaneId>> {
        let pane = self.begin_operation_handle();
        if pane.identity_preflight == super::PaneIdentityPreflight::Absent {
            return Ok(None);
        }
        if let Some(pane_id) = pane.stable_id {
            let current =
                current_pane_ref_for_id(&pane.transport, &pane.target.session_name, pane_id)
                    .await?;
            return Ok(current.map(|_| pane_id));
        }
        Ok(current_pane_entry(&pane.transport, &pane.target)
            .await?
            .map(|entry| entry.pane_id))
    }

    /// Checks whether this exact pane slot is currently listed by the
    /// daemon.
    pub async fn exists(&self) -> Result<bool> {
        Ok(self.id().await?.is_some())
    }

    /// Returns a sticky info snapshot scoped to this pane's session,
    /// window, and pane.
    ///
    /// The snapshot is assembled from live `list-sessions`,
    /// `list-windows`, `list-panes`, and `display-message -p` responses so
    /// pane process state — running pid, exit state, geometry — reflects
    /// the daemon's current view rather than any handle-cached value.
    /// Stale slots return what is still observable: a session-only
    /// snapshot when the window or pane is gone, or an empty snapshot
    /// when the session itself is gone.
    pub async fn info(&self) -> Result<InfoSnapshot> {
        let pane = self.begin_operation_handle();
        match pane.stable_id {
            Some(pane_id) => {
                pane_info_snapshot_for_id(&pane.transport, &pane.target.session_name, pane_id).await
            }
            None if pane.identity_preflight == super::PaneIdentityPreflight::Absent => {
                pane_info_snapshot_for_absent_slot(&pane.transport, &pane.target).await
            }
            None => pane_info_snapshot_for_slot(&pane.transport, &pane.target).await,
        }
    }

    /// Captures the live pane grid as a [`PaneSnapshot`].
    ///
    /// The captured grid is read directly from the daemon's live
    /// rmux-core screen — the same in-memory grid that the crate-private
    /// terminal parser feeds from PTY output — so dimensions, cursor
    /// state, and per-cell glyph/attribute/colour data round-trip without
    /// any `capture-pane -p` text reconstruction step. Wide-glyph padding
    /// is preserved as padding cells in the row-major layout, raw bytes
    /// that are not valid UTF-8 stay isolated to the cell text payload
    /// rather than reaching helper output, and the daemon-derived
    /// [`revision`](PaneSnapshot::revision) is non-zero for a live pane
    /// and changes whenever any observable pane field mutates — output,
    /// resize, clear, exit. Stale slots resolve to a default empty
    /// snapshot whose revision is `0`, distinct from any prior live
    /// revision.
    pub async fn snapshot(&self) -> Result<PaneSnapshot> {
        pane_snapshot(&self.begin_operation_handle()).await
    }

    /// Starts a daemon `capture-pane` request builder.
    pub fn capture_pane(&self) -> PaneCaptureBuilder<'_> {
        PaneCaptureBuilder::new(self)
    }

    /// Captures a fresh snapshot and searches its rendered visible text for
    /// the first literal match.
    ///
    /// This is a lossy rendered-text helper built from
    /// [`PaneSnapshot::visible_lines`]. It does not inspect raw output bytes
    /// and does not use any daemon/core regex search surface.
    pub async fn find_text(&self, text: impl AsRef<str>) -> Result<Option<PaneTextMatch>> {
        crate::extract::find_text(&self.begin_operation_handle(), text.as_ref().to_owned()).await
    }

    /// Captures a fresh snapshot and returns every literal rendered-text
    /// match, including overlapping matches on the same visible line.
    ///
    /// See [`Self::find_text`] for rendered-text and coordinate semantics.
    pub async fn find_text_all(&self, text: impl AsRef<str>) -> Result<Vec<PaneTextMatch>> {
        crate::extract::find_text_all(&self.begin_operation_handle(), text.as_ref().to_owned())
            .await
    }
}
