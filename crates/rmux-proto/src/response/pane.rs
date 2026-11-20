use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::{PaneId, PaneTarget, PaneTargetRef, ResizePaneAdjustment, RmuxError, WindowTarget};

mod snapshot;
pub use snapshot::{PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse};

mod output;
pub use output::{
    PaneOutputCursor, PaneOutputCursorResponse, PaneOutputEvent, PaneOutputLagNotice,
    PaneOutputLagResponse, PaneRecentOutput, SubscribePaneOutputResponse,
    UnsubscribePaneOutputResponse,
};

mod state;
pub use state::{
    ForegroundFieldSource, ForegroundSourcesDto, ForegroundStateDto, PaneForegroundStateResponse,
    PaneOptionEntry, PaneStateClosedReason, PaneStateCursorResponse, PaneStateEventDto,
    PaneStateLagResponse, PaneStateSnapshot, SubscribePaneStateResponse,
    UnsubscribePaneStateResponse,
};

mod stream;
pub use stream::{
    PaneRawBytes, PaneRawRebase, PaneRawRebaseReason, PaneRecoveryCoverage,
    PaneStreamCursorResponse, PaneStreamEndReason, PaneStreamEvent, PaneStreamLifecycleEvent,
    PaneSurfaceDynamicColors, PaneSurfaceFrame, PaneSurfaceHyperlink, PaneSurfaceSnapshot,
    SubscribePaneStreamResponse, UnsubscribePaneStreamResponse,
};

/// Response payload for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowResponse {
    /// The newly created pane target.
    pub pane: PaneTarget,
}

/// Atomic identity returned by the capability-gated SDK split endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowIdentityResponse {
    /// The newly created pane target using its visible `pane-base-index`.
    pub pane: PaneTarget,
    /// The newly created pane's stable daemon-lifetime identity.
    pub pane_id: PaneId,
}

/// Response payload for `swap-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapPaneResponse {
    /// The source slot involved in the swap.
    pub source: PaneTarget,
    /// The destination slot involved in the swap.
    pub target: PaneTarget,
}

/// Response payload for `move-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePaneResponse {
    /// The pane after it joined the destination window.
    pub target: PaneTarget,
}

/// Response payload for `last-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPaneResponse {
    /// The pane that became active.
    pub target: PaneTarget,
}

/// Response payload for `join-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPaneResponse {
    /// The pane after it joined the destination window.
    pub target: PaneTarget,
}

/// Response payload for `break-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakPaneResponse {
    /// The pane after it moved into its own window.
    pub target: PaneTarget,
    /// Optional printable output for `break-pane -P`.
    #[serde(default)]
    pub output: Option<CommandOutput>,
}

impl BreakPaneResponse {
    /// Returns the optional printable pane target output.
    #[must_use]
    pub const fn command_output(&self) -> Option<&CommandOutput> {
        self.output.as_ref()
    }
}

/// Response payload for `kill-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillPaneResponse {
    /// The pane that was removed.
    pub target: PaneTarget,
    /// Whether killing the pane also destroyed its window.
    pub window_destroyed: bool,
}

/// Response payload for `resize-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizePaneResponse {
    /// The pane that was resized.
    pub target: PaneTarget,
    /// The applied resize semantics.
    pub adjustment: ResizePaneAdjustment,
}

/// Response payload for `display-panes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPanesResponse {
    /// The active window that received the overlay.
    pub target: WindowTarget,
    /// The number of pane labels included in the overlay.
    pub pane_count: u32,
}

/// Response payload for `pipe-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipePaneResponse {
    /// The addressed pane.
    pub target: PaneTarget,
}

/// Response payload for `respawn-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespawnPaneResponse {
    /// The respawned pane target.
    pub target: PaneTarget,
}

/// Response payload for `select-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectPaneResponse {
    /// The pane that became active.
    pub target: PaneTarget,
}

/// Response payload for `send-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysResponse {
    /// The number of key tokens accepted by the server.
    pub key_count: usize,
}

/// Successful delivery for one target in a pane-input broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBroadcastInputSuccess {
    /// Zero-based target index from the request.
    pub target_index: u32,
    /// Resolved pane target that accepted the input.
    pub target: PaneTarget,
    /// Stable pane identity observed while resolving the target.
    pub pane_id: Option<PaneId>,
}

/// Failed delivery for one target in a pane-input broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBroadcastInputFailure {
    /// Zero-based target index from the request.
    pub target_index: u32,
    /// Original target that failed to receive the input.
    pub target: PaneTargetRef,
    /// Per-pane protocol error.
    pub error: RmuxError,
}

/// Response payload for a daemon-side pane-input broadcast.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBroadcastInputResponse {
    /// Number of key/text tokens accepted per successful target.
    pub key_count: usize,
    /// Targets that accepted the input, in request order.
    pub successes: Vec<PaneBroadcastInputSuccess>,
    /// Targets that rejected the input, in request order.
    pub failures: Vec<PaneBroadcastInputFailure>,
}

/// Response payload for SDK pane option mutations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOptionSetResponse {
    /// Stable pane id resolved by the daemon.
    pub pane_id: PaneId,
    /// Canonical option name.
    pub name: String,
    /// Exact explicit value before the mutation.
    pub old_value: Option<String>,
    /// Exact explicit value after the mutation.
    pub new_value: Option<String>,
    /// Whether the explicit value changed.
    pub changed: bool,
}

/// Response payload for SDK pane option lookups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOptionGetResponse {
    /// Stable pane id resolved by the daemon.
    pub pane_id: PaneId,
    /// Canonical option name.
    pub name: String,
    /// Exact pane-local explicit value.
    pub value: Option<String>,
}

/// Response payload for `list-panes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPanesResponse {
    /// The pre-rendered stdout bytes for the CLI.
    pub output: CommandOutput,
}

impl ListPanesResponse {
    /// Returns the reusable stdout payload for the list command.
    #[must_use]
    pub fn command_output(&self) -> &CommandOutput {
        &self.output
    }
}
