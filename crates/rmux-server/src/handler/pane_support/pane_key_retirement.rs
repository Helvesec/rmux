//! A closing press owns an exact active Dead/key incarnation, never a slot.
use rmux_proto::{OptionName, PaneId, PaneTarget, SessionId, WindowId};
use std::sync::atomic::Ordering;

use super::super::{
    attach_support::{ActiveAttachIdentity, ActiveAttachState},
    RequestHandler,
};
use super::pane_removal::PaneRemovalReason;
use crate::client_flags::ClientFlags;
use crate::pane_terminals::HandlerState;

pub(super) struct KeyDismissal {
    attach: ActiveAttachIdentity,
    target: PaneTarget,
    session: SessionId,
    window: WindowId,
    pane: PaneId,
    generation: u64,
}

pub(super) fn is_dead_key(state: &HandlerState, target: &PaneTarget) -> bool {
    let Some(session) = state.sessions.session(target.session_name()) else {
        return false;
    };
    let Some(pane) = session
        .window_at(target.window_index())
        .and_then(|window| window.pane(target.pane_index()))
    else {
        return false;
    };
    state.pane_is_dead(target.session_name(), pane.id())
        && state.options.resolve_for_pane(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
            OptionName::RemainOnExit,
        ) == Some("key")
}

impl KeyDismissal {
    pub(super) fn is_current(
        &self,
        state: &HandlerState,
        attaches: &ActiveAttachState,
        target: &PaneTarget,
    ) -> bool {
        let Some(active) = attaches.by_pid.get(&self.attach.attach_pid()) else {
            return false;
        };
        if !self
            .attach
            .matches_active_session(active, target.session_name(), self.session)
            || active.closing.load(Ordering::SeqCst)
            || !active.can_write
            || active.flags.contains(ClientFlags::READONLY)
            || active.suspended
            || active.prompt.is_some()
            || active.mode_tree.is_some()
            || active.overlay.is_some()
            || active.display_panes.is_some()
            || active.transient_message.is_some()
        {
            return false;
        }
        let Some(session) = state
            .sessions
            .session(target.session_name())
            .filter(|session| session.id() == self.session)
        else {
            return false;
        };
        let window = session.window();
        if state
            .transcript_handle(target)
            .ok()
            .is_some_and(|transcript| {
                let transcript = transcript.lock();
                transcript.copy_mode_state().is_some()
                    || transcript.clock_mode_generation().is_some()
            })
        {
            return false;
        }
        window.id() == self.window
            && window
                .active_pane()
                .is_some_and(|pane| pane.id() == self.pane && pane.index() == target.pane_index())
            && session
                .window_at(target.window_index())
                .is_some_and(|window| window.id() == self.window)
            && state.pane_output_generation_for_target(target, self.pane) == self.generation
            && is_dead_key(state, target)
    }
}

#[cfg(test)]
#[path = "../../handler_pane/key_retirement_tests.rs"]
mod tests;

impl RequestHandler {
    pub(super) async fn capture_key_dismissal(
        &self,
        identity: ActiveAttachIdentity,
        target: &PaneTarget,
        session_id: SessionId,
    ) -> Option<KeyDismissal> {
        let state = self.state.lock().await;
        let attaches = self.active_attach.lock().await;
        if !is_dead_key(&state, target) {
            return None;
        }
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        let pane = window.pane(target.pane_index())?.id();
        let intent = KeyDismissal {
            attach: identity,
            target: target.clone(),
            session: session_id,
            window: window.id(),
            pane,
            generation: state.pane_output_generation_for_target(target, pane),
        };
        intent
            .is_current(&state, &attaches, target)
            .then_some(intent)
    }

    pub(super) async fn dismiss_kept_pane_by_key(
        &self,
        intent: KeyDismissal,
    ) -> std::io::Result<()> {
        // A stale intent consumes this press without redirecting it to a new
        // generation, another active pane, or a recycled index.
        let result = self
            .remove_pane(intent.target.clone(), false, PaneRemovalReason::Key(intent))
            .await;
        if let Some(rmux_proto::Response::Error(error)) = result {
            return Err(super::io_other(error.error));
        }
        Ok(())
    }
}
