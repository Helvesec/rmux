use std::sync::atomic::Ordering;

use rmux_core::PaneId;
use rmux_proto::{PaneTarget, SessionName, WindowId};

use super::super::attach_support::{
    ActiveAttach, ActiveAttachIdentity, ActiveAttachState, ClientFlags,
};
use super::PendingClipboardQuery;
use crate::pane_io::AttachControlSender;
use crate::pane_terminals::HandlerState;

pub(super) fn pane_target_for_stable_identity_in_session(
    state: &HandlerState,
    session_name: &SessionName,
    window_id: WindowId,
    pane_id: PaneId,
) -> Option<PaneTarget> {
    let session = state.sessions.session(session_name)?;
    session.windows().iter().find_map(|(window_index, window)| {
        (window.id() == window_id)
            .then(|| window.panes().iter().find(|pane| pane.id() == pane_id))
            .flatten()
            .map(|pane| PaneTarget::with_window(session_name.clone(), *window_index, pane.index()))
    })
}

pub(super) fn resolve_pending_clipboard_target(
    state: &HandlerState,
    active_attach: &ActiveAttachState,
    identity: ActiveAttachIdentity,
    pending: &PendingClipboardQuery,
) -> Option<PaneTarget> {
    active_attach
        .by_pid
        .get(&identity.attach_pid())
        .filter(|active| {
            identity.matches_active(active)
                && active.session_id == pending.attach.session_id()
                && !active.suspended
                && !active.closing.load(Ordering::SeqCst)
                && !active.render_stream
                && active.can_write
                && !active.flags.contains(ClientFlags::READONLY)
                && !active.clipboard_queries_desynchronized
                && attach_session_contains_window(state, active, pending.pane.window_id)
        })?;
    pending.pane.resolve(state)
}

pub(super) fn most_recent_clipboard_attach(
    state: &HandlerState,
    active_attach: &ActiveAttachState,
    window_id: WindowId,
) -> Option<(ActiveAttachIdentity, AttachControlSender)> {
    active_attach
        .by_pid
        .iter()
        .filter(|(_, active)| {
            !active.suspended
                && !active.closing.load(Ordering::SeqCst)
                && !active.render_stream
                && active.can_write
                && !active.flags.contains(ClientFlags::READONLY)
                && !active.clipboard_queries_desynchronized
                && attach_session_contains_window(state, active, window_id)
        })
        .max_by_key(|(_, active)| (active.last_activity_sequence, active.id))
        .map(|(attach_pid, active)| (active.identity(*attach_pid), active.control_tx.clone()))
}

fn attach_session_contains_window(
    state: &HandlerState,
    active: &ActiveAttach,
    window_id: WindowId,
) -> bool {
    state
        .sessions
        .session_by_id(active.session_id)
        .filter(|session| session.name() == &active.session_name)
        .is_some_and(|session| {
            session
                .windows()
                .values()
                .any(|window| window.id() == window_id)
        })
}
