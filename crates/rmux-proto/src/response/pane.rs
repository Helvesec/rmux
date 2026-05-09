use serde::{Deserialize, Serialize};

use super::CommandOutput;
use crate::{PaneId, PaneOutputSubscriptionId, PaneTarget, ResizePaneAdjustment, WindowTarget};

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
