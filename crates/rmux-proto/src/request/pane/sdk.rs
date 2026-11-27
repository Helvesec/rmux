//! SDK pane requests that address panes by slot or stable pane id.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    PaneStateSubscriptionId, PaneTargetRef, ProcessCommand, ResizePaneAdjustment, SetOptionMode,
};

/// SDK pane input request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInputRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// Text or key tokens to send.
    pub keys: Vec<String>,
    /// Whether tokens should be written literally instead of interpreted as
    /// tmux-compatible key names.
    #[serde(default)]
    pub literal: bool,
}

/// SDK pane input broadcast request with stable pane-id targeting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneBroadcastInputRequest {
    /// Pane targets addressed in caller order.
    pub targets: Vec<PaneTargetRef>,
    /// Text or key tokens to send to each pane.
    pub keys: Vec<String>,
    /// Whether tokens should be written literally instead of interpreted as
    /// tmux-compatible key names.
    #[serde(default)]
    pub literal: bool,
}

/// SDK resize request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneResizeRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// The semantic resize request.
    pub adjustment: ResizePaneAdjustment,
}

/// SDK kill request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneKillRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// Whether all panes except the target should be killed.
    #[serde(default)]
    pub kill_all_except: bool,
}

/// SDK respawn request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneRespawnRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// Whether a running pane should be killed before respawning.
    #[serde(default)]
    pub kill: bool,
    /// Optional working-directory override.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Legacy optional shell command argv. A single argument is executed via
    /// `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
    /// Optional pane-local `remain-on-exit` override applied before respawn.
    #[serde(default)]
    pub keep_alive_on_exit: Option<bool>,
}

/// SDK snapshot request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshotRefRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
}

/// SDK select/title request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSelectRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// Optional pane title to set without changing the active pane.
    #[serde(default)]
    pub title: Option<String>,
}

/// SDK pane option mutation request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOptionSetRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// The tmux-style option name.
    pub name: String,
    /// The value to set. `None` is valid only when `unset` is true or when the
    /// option type supports value-less toggles.
    #[serde(default)]
    pub value: Option<String>,
    /// The scalar/array mutation mode.
    pub mode: SetOptionMode,
    /// Whether to remove the pane-local explicit value instead of setting it.
    #[serde(default)]
    pub unset: bool,
}

/// SDK pane option lookup request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOptionGetRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// The tmux-style option name.
    pub name: String,
}

/// SDK pane-state subscription request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribePaneStateRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
    /// Whether the initial snapshot and stream should include title changes.
    #[serde(default = "default_include_pane_state_title")]
    pub include_title: bool,
    /// Whether the initial snapshot and stream should include pane options.
    #[serde(default = "default_include_pane_state_options")]
    pub include_options: bool,
    /// Whether the initial snapshot and stream should include foreground state.
    #[serde(default)]
    pub include_foreground: bool,
}

/// SDK pane-state cursor request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneStateCursorRequest {
    /// The subscription to poll.
    pub subscription_id: PaneStateSubscriptionId,
    /// Return records whose revision is strictly greater than this value.
    pub after_revision: u64,
    /// Whether the server may hold the request until a new record appears.
    #[serde(default)]
    pub wait: bool,
    /// Optional per-response event cap.
    #[serde(default)]
    pub max_events: Option<u16>,
}

/// SDK pane-state unsubscription request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsubscribePaneStateRequest {
    /// The subscription to remove.
    pub subscription_id: PaneStateSubscriptionId,
}

/// SDK foreground-state request that can address a stable pane id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneForegroundStateRequest {
    /// The exact pane target or stable pane id.
    pub target: PaneTargetRef,
}

const fn default_include_pane_state_title() -> bool {
    true
}

const fn default_include_pane_state_options() -> bool {
    true
}
