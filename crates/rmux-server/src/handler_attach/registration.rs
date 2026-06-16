use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use rmux_os::identity::UserIdentity;
#[cfg(test)]
use tokio::sync::mpsc;

use crate::handler::RequestHandler;
use crate::mouse::ClientMouseState;
#[cfg(test)]
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
#[cfg(test)]
use crate::server_access::current_owner_uid;

use super::state::{ActiveAttach, AttachRegistration};

impl RequestHandler {
    #[cfg(test)]
    pub(crate) async fn register_attach(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
    ) -> u64 {
        self.register_attach_with_terminal_context(
            requester_pid,
            session_name,
            control_tx,
            OuterTerminalContext::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_terminal_context(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        terminal_context: OuterTerminalContext,
    ) -> u64 {
        self.register_attach_with_closing(
            requester_pid,
            session_name,
            control_tx,
            Arc::new(AtomicBool::new(false)),
            terminal_context,
            super::ClientFlags::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_closing(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        closing: Arc<AtomicBool>,
        terminal_context: OuterTerminalContext,
        flags: super::ClientFlags,
    ) -> u64 {
        self.register_attach_with_access(
            requester_pid,
            session_name,
            AttachRegistration {
                control_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing,
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context,
                flags,
                render_stream: false,
                uid: current_owner_uid(),
                user: UserIdentity::Uid(current_owner_uid()),
                can_write: true,
                client_size: None,
            },
        )
        .await
    }

    pub(crate) async fn register_attach_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        registration: AttachRegistration,
    ) -> u64 {
        let mut replaced_key_table = None;
        let attached_session_name = session_name.clone();
        let client_size = if let Some(client_size) = registration.client_size {
            client_size
        } else {
            let state = self.state.lock().await;
            state
                .sessions
                .session(&attached_session_name)
                .map(|session| session.window().size())
                .unwrap_or(super::super::DEFAULT_SESSION_SIZE)
        };
        let mut active_attach = self.active_attach.lock().await;
        let attach_id = active_attach.next_id;
        active_attach.next_id += 1;
        if let Some(mut previous) = active_attach.by_pid.insert(
            requester_pid,
            ActiveAttach {
                id: attach_id,
                session_name,
                last_session: None,
                flags: registration.flags,
                pan_window: None,
                pan_ox: 0,
                pan_oy: 0,
                control_tx: registration.control_tx,
                control_backlog: registration.control_backlog,
                render_stream: registration.render_stream,
                render_refresh_pending: false,
                uid: registration.uid,
                user: registration.user,
                can_write: registration.can_write,
                suspended: false,
                closing: registration.closing,
                terminal_context: registration.terminal_context,
                client_size,
                client_pixels: None,
                persistent_overlay_epoch: registration.persistent_overlay_epoch,
                render_generation: 0,
                overlay_generation: 0,
                overlay_state_id: 0,
                display_panes_state_id: 0,
                key_table_name: None,
                key_table_set_at: None,
                repeat_deadline: None,
                repeat_active: false,
                last_key: None,
                mouse: ClientMouseState {
                    slider_mpos: -1,
                    ..ClientMouseState::default()
                },
                prompt: None,
                mode_tree_state_id: 0,
                mode_tree: None,
                mode_tree_frame: None,
                overlay: None,
                display_panes: None,
            },
        ) {
            replaced_key_table = previous.key_table_name.clone();
            super::terminate_overlay_job(previous.overlay.take());
            previous.closing.store(true, Ordering::SeqCst);
            let _ = previous.control_tx.send(AttachControl::Detach);
        }
        drop(active_attach);

        if let Some(table_name) = replaced_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }

        let mut state = self.state.lock().await;
        if let Some(session) = state.sessions.session_mut(&attached_session_name) {
            session.touch_attached();
        }
        drop(state);
        self.refresh_clock_overlays_for_session(&attached_session_name)
            .await;
        attach_id
    }

    pub(crate) async fn finish_attach(&self, requester_pid: u32, attach_id: u64) {
        let (removed_session, removed_key_table, removed_overlay) = {
            let mut active_attach = self.active_attach.lock().await;
            if active_attach
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == attach_id)
            {
                active_attach
                    .by_pid
                    .remove(&requester_pid)
                    .map(|active| {
                        (
                            Some(active.session_name),
                            active.key_table_name,
                            active.overlay,
                        )
                    })
                    .unwrap_or((None, None, None))
            } else {
                (None, None, None)
            }
        };
        super::terminate_overlay_job(removed_overlay);
        if let Some(table_name) = removed_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }
        if let Some(session_name) = removed_session {
            self.destroy_unattached_sessions(vec![session_name]).await;
        }
    }
}
