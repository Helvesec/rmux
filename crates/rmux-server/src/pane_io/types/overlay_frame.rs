use super::super::attach_control::AttachControlBacklogReservation;

#[derive(Debug)]
pub(crate) struct OverlayFrame {
    pub(crate) frame: Vec<u8>,
    pub(crate) render_generation: u64,
    pub(crate) overlay_generation: u64,
    pub(crate) persistent: bool,
    pub(crate) row_diff: bool,
    /// Popup/menu ownership suppresses base-pane paints; other cached overlays
    /// (such as clocks and choosers) retain their existing refresh behavior.
    pub(crate) defer_pane_updates: bool,
    /// Recompose the cached base and overlay in a single terminal frame.
    pub(crate) repaint_background: bool,
    pub(crate) persistent_state_id: Option<u64>,
    // A coalesced repaint remains charged while deferred or being written.
    pub(crate) backlog_reservation: Option<AttachControlBacklogReservation>,
}

impl OverlayFrame {
    pub(crate) fn new(frame: Vec<u8>, render_generation: u64, overlay_generation: u64) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: false,
            row_diff: false,
            defer_pane_updates: false,
            repaint_background: false,
            persistent_state_id: None,
            backlog_reservation: None,
        }
    }

    pub(crate) fn persistent(
        frame: Vec<u8>,
        render_generation: u64,
        overlay_generation: u64,
    ) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: true,
            row_diff: false,
            defer_pane_updates: false,
            repaint_background: false,
            persistent_state_id: None,
            backlog_reservation: None,
        }
    }

    /// Only popup reader refreshes may elide unchanged, self-contained rows.
    /// Keep `frame` complete so queue replacement and screen restoration work.
    pub(crate) fn with_row_diff(mut self) -> Self {
        self.row_diff = true;
        self
    }

    pub(crate) fn with_deferred_pane_updates(mut self) -> Self {
        self.defer_pane_updates = true;
        self
    }

    pub(crate) fn with_background_repaint(mut self) -> Self {
        self.repaint_background = true;
        self
    }

    pub(crate) fn persistent_with_state(
        frame: Vec<u8>,
        render_generation: u64,
        overlay_generation: u64,
        persistent_state_id: u64,
    ) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: true,
            row_diff: false,
            defer_pane_updates: false,
            repaint_background: false,
            persistent_state_id: Some(persistent_state_id),
            backlog_reservation: None,
        }
    }
}
