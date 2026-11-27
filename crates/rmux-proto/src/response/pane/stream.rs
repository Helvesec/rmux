//! Recoverable pane-stream responses: raw rebases, surface frames and lifecycle events.

use serde::{Deserialize, Serialize};

use super::{PaneSnapshotCell, PaneSnapshotCursor, PaneSnapshotResponse};
use crate::{PaneId, PaneOutputSubscriptionId, PaneTarget};

/// Why a raw pane stream emitted a new authoritative keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PaneRawRebaseReason {
    /// Initial authoritative boundary created when the stream opens.
    Initial,
    /// The consumer fell behind the bounded output ring.
    Lag,
    /// Pane geometry changed without a corresponding output byte.
    Resize,
    /// Retained terminal history was explicitly cleared.
    ClearHistory,
    /// An incomplete parser sequence expired.
    ParserStateExpired,
    /// The terminal state was reset.
    TerminalReset,
    /// Another cold-path transcript mutation invalidated continuation.
    TranscriptMutation,
    /// The pane process generation changed.
    GenerationChanged,
}

/// Completeness report for a bounded raw recovery keyframe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRecoveryCoverage {
    /// Total scrollback rows retained by the daemon at the capture boundary.
    pub history_rows_total: u64,
    /// Newest whole scrollback rows represented by the keyframe.
    pub history_rows_included: u64,
    /// Whether terminal metadata carried by the keyframe is lossless.
    ///
    /// `true` guarantees exact reconstruction of stored title, OSC 7 path,
    /// title-stack, and hyperlink values. `false` means at least one recovery
    /// metadata value was omitted, shortened to fit a byte budget, or stripped
    /// of control characters for safe OSC replay. This is independent of the
    /// history-row coverage above; the visible cell grid remains authoritative.
    pub metadata_complete: bool,
}

impl PaneRecoveryCoverage {
    /// Returns whether every retained scrollback row is represented.
    #[must_use]
    pub const fn history_complete(self) -> bool {
        self.history_rows_included >= self.history_rows_total
    }
}

/// Complete raw-emulator recovery state delivered in-band.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRawRebase {
    /// Stream-local epoch stamped onto subsequent raw byte events.
    pub epoch: u64,
    /// Pane process generation observed by the atomic boundary.
    pub generation: u64,
    /// Cold-path invalidation revision observed by the atomic boundary.
    pub invalidation_revision: u64,
    /// First pane-output sequence not represented by this keyframe.
    pub next_sequence: u64,
    /// Terminal width represented by the keyframe.
    pub cols: u16,
    /// Terminal height represented by the keyframe.
    pub rows: u16,
    /// ANSI bytes that reset and reconstruct a compatible terminal emulator.
    pub keyframe: Vec<u8>,
    /// Whether the captured pane is currently using its alternate screen.
    pub alternate: bool,
    /// Explicit report for bounded scrollback and terminal metadata.
    pub coverage: PaneRecoveryCoverage,
    /// Optional typed snapshot requested by an inspection-oriented consumer.
    pub snapshot: Option<PaneSnapshotResponse>,
    /// Cause of this rebase.
    pub reason: PaneRawRebaseReason,
}

/// One raw byte event tied to a preceding rebase epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRawBytes {
    /// Stream-local epoch established by the latest raw rebase.
    pub epoch: u64,
    /// Monotonic pane-output sequence.
    pub sequence: u64,
    /// Exact output bytes published for this sequence.
    pub bytes: Vec<u8>,
}

/// One OSC 8 hyperlink referenced by a structured pane surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceHyperlink {
    /// Non-zero hyperlink inner ID stored by visible cells.
    pub id: u32,
    /// OSC 8 target URI associated with `id`.
    pub uri: String,
}

/// Application-defined OSC 10/11/12 colours.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceDynamicColors {
    /// OSC 10 default foreground colour, when defined by the application.
    pub foreground: Option<String>,
    /// OSC 11 default background colour, when defined by the application.
    pub background: Option<String>,
    /// OSC 12 cursor colour, when defined by the application.
    pub cursor: Option<String>,
}

