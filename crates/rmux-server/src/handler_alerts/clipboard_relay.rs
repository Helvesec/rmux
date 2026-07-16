//! Immediate, identity-scoped relay of application clipboard writes.

use std::collections::HashSet;
use std::sync::atomic::Ordering;

use rmux_proto::OptionName;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use crate::outer_terminal::OuterTerminal;
use crate::pane_io::{AttachControl, PaneAlertEvent};

impl RequestHandler {
    pub(super) fn try_relay_visible_inactive_pane_clipboard(
        &self,
        event: &PaneAlertEvent,
    ) -> Vec<ActiveAttachIdentity> {
        if event.clipboard_writes.is_empty() {
            return Vec::new();
        }

        // Pane readers invoke this while publishing the OSC 52 event. Never
        // wait on handler locks from that path: if a target mutation is in
        // flight, fail closed instead of routing with a stale visibility
        // snapshot. When both locks are immediately available, the enqueue is
        // serialized with attach switches in their established state ->
        // active_attach order, so a following switch is ordered after the
        // clipboard write in the same FIFO.
        let Ok(state) = self.state.try_lock() else {
            return Vec::new();
        };
        if !matches!(
            state.options.resolve(None, OptionName::SetClipboard),
            Some("on")
        ) {
            return Vec::new();
        }
        let Some(runtime_session_name) = state.resolve_pane_event_runtime_session(
            &event.session_name,
            event.pane_id,
            event.generation,
        ) else {
            return Vec::new();
        };
        let Some(pane_target) =
            state.pane_target_for_runtime_pane(&runtime_session_name, event.pane_id)
        else {
            return Vec::new();
        };
        let Some(source_window_id) = state
            .sessions
            .session(pane_target.session_name())
            .and_then(|session| session.window_at(pane_target.window_index()))
            .map(rmux_core::Window::id)
        else {
            return Vec::new();
        };
        let visible_inactive_sessions = state
            .window_linked_current_sessions_list(
                pane_target.session_name(),
                pane_target.window_index(),
            )
            .into_iter()
            .filter_map(|session_name| {
                let session = state.sessions.session(&session_name)?;
                let window = session.window_at(session.active_window_index())?;
                (window.id() == source_window_id && session.active_pane_id() != Some(event.pane_id))
                    .then_some(session.id())
            })
            .collect::<HashSet<_>>();
        if visible_inactive_sessions.is_empty() {
            return Vec::new();
        }

        let Ok(mut active_attach) = self.active_attach.try_lock() else {
            return Vec::new();
        };
        let mut disconnected = Vec::new();
        for (attach_pid, active) in &mut active_attach.by_pid {
            if active.suspended
                || active.closing.load(Ordering::SeqCst)
                || !visible_inactive_sessions.contains(&active.session_id)
            {
                continue;
            }
            let Some(session) = state.sessions.session_by_id(active.session_id) else {
                continue;
            };
            if session.name() != &active.session_name {
                continue;
            }
            let outer_terminal = OuterTerminal::resolve_for_session(
                &state.options,
                Some(session.name()),
                active.terminal_context.clone(),
            );
            let mut payload = Vec::new();
            for content in &event.clipboard_writes {
                if let Some(encoded) = outer_terminal.encode_clipboard_set(content) {
                    payload.extend(encoded);
                }
            }
            if payload.is_empty() {
                continue;
            }
            let control = AttachControl::ClipboardWrite {
                bytes: payload,
                reservation: None,
            };
            if active.control_tx.send(control).is_err() {
                active.closing.store(true, Ordering::SeqCst);
                // Leave the registration in place until the common attach
                // cleanup owns it. That path terminates overlays, drops key
                // table references, reconciles size, and applies
                // destroy-unattached exactly once.
                active.emit_detached_on_finish = true;
                disconnected.push(active.identity(*attach_pid));
            }
        }
        if !disconnected.is_empty() {
            self.bump_active_attach_epoch();
        }
        disconnected
    }
}
