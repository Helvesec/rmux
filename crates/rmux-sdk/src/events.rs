//! Inert pane event DTOs for SDK consumers.
//!
//! This module is the public SDK home for the tmux-compatible control-mode
//! pane event vocabulary. The [`types`] submodule defines the
//! [`PaneEvent`] enum and its leaf payloads; the parent
//! crate re-exports the public surface unchanged so SDK users import every
//! variant through `rmux_sdk` without ever depending on `rmux-core`,
//! `rmux-server`, `rmux-client`, or `rmux-pty`.
//!
//! The events here are *inert* DTOs. The SDK does not subscribe to,
//! resequence, or emit these events; the `rmux-server` control-mode
//! plumbing in `crates/rmux-server/src/control.rs` is the authoritative
//! producer, and the daemon-side ordering rules documented on
//! [`PaneEvent`] match that producer's behaviour.

mod pane_stream;
pub mod recovery;
pub mod render;
pub mod streams;
pub mod surface;
pub mod types;

pub use pane_stream::{PaneStreamEndReason, PaneStreamLifecycleEvent};
pub use recovery::{
    PaneRecoveryApplyError, PaneRecoveryEvent, PaneRecoveryOptions, PaneRecoveryRebase,
    PaneRecoveryRebaseReason, PaneRecoveryState, PaneRecoveryStream,
};
pub use render::{PaneRenderStream, RenderUpdate};
pub use streams::{
    PaneLagNotice, PaneLineItem, PaneLineStream, PaneOutputChunk, PaneOutputStart,
    PaneOutputStream, PaneRecentOutput,
};
pub use surface::{
    PaneSurfaceApplyError, PaneSurfaceEvent, PaneSurfaceFrame, PaneSurfaceSnapshot,
    PaneSurfaceState, PaneSurfaceStream,
};
pub use types::{
    PaneCommandStatus, PaneCommandSummary, PaneDisconnectReason, PaneEvent, PaneExitReason,
    PaneNotification, PanePermissionScope,
};