/// Authoritative structured pane state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceSnapshot {
    /// Visible pane width in terminal columns.
    pub cols: u16,
    /// Visible pane height in terminal rows.
    pub rows: u16,
    /// Row-major visible cells, exactly `cols * rows` long.
    pub cells: Vec<PaneSnapshotCell>,
    /// URI metadata keyed by the non-zero hyperlink IDs stored in `cells`.
    pub hyperlinks: Vec<PaneSurfaceHyperlink>,
    /// Current cursor state.
    pub cursor: PaneSnapshotCursor,
    /// Terminal title observed at this boundary.
    pub title: String,
    /// Terminal-reported working-directory path.
    pub path: String,
    /// Application-defined OSC 10/11/12 colours.
    pub dynamic_colors: PaneSurfaceDynamicColors,
    /// Whether title, path, and visible-cell hyperlink metadata was copied
    /// completely into this transport-bounded snapshot.
    pub metadata_complete: bool,
    /// Raw terminal mode bitset.
    pub mode_bits: u32,
    /// Whether the alternate screen is active.
    pub alternate: bool,
    /// Inclusive top row of the scrolling region.
    pub scroll_top: u32,
    /// Inclusive bottom row of the scrolling region.
    pub scroll_bottom: u32,
    /// Number of retained scrollback rows.
    pub history_size: u64,
    /// Bytes retained by the terminal history representation.
    pub history_bytes: u64,
    /// Monotonic pane-grid revision shared with `PaneSnapshotResponse`.
    pub revision: u64,
}

/// A self-contained surface frame; no earlier patch is required to apply it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSurfaceFrame {
    /// Stream-local epoch established by the latest surface reset.
    pub epoch: u64,
    /// Monotonic revision of the complete surface projection.
    pub revision: u64,
    /// First pane-output sequence not represented by this atomic surface.
    pub next_output_sequence: u64,
    /// Complete authoritative surface represented by this frame.
    pub snapshot: PaneSurfaceSnapshot,
}

/// Pane lifecycle observations that do not terminate the logical stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PaneStreamLifecycleEvent {
    /// The current child process exited after its output reached EOF.
    ///
    /// A kept pane may later be respawned on the same subscription.
    ProcessExited {
        /// Exact output-ring sequence occupied by the EOF marker when it was
        /// still retained. `None` means lifecycle was recovered across an
        /// invalidation or retention gap; the adjacent rebase is authoritative
        /// for raw sequence continuation.
        output_sequence: Option<u64>,
    },
}

/// Distinguishes logical pane removal from subscription termination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PaneStreamEndReason {
    /// The logical pane resource was removed.
    PaneRemoved,
    /// The caller no longer has permission to observe the pane.
    AccessRevoked,
    /// The bounded subscription could not keep up.
    SlowConsumer,
    /// The idle subscription lease expired.
    SubscriptionExpired,
    /// The underlying detached transport was lost.
    TransportLost,
    /// The daemon could not materialize the requested projection.
    ProjectionFailed,
}

/// One item in either recoverable pane projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PaneStreamEvent {
    /// Authoritative raw-emulator reset and reconstruction payload.
    RawRebase(Box<PaneRawRebase>),
    /// Exact raw output following the latest rebase.
    RawBytes(PaneRawBytes),
    /// Complete authoritative surface establishing a new epoch.
    SurfaceReset(Box<PaneSurfaceFrame>),
    /// Self-contained authoritative surface update.
    SurfacePatch(Box<PaneSurfaceFrame>),
    /// Non-terminal process lifecycle observation.
    Lifecycle(PaneStreamLifecycleEvent),
    /// Typed terminal or subscription termination.
    End(PaneStreamEndReason),
}

/// Atomic initial event and already-reserved pane stream subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneStreamResponse {
    /// Opaque stream subscription allocated by the daemon.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Resolved pane slot at subscription time.
    pub target: PaneTarget,
    /// Stable pane identity at subscription time.
    pub pane_id: PaneId,
    /// Initial authoritative event.
    pub event: PaneStreamEvent,
}

/// Batched recoverable pane stream events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStreamCursorResponse {
    /// Polled stream subscription.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Events delivered in stream order.
    pub events: Vec<PaneStreamEvent>,
    /// Whether this response stopped at the server-side batch cap.
    pub limited: bool,
}

/// Result of explicitly closing a recoverable pane stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneStreamResponse {
    /// Requested stream subscription.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Whether a live stream was removed.
    pub removed: bool,
}
