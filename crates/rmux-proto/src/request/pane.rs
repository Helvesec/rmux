use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;

use crate::{
    PaneTarget, ProcessCommand, ResizePaneAdjustment, SessionName, SplitDirection, WindowTarget,
};

mod compat;

mod output;
pub use output::{
    PaneOutputCursorRequest, PaneOutputSubscriptionStart, PaneSnapshotRequest,
    PaneStreamCursorRequest, PaneStreamMode, SubscribePaneOutputRefRequest,
    SubscribePaneOutputRequest, SubscribePaneStreamRequest, UnsubscribePaneOutputRequest,
    UnsubscribePaneStreamRequest,
};
mod sdk;
pub use sdk::{
    PaneBroadcastInputRequest, PaneForegroundStateRequest, PaneInputRequest, PaneKillRequest,
    PaneOptionGetRequest, PaneOptionSetRequest, PaneResizeRequest, PaneRespawnRequest,
    PaneSelectRequest, PaneSnapshotRefRequest, PaneStateCursorRequest, SubscribePaneStateRequest,
    UnsubscribePaneStateRequest,
};
mod send_keys;
pub use send_keys::{SendKeysExt2Request, SendKeysExtRequest, SendKeysRequest};

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
    /// Whether the new pane is inserted *before* the target on the chosen
    /// axis (tmux `-b`). Default `false` puts the new pane after the target.
    #[serde(default)]
    pub before: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
}

/// Extended request payload for `split-window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SplitWindowExtRequest {
    /// The exact split target.
    pub target: SplitWindowTarget,
    /// The requested split direction.
    pub direction: SplitDirection,
    /// Whether the new pane is inserted *before* the target on the chosen
    /// axis (tmux `-b`). Default `false` puts the new pane after the target.
    #[serde(default)]
    pub before: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Legacy optional command argv for the new pane. A single argument runs
    /// via `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode for the new pane.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
    /// Optional working-directory override for the new pane process.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional pane-local `remain-on-exit` override applied before spawn.
    #[serde(default)]
    pub keep_alive_on_exit: Option<bool>,
    /// Whether pane selection should stay on the original pane after split.
    #[serde(default)]
    pub detached: bool,
    /// Optional tmux `-l` split size expression.
    #[serde(default)]
    pub size: Option<String>,
    /// Whether an existing zoomed window should remain zoomed after split.
    #[serde(default)]
    pub preserve_zoom: bool,
    /// Whether the new pane should split the full window root (`split-window -f`).
    #[serde(default)]
    pub full_size: bool,
    /// Raw bytes read from client stdin for `split-window -I`.
    #[serde(default)]
    pub stdin_payload: Option<Vec<u8>>,
}

/// Request payload for `split-window` carrying raw tmux target text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowTargetActionRequest {
    /// Optional raw `-t` target. `None` uses the requester's current pane.
    #[serde(default)]
    pub target: Option<String>,
    /// The requested split direction.
    pub direction: SplitDirection,
    /// Whether the new pane is inserted before the target.
    #[serde(default)]
    pub before: bool,
    /// Optional per-spawn environment overrides in `NAME=VALUE` form.
    #[serde(default)]
    pub environment: Option<Vec<String>>,
    /// Legacy optional command argv for the new pane.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode for the new pane.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
    /// Optional working-directory override for the new pane process.
    #[serde(default)]
    pub start_directory: Option<PathBuf>,
    /// Optional pane-local `remain-on-exit` override applied before spawn.
    #[serde(default)]
    pub keep_alive_on_exit: Option<bool>,
    /// Whether pane selection should stay on the original pane after split.
    #[serde(default)]
    pub detached: bool,
    /// Optional tmux `-l` split size expression.
    #[serde(default)]
    pub size: Option<String>,
    /// Whether an existing zoomed window should remain zoomed after split.
    #[serde(default)]
    pub preserve_zoom: bool,
    /// Whether the new pane should split the full window root.
    #[serde(default)]
    pub full_size: bool,
    /// Raw bytes read from client stdin for `split-window -I`.
    #[serde(default)]
    pub stdin_payload: Option<Vec<u8>>,
}

