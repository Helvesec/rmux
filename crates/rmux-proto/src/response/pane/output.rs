//! Live pane-output subscription responses.

use serde::{Deserialize, Serialize};

use crate::{PaneId, PaneOutputSubscriptionId, PaneTarget};

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
