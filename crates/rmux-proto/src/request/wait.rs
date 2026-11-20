//! `wait-for` channel requests and daemon-backed SDK output waits.

use serde::{Deserialize, Serialize};

use super::PaneOutputSubscriptionStart;
use crate::{PaneTarget, PaneTargetRef, SdkWaitId, SdkWaitOwnerId};

/// The supported `wait-for` operation modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaitForMode {
    /// Wait for the next signal on the named channel.
    Wait,
    /// Signal all current plain waiters on the named channel.
    Signal,
    /// Acquire the named server-local lock, waiting in FIFO order when held.
    Lock,
    /// Release the named server-local lock.
    Unlock,
}

/// Request payload for `wait-for`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForRequest {
    /// The server-local wait channel name.
    pub channel: String,
    /// The selected wait operation.
    pub mode: WaitForMode,
}

/// Request payload for a daemon-backed SDK byte wait.
///
/// This is intentionally distinct from tmux-compatible [`WaitForRequest`].
/// SDK waits are pane-output waits with typed IDs used only for cancellation
/// and teardown bookkeeping; they never signal or lock tmux `wait-for`
/// channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkWaitForOutputRequest {
    /// Opaque SDK transport owner for this wait.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
    /// Pane whose raw output stream is observed.
    pub target: PaneTarget,
    /// Raw byte sequence to search for in pane output.
    pub bytes: Vec<u8>,
    /// Cursor position used when arming the wait.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for a daemon-backed SDK byte wait by slot or stable id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdkWaitForOutputRefRequest {
    /// Opaque SDK transport owner for this wait.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
    /// Pane whose raw output stream is observed.
    pub target: PaneTargetRef,
    /// Raw byte sequence to search for in pane output.
    pub bytes: Vec<u8>,
    /// Cursor position used when arming the wait.
    pub start: PaneOutputSubscriptionStart,
}

/// Request payload for best-effort cancellation of a daemon-backed SDK wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelSdkWaitRequest {
    /// Opaque SDK transport owner for the wait being cancelled.
    pub owner_id: SdkWaitOwnerId,
    /// Wait ID allocated by the SDK under `owner_id`.
    pub wait_id: SdkWaitId,
}
