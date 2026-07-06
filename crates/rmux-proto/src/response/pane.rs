use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::{
    PaneId, PaneOutputSubscriptionId, PaneStateSubscriptionId, PaneTarget, PaneTargetRef,
    ResizePaneAdjustment, RmuxError, WindowTarget,
};

/// Response payload for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowResponse {
    /// The newly created pane target.
    pub pane: PaneTarget,
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

/// Explicit pane option entry included in pane-state snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOptionEntry {
    /// Canonical option name.
    pub name: String,
    /// Exact explicit option value rendered by the daemon.
    pub value: String,
}

/// Source labels for best-effort foreground fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ForegroundFieldSource {
    /// Read from the foreground process or foreground process group.
    Process,
    /// Read from the root pane process.
    RootProcess,
    /// Read from the terminal OSC 7 path.
    Osc7,
    /// Read from the pane launch profile.
    Profile,
    /// Read from the session environment.
    Environment,
    /// Read from RMUX's runtime window-name cache.
    RuntimeName,
}

/// Per-field source report for best-effort foreground state.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundSourcesDto {
    /// Source for `pid`.
    #[serde(default)]
    pub pid: Option<ForegroundFieldSource>,
    /// Source for `command`.
    #[serde(default)]
    pub command: Option<ForegroundFieldSource>,
    /// Source for `cwd`.
    #[serde(default)]
    pub cwd: Option<ForegroundFieldSource>,
    /// Source for `exe`.
    #[serde(default)]
    pub exe: Option<ForegroundFieldSource>,
}

/// Best-effort foreground process state for a pane.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundStateDto {
    /// Foreground or root process id, when knowable.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Executable command name, when knowable.
    #[serde(default)]
    pub command: Option<String>,
    /// Current working directory, when knowable.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Executable path, when knowable.
    #[serde(default)]
    pub exe: Option<String>,
    /// Per-field source labels.
    #[serde(default)]
    pub sources: ForegroundSourcesDto,
}

/// Initial or rebased pane-state snapshot.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStateSnapshot {
    /// Global pane-state journal revision at snapshot time.
    pub revision: u64,
    /// Current pane title when requested.
    #[serde(default)]
    pub title: Option<String>,
    /// Current explicit pane-local options when requested.
    #[serde(default)]
    pub options: Vec<PaneOptionEntry>,
    /// Best-effort foreground state when requested.
    #[serde(default)]
    pub foreground: Option<ForegroundStateDto>,
}

/// Terminal reason for a pane-state stream close event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PaneStateClosedReason {
    /// The pane process exited and the pane was removed.
    Exited,
    /// The pane process died but remain-on-exit kept the pane around.
    DiedKept,
    /// The pane was killed by an explicit RMUX operation.
    Killed,
}

/// One revisioned pane-state event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneStateEventDto {
    /// The pane title changed.
    TitleChanged {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous title.
        old_title: String,
        /// New title.
        new_title: String,
    },
    /// A pane-local option was set or replaced.
    OptionSet {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
        /// New explicit value.
        new_value: String,
    },
    /// A pane-local option was unset.
    OptionUnset {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Canonical option name.
        name: String,
        /// Previous explicit value.
        old_value: Option<String>,
    },
    /// Best-effort foreground state changed.
    ForegroundChanged {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Previous foreground state.
        old_state: ForegroundStateDto,
        /// New foreground state.
        new_state: ForegroundStateDto,
    },
    /// The pane reached a terminal state for this subscription.
    Closed {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Terminal close reason.
        reason: PaneStateClosedReason,
    },
}

/// Response payload for subscribing to revisioned pane-state events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneStateResponse {
    /// The newly allocated subscription identifier.
    pub subscription_id: PaneStateSubscriptionId,
    /// Stable pane identity for the subscribed pane.
    pub pane_id: PaneId,
    /// Atomic initial snapshot.
    pub snapshot: PaneStateSnapshot,
}

/// Response payload for unsubscribing from pane-state events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneStateResponse {
    /// The requested subscription identifier.
    pub subscription_id: PaneStateSubscriptionId,
    /// Whether a live subscription was removed.
    pub removed: bool,
}

/// Response payload for a pane-state cursor request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStateCursorResponse {
    /// The polled subscription identifier.
    pub subscription_id: PaneStateSubscriptionId,
    /// Events delivered in strictly increasing revision order.
    pub events: Vec<PaneStateEventDto>,
    /// Revision callers should use as the next `after_revision` cursor.
    pub next_revision: u64,
}

/// Response payload for a pane-state subscription lag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStateLagResponse {
    /// The polled subscription identifier.
    pub subscription_id: PaneStateSubscriptionId,
    /// The stale cursor revision that fell behind retention.
    pub missed_from_revision: u64,
    /// Oldest retained revision after the gap.
    pub resume_revision: u64,
    /// Atomic rebased snapshot.
    pub snapshot: PaneStateSnapshot,
}

