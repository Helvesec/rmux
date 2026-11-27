use std::io;

mod pane_attached_input;
mod pane_attached_key_dispatch;
mod pane_broadcast;
mod pane_by_id;
mod pane_by_id_selection;
#[cfg(windows)]
mod pane_deferred_wait;
mod pane_display_panes;
mod pane_exit_probe;
mod pane_inspection;
mod pane_io_encoding;
mod pane_key_bindings;
mod pane_key_retirement;
mod pane_kill_effects;
mod pane_layout;
mod pane_lifecycle;
mod pane_management;
mod pane_process;
mod pane_prompt_input;
mod pane_removal;
mod pane_selection;
mod pane_send_key_formats;
mod window_snapshot;
use pane_send_key_formats::expand_send_key_tokens;

mod pane_send_keys;
mod pane_snapshot;
mod pane_split_effects;
mod pane_timer_mutations;
mod pane_transfer;
#[cfg(windows)]
mod pane_windows_console_sequence;

#[cfg(test)]
pub(in crate::handler) use pane_attached_input::bracketed_paste::take_delimiter_search_bytes_for_test;
pub(in crate::handler) use pane_attached_input::bracketed_paste::{
    strip_bracketed_paste_markers, strip_bracketed_paste_markers_after_append,
    take_incomplete_bracketed_paste_segment,
};
pub(super) use pane_attached_input::retain_partial_attached_escape_input;
#[cfg(test)]
pub(in crate::handler) use pane_attached_input::AttachedMouseTimerPause;
pub(in crate::handler) use pane_attached_input::{
    decode_attached_terminal_control_after_append, decode_pane_bound_terminal_string,
    PaneBoundTerminalStringDecode, TerminalResponseDecode,
};
pub(in crate::handler) use pane_attached_input::{AssumedPasteState, HeldKeys};
#[cfg(test)]
pub(in crate::handler) use pane_attached_input::{LiveInputWork, LiveInputWorkProbe};
pub(super) use pane_by_id::resolve_pane_target_ref;
#[cfg(windows)]
pub(in crate::handler) use pane_deferred_wait::format_references_pane_pid;
pub(super) use pane_inspection::{
    attached_status_message_for_error, command_output_from_lines, display_time,
};
use pane_io_encoding::{
    encode_key_for_target, encode_mouse_for_target, encode_tokens_for_target,
    pane_id_for_input_target, prepare_synchronized_pane_input_writes, write_bytes_to_target,
    write_bytes_to_targets,
};
pub(super) use pane_io_encoding::{
    prepare_committed_focus_input_write, prepare_pane_bracketed_paste_write,
    prepare_pane_input_write, write_attached_bytes_to_target_io, write_bytes_to_target_io,
    PaneInputLiveness, PaneInputWrite,
};
#[cfg(windows)]
pub(super) use pane_io_encoding::{
    windows_paste_sink, WindowsPasteSink, LEGACY_CONPTY_NON_UTF8_BRACKETED_PASTE_ERROR,
};
pub(in crate::handler) use pane_management::{
    append_floating_stdin_output, PaneStdinPolicy, SplitWindowParts, SplitWindowResponseMode,
};
pub(super) use pane_prompt_input::decode_prompt_input_event;
pub(in crate::handler) use pane_snapshot::{
    collect_cells, compute_snapshot_fingerprint, cursor_coord_to_u16, CellCollectionBudget,
    PaneSnapshotRevisionRegistry,
};

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
        .map(|session| (session.name(), session))
        .map(|(session_name, _)| session_name)
        .min_by(|left, right| left.as_str().cmp(right.as_str()))
        .and_then(|session_name| active_pane_target_for_session(state, session_name))
        .ok_or_else(|| RmuxError::Server("no active pane is available".to_owned()))
}
