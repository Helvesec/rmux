use std::io;

#[path = "handler_pane/attached_input.rs"]
mod pane_attached_input;
#[path = "handler_pane/attached_key_dispatch.rs"]
mod pane_attached_key_dispatch;
#[path = "handler_pane/broadcast.rs"]
mod pane_broadcast;
#[path = "handler_pane/by_id.rs"]
mod pane_by_id;
#[path = "handler_pane/by_id_selection.rs"]
mod pane_by_id_selection;
#[cfg(windows)]
#[path = "handler_pane/deferred_wait.rs"]
mod pane_deferred_wait;
#[path = "handler_pane/display_panes.rs"]
mod pane_display_panes;
#[path = "handler_pane/exit_probe.rs"]
mod pane_exit_probe;
#[path = "handler_pane/inspection.rs"]
mod pane_inspection;
#[path = "handler_pane/io_encoding.rs"]
mod pane_io_encoding;
#[path = "handler_pane/key_bindings.rs"]
mod pane_key_bindings;
#[path = "handler_pane/kill_effects.rs"]
mod pane_kill_effects;
#[path = "handler_pane/layout.rs"]
mod pane_layout;
#[path = "handler_pane/lifecycle.rs"]
mod pane_lifecycle;
#[path = "handler_pane/management.rs"]
mod pane_management;
#[path = "handler_pane/process.rs"]
mod pane_process;
#[path = "handler_pane/prompt_input.rs"]
mod pane_prompt_input;
#[path = "handler_pane/selection.rs"]
mod pane_selection;
#[path = "handler_pane/send_keys.rs"]
mod pane_send_keys;
#[path = "handler_pane/snapshot.rs"]
mod pane_snapshot;
#[path = "handler_pane/split_effects.rs"]
mod pane_split_effects;
#[path = "handler_pane/timer_mutations.rs"]
mod pane_timer_mutations;
#[path = "handler_pane/transfer.rs"]
mod pane_transfer;
#[cfg(windows)]
#[path = "handler_pane/windows_console_sequence.rs"]
mod pane_windows_console_sequence;

pub(in crate::handler) use pane_attached_input::bracketed_paste::{
    strip_bracketed_paste_markers, strip_bracketed_paste_markers_after_append,
};
pub(super) use pane_attached_input::retain_partial_attached_escape_input;
pub(super) use pane_by_id::resolve_pane_target_ref;
#[cfg(windows)]
pub(in crate::handler) use pane_deferred_wait::format_references_pane_pid;
pub(super) use pane_inspection::{
    attached_status_message_for_error, command_output_from_lines, display_time,
};
use pane_io_encoding::{
    encode_key_for_target, encode_mouse_for_target, encode_tokens_for_target,
    expand_send_key_tokens, pane_id_for_input_target, prepare_synchronized_pane_input_writes,
    write_bytes_to_target, write_bytes_to_targets, PaneInputWrite,
};
pub(super) use pane_io_encoding::{
    prepare_pane_input_write, write_attached_bytes_to_target_io, write_bytes_to_target_io,
    PaneInputLiveness,
};
pub(in crate::handler) use pane_management::{SplitWindowParts, SplitWindowResponseMode};
pub(super) use pane_prompt_input::decode_prompt_input_event;
pub(in crate::handler) use pane_snapshot::PaneSnapshotRevisionRegistry;

use rmux_proto::{PaneTarget, RmuxError, Target};

use super::RequestHandler;
use crate::mouse::AttachedMouseEvent;
use crate::pane_terminals::{session_not_found, HandlerState};

struct AttachedKeyDispatch {
    attach_pid: u32,
    live_identity: Option<super::attach_support::ActiveAttachIdentity>,
    live_session_id: Option<rmux_proto::SessionId>,
    requester_pid: u32,
    current_target: Option<Target>,
    mouse_target: Option<Target>,
    mouse_event: Option<AttachedMouseEvent>,
    key: rmux_core::KeyCode,
    attached_live_input: bool,
}

fn io_other(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

fn active_pane_target_for_session(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
) -> Option<PaneTarget> {
    let session = state.sessions.session(session_name)?;
    let window_index = session.active_window_index();
    let window = session.window_at(window_index)?;
    let pane = window.active_pane()?;
    Some(PaneTarget::with_window(
        session_name.clone(),
        window_index,
        pane.index(),
    ))
}

pub(super) fn resolve_input_target(
    state: &HandlerState,
    explicit: Option<&PaneTarget>,
    attached_session: Option<&rmux_proto::SessionName>,
) -> Result<PaneTarget, RmuxError> {
    if let Some(target) = explicit {
        return Ok(target.clone());
    }
    if let Some(session_name) = attached_session {
        return active_pane_target_for_session(state, session_name)
            .ok_or_else(|| session_not_found(session_name));
    }
    state
        .sessions
        .iter()
        .map(|(session_name, _)| session_name)
        .min_by(|left, right| left.as_str().cmp(right.as_str()))
        .and_then(|session_name| active_pane_target_for_session(state, session_name))
        .ok_or_else(|| RmuxError::Server("no active pane is available".to_owned()))
}
