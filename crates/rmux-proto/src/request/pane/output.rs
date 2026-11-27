//! Pane snapshot, output-subscription and recoverable-stream requests.

use serde::{Deserialize, Serialize};

use crate::{PaneOutputSubscriptionId, PaneTarget, PaneTargetRef};

/// Request payload for the daemon-backed pane snapshot endpoint.
///
/// Unlike [`CapturePaneRequest`](crate::CapturePaneRequest), which returns a
/// pre-rendered byte stream of the visible viewport, this request asks the
/// daemon to expose its live in-memory grid as structured cells. The daemon
/// reads the cells directly from the rmux-core screen that is fed by its
/// crate-private terminal parser, so there is no `String::from_utf8_lossy`
/// reconstruction step on either side of the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotRequest {
    /// The exact pane target whose visible viewport should be captured.
    pub target: PaneTarget,
}

/// Starting position for a pane-output subscription cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneOutputSubscriptionStart {
    /// Start after the newest output currently retained by the pane.
    Now,
    /// Start at the oldest retained output event.
    Oldest,
}

/// Request payload for subscribing to live pane-output events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneOutputRequest {
    /// The exact pane target whose output should be subscribed.
    pub target: PaneTarget,
    /// The initial cursor position.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for subscribing to live pane-output events by slot or id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneOutputRefRequest {
    /// The exact pane target or stable pane id whose output should be
    /// subscribed.
    pub target: PaneTargetRef,
    /// The initial cursor position.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for unsubscribing from live pane-output events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneOutputRequest {
    /// The subscription to remove.
    pub subscription_id: PaneOutputSubscriptionId,
}

/// Request payload for polling a pane-output subscription cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutputCursorRequest {
    /// The subscription whose cursor should be polled.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Optional caller-requested event cap. The server clamps this to the
    /// recorded v1 default batch limit.
    #[serde(default)]
    pub max_events: Option<u16>,
}

/// Projection selected for a recoverable pane stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PaneStreamMode {
    /// Raw terminal bytes with automatic in-band renderer rebases.
    Raw,
    /// Authoritative structured viewport frames.
    Surface,
}

/// Request payload for opening a recoverable pane stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneStreamRequest {
    /// Stable pane identity or pane slot resolved atomically by the daemon.
    pub target: PaneTargetRef,
    /// Requested projection.
    pub mode: PaneStreamMode,
    /// Include a typed snapshot with raw rebases.
    ///
    /// Raw emulator consumers normally leave this false; inspection tools can
    /// request it without forcing every rebase to carry duplicate state.
    pub include_snapshot: bool,
}

/// Request payload for polling a recoverable pane stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStreamCursorRequest {
    /// Opaque stream subscription allocated by the daemon.
    pub subscription_id: PaneOutputSubscriptionId,
    /// Optional caller cap, clamped to the daemon subscription limit.
    #[serde(default)]
    pub max_events: Option<u16>,
}

/// Request payload for closing a recoverable pane stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneStreamRequest {
    /// Opaque stream subscription allocated by the daemon.
    pub subscription_id: PaneOutputSubscriptionId,
}
