use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::{
    PaneOutputSubscriptionId, PaneTarget, ResizePaneAdjustment, SessionName, SplitDirection,
    WindowTarget,
};

/// Target forms accepted by `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SplitWindowTarget {
    /// Splits the active pane in the addressed session.
    Session(SessionName),
    /// Splits the addressed pane directly.
    Pane(PaneTarget),
}

/// Request payload for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowRequest {
    /// The exact split target.
    pub target: SplitWindowTarget,
    /// The requested split direction.
    pub direction: SplitDirection,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
}

/// Extended request payload for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowExtRequest {
    /// The exact split target.
    pub target: SplitWindowTarget,
    /// The requested split direction.
    pub direction: SplitDirection,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Optional command argv for the new pane. A single argument runs via `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

/// The supported relative directions for `swap-pane`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwapPaneDirection {
    /// Swap the target pane with the next pane.
    Down,
    /// Swap the target pane with the previous pane.
    Up,
}

/// Request payload for `swap-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SwapPaneRequest {
    /// The source pane slot.
    pub source: PaneTarget,
    /// The destination pane slot.
    pub target: PaneTarget,
    /// The optional relative swap direction for `-D` or `-U`.
    #[serde(default)]
    pub direction: Option<SwapPaneDirection>,
    /// Whether pane selection should remain detached from the swap.
    pub detached: bool,
    /// Whether zoomed windows should be restored after the swap (`-Z`).
    #[serde(default)]
    pub preserve_zoom: bool,
}

/// Request payload for `last-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastPaneRequest {
    /// The addressed window.
    pub target: WindowTarget,
}

/// Request payload for `join-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinPaneRequest {
    /// The source pane being moved.
    pub source: PaneTarget,
    /// The destination pane the source is joined next to.
    pub target: PaneTarget,
    /// The layout direction requested for the join.
    pub direction: SplitDirection,
    /// Whether the destination pane should remain inactive after the join.
    pub detached: bool,
    /// Whether the source pane should be inserted before the target pane.
    #[serde(default)]
    pub before: bool,
    /// Whether the source pane should span the full window.
    #[serde(default)]
    pub full_size: bool,
    /// Optional requested size for the inserted pane.
    #[serde(default)]
    pub size: Option<PaneSplitSize>,
}

/// Request payload for `break-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakPaneRequest {
    /// The source pane being moved into its own window.
    pub source: PaneTarget,
    /// The optional destination window slot.
    pub target: Option<WindowTarget>,
    /// The optional explicit name for the new window.
    pub name: Option<String>,
    /// Whether the new window should remain inactive after the break.
    pub detached: bool,
    /// Whether the pane should be placed after the destination or current window.
    #[serde(default)]
    pub after: bool,
    /// Whether the pane should be placed before the destination or current window.
    #[serde(default)]
    pub before: bool,
    /// Whether the resulting pane target should be printed.
    #[serde(default)]
    pub print_target: bool,
    /// Optional format used when printing the resulting pane target.
    #[serde(default)]
    pub format: Option<String>,
}

/// Size forms accepted by pane split and join commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaneSplitSize {
    /// A concrete absolute size in cells.
    Absolute(u32),
    /// A percentage of the relevant base size.
    Percentage(u8),
}

/// Request payload for `move-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovePaneRequest {
    /// The source pane being moved.
    pub source: PaneTarget,
    /// The destination pane the source is joined next to.
    pub target: PaneTarget,
    /// The layout direction requested for the move.
    pub direction: SplitDirection,
    /// Whether the destination pane should remain inactive after the move.
    pub detached: bool,
    /// Whether the source pane should be inserted before the target pane.
    #[serde(default)]
    pub before: bool,
    /// Whether the source pane should span the full window.
    #[serde(default)]
    pub full_size: bool,
    /// Optional requested size for the inserted pane.
    #[serde(default)]
    pub size: Option<PaneSplitSize>,
}

/// Request payload for `kill-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillPaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Whether all panes except the target should be killed.
    #[serde(default)]
    pub kill_all_except: bool,
}

/// Request payload for `resize-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizePaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// The semantic resize request.
    pub adjustment: ResizePaneAdjustment,
}

/// Request payload for `display-panes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayPanesRequest {
    /// The exact session whose active window should receive the overlay.
    pub target: SessionName,
    /// Optional duration override in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Whether the command should return immediately without waiting for selection.
    #[serde(default)]
    pub non_blocking: bool,
    /// Whether pane selection should not run a follow-up command.
    #[serde(default)]
    pub no_command: bool,
    /// Optional template command executed after pane selection.
    #[serde(default)]
    pub template: Option<String>,
}

/// Request payload for `pipe-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PipePaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Whether pipe output should be written into the pane (`-I`).
    #[serde(default)]
    pub stdin: bool,
    /// Whether pane output should be written into the pipe (`-O`).
    #[serde(default)]
    pub stdout: bool,
    /// Whether an existing pipe should be toggled off without reopening (`-o`).
    #[serde(default)]
    pub once: bool,
    /// The optional shell command. Omitting it closes any existing pipe.
    #[serde(default)]
    pub command: Option<String>,
}

/// Request payload for `respawn-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RespawnPaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Whether a running pane should be killed before respawning (`-k`).
    #[serde(default)]
    pub kill: bool,
    /// Optional working-directory override.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Optional shell command argv. A single argument is executed via `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
}

/// Request payload for `select-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectPaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Optional pane title to set without changing the active pane (`-T`).
    #[serde(default)]
    pub title: Option<String>,
}

/// Direction used by `select-pane -U/-D/-L/-R`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectPaneDirection {
    /// Select the pane above the target pane.
    Up,
    /// Select the pane below the target pane.
    Down,
    /// Select the pane to the left of the target pane.
    Left,
    /// Select the pane to the right of the target pane.
    Right,
}

/// Request payload for directional `select-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectPaneAdjacentRequest {
    /// The pane used as the directional anchor.
    pub target: PaneTarget,
    /// The requested adjacent-pane direction.
    pub direction: SelectPaneDirection,
}

/// Request payload for `select-pane -m` and `select-pane -M`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectPaneMarkRequest {
    /// The pane target used to resolve the current session/window context.
    pub target: PaneTarget,
    /// Whether to clear the existing marked pane instead of toggling the target.
    pub clear: bool,
    /// Optional pane title to set while applying the mark operation (`-T`).
    #[serde(default)]
    pub title: Option<String>,
}

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

/// Request payload for `send-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Key tokens in left-to-right order.
    pub keys: Vec<String>,
}

/// Extended request payload for `send-keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeysExtRequest {
    /// The optional explicit pane target.
    pub target: Option<PaneTarget>,
    /// Key tokens in left-to-right order.
    pub keys: Vec<String>,
    /// Whether tmux format expansion should be applied to each token first.
    pub expand_formats: bool,
    /// Whether each token should be interpreted as a hexadecimal byte value.
    pub hex: bool,
    /// Whether tokens should be sent as literal bytes instead of key names.
    #[serde(default)]
    pub literal: bool,
    /// Whether keys should be dispatched through the client's key table.
    pub dispatch_key_table: bool,
    /// Whether tokens describe copy-mode commands.
    pub copy_mode_command: bool,
    /// Whether the payload should be treated as a mouse event.
    pub forward_mouse_event: bool,
    /// Whether the target terminal should be reset before sending keys.
    pub reset_terminal: bool,
    /// Optional tmux repeat count for command or key dispatch.
    pub repeat_count: Option<usize>,
}
