//! Revisioned pane-state and foreground-state responses.

use serde::{Deserialize, Serialize};

use crate::{PaneId, PaneStateSubscriptionId};

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
#[non_exhaustive]
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
#[non_exhaustive]
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
#[non_exhaustive]
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
    /// The pane process exited, with the exit details the daemon observed.
    ///
    /// Sent instead of [`PaneStateEventDto::Closed`] for process exits to
    /// connections that negotiated
    /// [`CAPABILITY_SDK_PANE_STATE_EXIT_EVENTS`](crate::CAPABILITY_SDK_PANE_STATE_EXIT_EVENTS);
    /// every other connection receives the legacy `Closed` event, so the
    /// variant index stays append-only and older readers never see it.
    Exited {
        /// Global pane-state revision.
        revision: u64,
        /// Stable pane id.
        pane_id: PaneId,
        /// Terminal close reason.
        reason: PaneStateClosedReason,
        /// The pane process's exit status when the daemon observed one.
        exit_status: Option<i32>,
        /// The pane process's terminating signal when the daemon observed
        /// one.
        exit_signal: Option<i32>,
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