/// Response payload for a one-shot foreground state request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneForegroundStateResponse {
    /// Stable pane id resolved by the daemon.
    pub pane_id: PaneId,
    /// Journal revision observed when sampling.
    pub revision: u64,
    /// Best-effort foreground state. `None` means the pane disappeared.
    pub state: Option<ForegroundStateDto>,
}

/// Serializable pane-output cursor state returned by subscription endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputCursor {
    /// The next output sequence the cursor expects.
    pub next_sequence: u64,
    /// Total output events this cursor has skipped after explicit gaps.
    pub missed_events: u64,
}

/// One pane-output event delivered through a subscription cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputEvent {
    /// Monotonic per-pane output sequence.
    pub sequence: u64,
    /// Raw pane output bytes.
    pub bytes: Vec<u8>,
}

/// Recent live bytes included with a lag notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRecentOutput {
    /// Retained recent raw pane output bytes.
    pub bytes: Vec<u8>,
    /// Oldest output sequence contributing retained bytes.
    pub oldest_sequence: Option<u64>,
    /// Newest output sequence contributing retained bytes.
    pub newest_sequence: Option<u64>,
}

/// Explicit report for a subscription cursor that fell behind retention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputLagNotice {
    /// Sequence the subscriber expected before lag was detected.
    pub expected_sequence: u64,
    /// Oldest retained sequence where the subscriber can resume.
    pub resume_sequence: u64,
    /// Number of output events skipped by this lag notice.
    pub missed_events: u64,
    /// Newest output sequence appended when lag was detected.
    pub newest_sequence: u64,
    /// Bounded recent live output available at lag detection time.
    pub recent: PaneRecentOutput,
}

/// Response payload for subscribing to live pane-output events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneOutputResponse {
    /// The newly allocated subscription identifier.
    pub subscription_id: PaneOutputSubscriptionId,
    /// The resolved target at subscription time.
    pub target: PaneTarget,
    /// Stable pane identity for the subscribed pane.
    pub pane_id: PaneId,
    /// Initial cursor state.
    pub cursor: PaneOutputCursor,
}

/// Response payload for unsubscribing from live pane-output events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneOutputResponse {
    /// The requested subscription identifier.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Whether a live subscription was removed by this request.
    pub removed: bool,
}

/// Response payload for polling a live pane-output subscription cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputCursorResponse {
    /// The polled subscription identifier.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Cursor state after this poll.
    pub cursor: PaneOutputCursor,
    /// Output events delivered in ascending sequence order.
    pub events: Vec<PaneOutputEvent>,
    /// Whether this response stopped at the server-side batch cap.
    pub limited: bool,
}

/// Response payload for a pane-output subscription lag notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputLagResponse {
    /// The polled subscription identifier.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Cursor state after applying the lag notice.
    pub cursor: PaneOutputCursor,
    /// Detailed gap report.
    pub lag: PaneOutputLagNotice,
}

/// One captured pane cell on the daemon snapshot wire.
///
/// Cells are produced from rmux-core's structured `ScreenCellView`, so the
/// glyph text, recorded display width, and padding flag travel verbatim
/// across the wire. Padding cells (the trailing column of a wide glyph)
/// carry `width = 0` and `padding = true`; their `text` field carries the
/// space sentinel rmux-core uses to represent owned padding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotCell {
    /// Recorded glyph text payload.
    pub text: String,
    /// Recorded display width of the leading glyph; `0` for padding cells.
    pub width: u8,
    /// Whether this cell is wide-glyph padding for the preceding column.
    pub padding: bool,
    /// Raw cell attribute bitset.
    pub attributes: u16,
    /// Raw foreground colour encoding.
    pub fg: i32,
    /// Raw background colour encoding.
    pub bg: i32,
    /// Raw underline colour encoding.
    pub us: i32,
    /// Hyperlink inner ID.
    pub link: u32,
}

/// Captured cursor position on the daemon snapshot wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotCursor {
    /// Zero-based cursor row within the visible viewport.
    pub row: u16,
    /// Zero-based cursor column within the visible viewport.
    pub col: u16,
    /// Whether the cursor is visible according to the live mode bits.
    pub visible: bool,
    /// Raw cursor style value.
    pub style: u32,
}

/// Response payload for the daemon-backed pane snapshot endpoint.
///
/// `cells` is row-major with `row * cols + col` indexing and exactly
/// `cols * rows` entries. The daemon-derived `revision` is non-zero for
/// every captured live pane and changes whenever any observable field
/// (cells, cursor, output_sequence, history bytes/lines, pane id) changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotResponse {
    /// Visible pane width in terminal columns.
    pub cols: u16,
    /// Visible pane height in terminal rows.
    pub rows: u16,
    /// Row-major cells, `cols * rows` long.
    pub cells: Vec<PaneSnapshotCell>,
    /// Captured cursor coordinates and state.
    pub cursor: PaneSnapshotCursor,
    /// Daemon-derived revision counter for this captured state.
    pub revision: u64,
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
