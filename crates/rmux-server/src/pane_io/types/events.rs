use rmux_core::{PaneId, TerminalClipboardQuery};
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneAlertEvent {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) bell_count: u64,
    pub(crate) title_changed: bool,
    pub(crate) title_change: Option<(String, String)>,
    /// The pane reported a different OSC 7 working directory in this batch.
    /// Attached clients must re-render for it to reach their outer terminal,
    /// the same way a title change does (issue #182).
    pub(crate) path_changed: bool,
    pub(crate) clipboard_set: bool,
    /// Decoded payloads of the inbound OSC 52 clipboard writes in this batch, in
    /// arrival order. Each becomes a paste buffer under `set-clipboard on`
    /// (tmux's `paste_add` in input_osc_52); empty for a query or for panes that
    /// emitted no clipboard write.
    pub(crate) clipboard_writes: Vec<Vec<u8>>,
    /// Typed pane-originated OSC 52 queries in arrival order. The server
    /// consumes these independently of generic terminal passthrough rendering.
    pub(crate) clipboard_queries: Vec<TerminalClipboardQuery>,
    /// True when this batch toggled one of the pane's mouse-tracking modes
    /// (?1000/?1002/?1003). Attached clients must rebuild their outer
    /// terminal so pane-driven tracking reaches the outer terminal without
    /// waiting for an unrelated refresh (issue #93).
    pub(crate) mouse_mode_changed: bool,
    /// True when this batch entered or left the alternate screen. Pane
    /// scrollbars are suppressed there, so the pane PTY geometry must be
    /// reconciled immediately rather than waiting for an outer resize.
    pub(crate) alternate_mode_changed: bool,
    pub(crate) queue_activity_alert: bool,
    pub(crate) generation: Option<u64>,
}

/// Everything a pane's output publication hands to the server.
///
/// Alerts are a callback because they are coalesced per pane; window renames
/// are a sequence because their order across panes is the product contract.
/// Both live behind one handle so a reader cannot hold one without the other,
/// and so the reader can ask the sequence for room *before* it publishes and
/// before it takes any output or transcript lock.
pub(crate) struct PaneEffectSink {
    alert: Box<dyn Fn(PaneAlertEvent) + Send + Sync>,
    renames: Arc<crate::pane_rename_queue::PaneRenameQueue>,
}

impl std::fmt::Debug for PaneEffectSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneEffectSink")
            .field("renames", &self.renames)
            .finish_non_exhaustive()
    }
}

impl PaneEffectSink {
    pub(crate) fn new(
        alert: impl Fn(PaneAlertEvent) + Send + Sync + 'static,
        renames: Arc<crate::pane_rename_queue::PaneRenameQueue>,
    ) -> Self {
        Self {
            alert: Box::new(alert),
            renames,
        }
    }

    pub(crate) fn alert(&self, event: PaneAlertEvent) {
        (self.alert)(event);
    }

    pub(crate) fn renames(&self) -> &crate::pane_rename_queue::PaneRenameQueue {
        &self.renames
    }
}

pub(crate) type PaneAlertCallback = Arc<PaneEffectSink>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneExitEvent {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: Option<u64>,
    output_state: PaneExitOutputState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneExitOutputState {
    EofPublished,
    #[cfg(any(windows, test))]
    EofPending,
}

impl PaneExitEvent {
    pub(crate) fn eof_published(
        session_name: rmux_proto::SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Self {
        Self {
            session_name,
            pane_id,
            generation,
            output_state: PaneExitOutputState::EofPublished,
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) fn eof_pending(
        session_name: rmux_proto::SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Self {
        Self {
            session_name,
            pane_id,
            generation,
            output_state: PaneExitOutputState::EofPending,
        }
    }

    pub(crate) fn output_eof_published(&self) -> bool {
        matches!(self.output_state, PaneExitOutputState::EofPublished)
    }
}

pub(crate) type PaneExitCallback = Arc<dyn Fn(PaneExitEvent) + Send + Sync>;