/// SDK split request that returns the new pane's visible slot and stable id
/// atomically with the mutation.
///
/// This is a capability-gated append-only wire extension. The existing
/// [`SplitWindowTargetActionRequest`] remains unchanged for CLI callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitWindowIdentityRequest {
    /// Complete split action resolved server-side before the mutation.
    pub action: SplitWindowTargetActionRequest,
}

impl<'de> Deserialize<'de> for SplitWindowExtRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SplitWindowExtRequest",
            &[
                "target",
                "direction",
                "before",
                "environment",
                "command",
                "process_command",
                "start_directory",
                "keep_alive_on_exit",
                "detached",
                "size",
                "preserve_zoom",
                "full_size",
                "stdin_payload",
            ],
            compat::SplitWindowExtRequestVisitor,
        )
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LastPaneRequest {
    /// The addressed window.
    pub target: WindowTarget,
    /// Whether an existing zoomed window should remain zoomed after selecting.
    #[serde(default)]
    pub preserve_zoom: bool,
    /// Optional input gating to apply to the newly selected pane.
    #[serde(default)]
    pub input_disabled: Option<bool>,
}

impl<'de> Deserialize<'de> for LastPaneRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "LastPaneRequest",
            &["target", "preserve_zoom", "input_disabled"],
            compat::LastPaneRequestVisitor,
        )
    }
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

/// Request payload for `resize-pane` carrying raw tmux target text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizePaneTargetActionRequest {
    /// Optional raw `-t` target. `None` uses the requester's current pane.
    #[serde(default)]
    pub target: Option<String>,
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
    /// Optional attached client that should receive the pane overlay.
    #[serde(default)]
    pub target_client: Option<String>,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    /// Legacy optional shell command argv. A single argument is executed via
    /// `$SHELL -c`.
    #[serde(default)]
    pub command: Option<Vec<String>>,
    /// Explicit process launch mode.
    #[serde(default)]
    pub process_command: Option<ProcessCommand>,
}

impl<'de> Deserialize<'de> for RespawnPaneRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "RespawnPaneRequest",
            &[
                "target",
                "kill",
                "start_directory",
                "environment",
                "command",
                "process_command",
            ],
            compat::RespawnPaneRequestVisitor,
        )
    }
}

/// Request payload for `select-pane`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectPaneRequest {
    /// The exact pane target.
    pub target: PaneTarget,
    /// Optional pane title to set without changing the active pane (`-T`).
    #[serde(default)]
    pub title: Option<String>,
    /// Optional pane input state mutation (`select-pane -d` / `-e`).
    #[serde(default)]
    pub input_disabled: Option<bool>,
    /// Whether an existing zoomed window should remain zoomed after selecting.
    #[serde(default)]
    pub preserve_zoom: bool,
    /// Optional pane style to set on the pane-local `window-style` option (`-P`).
    #[serde(default)]
    pub style: Option<String>,
}

impl<'de> Deserialize<'de> for SelectPaneRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SelectPaneRequest",
            &[
                "target",
                "title",
                "input_disabled",
                "preserve_zoom",
                "style",
            ],
            compat::SelectPaneRequestVisitor,
        )
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelectPaneAdjacentRequest {
    /// The pane used as the directional anchor.
    pub target: PaneTarget,
    /// The requested adjacent-pane direction.
    pub direction: SelectPaneDirection,
    /// Whether an existing zoomed window should remain zoomed after selecting.
    #[serde(default)]
    pub preserve_zoom: bool,
}

impl<'de> Deserialize<'de> for SelectPaneAdjacentRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_struct(
            "SelectPaneAdjacentRequest",
            &["target", "direction", "preserve_zoom"],
            compat::SelectPaneAdjacentRequestVisitor,
        )
    }
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
