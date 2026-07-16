use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::Arc;

use rmux_core::LifecycleEvent;
#[cfg(test)]
use rmux_os::identity::UserIdentity;
#[cfg(test)]
use tokio::sync::mpsc;

use crate::handler::RequestHandler;
use crate::mouse::ClientMouseState;
#[cfg(test)]
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::{AttachControl, AttachControlSender};
#[cfg(test)]
use crate::server_access::current_owner_uid;

use super::state::{ActiveAttach, ActiveAttachIdentity, AttachRegistration};

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
            None,
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
        .expect("test attach registration session must remain current")
    }

    #[cfg(test)]
    pub(crate) async fn register_attach_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
    ) -> Option<u64> {
        self.register_attach_identity_with_access(
            requester_pid,
            session_name,
            expected_session_id,
            registration,
        )
        .await
        .map(ActiveAttachIdentity::attach_id)
    }

    pub(crate) async fn register_attach_identity_with_access(
        &self,
        requester_pid: u32,
        session_name: rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        registration: AttachRegistration,
    ) -> Option<ActiveAttachIdentity> {
        #[cfg(windows)]
        self.wait_for_windows_deferred_session_panes_ready(&session_name)
            .await;
        let mut replaced_key_table = None;
        let mut replaced_overlay = None;
        let attached_session_name = session_name.clone();
        let state = self.state.lock().await;
        let session = state.sessions.session(&attached_session_name)?;
        let session_id = session.id();
        if expected_session_id.is_some_and(|expected| expected != session_id) {
            return None;
        }
        let active_window_index = Some(session.active_window_index());
        let client_size = registration
            .client_size
            .unwrap_or_else(|| session.window().size());
        let mut active_attach = self.active_attach.lock().await;
        let attach_id = active_attach.next_id;
        active_attach.next_id += 1;
        let size_sequence = active_attach.next_size_sequence;
        active_attach.next_size_sequence = active_attach.next_size_sequence.saturating_add(1);
        let control_backlog = registration.control_backlog;
        let control_tx = AttachControlSender::new(
            registration.control_tx,
            Arc::clone(&control_backlog),
            super::ATTACH_CONTROL_BACKLOG_LIMIT,
            Arc::clone(&registration.closing),
        );
        if let Some(mut previous) = active_attach.by_pid.insert(
            requester_pid,
            ActiveAttach {
                id: attach_id,
                session_name,
                session_id,
                last_session: None,
                last_session_id: None,
                flags: registration.flags,
                control_tx,
                control_backlog,
                render_stream: registration.render_stream,
                render_refresh_pending: false,
                uid: registration.uid,
                user: registration.user,
                can_write: registration.can_write,
                suspended: false,
                closing: registration.closing,
                emit_detached_on_finish: false,
                terminal_context: registration.terminal_context,
                client_size,
                client_pixels: None,
                size_sequence,
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
            active_attach.forget_attached_client_windows(requester_pid);
            replaced_key_table = previous.key_table_name.clone();
            replaced_overlay = previous.overlay.take();
            let _ = previous.control_tx.send(AttachControl::Detach);
            previous.closing.store(true, Ordering::SeqCst);
        }
        if let Some(window_index) = active_window_index {
            active_attach.seed_active_client_for_window(
                requester_pid,
                &attached_session_name,
                window_index,
            );
        }
        drop(active_attach);
        drop(state);
        self.bump_active_attach_epoch();
        super::terminate_overlay_job(replaced_overlay);

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
        Some(ActiveAttachIdentity::new(
            requester_pid,
            attach_id,
            session_id,
        ))
    }

    pub(crate) async fn finish_attach(&self, requester_pid: u32, attach_id: u64) {
        let (removed_session, removed_key_table, removed_overlay, emit_detached) = {
            let mut active_attach = self.active_attach.lock().await;
            if active_attach
                .by_pid
                .get(&requester_pid)
                .is_some_and(|active| active.id == attach_id)
            {
                active_attach
                    .remove_attached_client(requester_pid)
                    .map(|active| {
                        let emit_detached = active.emit_detached_on_finish
                            || !active.closing.load(Ordering::SeqCst);
                        (
                            Some((active.session_name, active.session_id)),
                            active.key_table_name,
                            active.overlay,
                            emit_detached,
                        )
                    })
                    .unwrap_or((None, None, None, false))
            } else {
                (None, None, None, false)
            }
        };
        if removed_session.is_some() {
            self.bump_active_attach_epoch();
        }
        super::terminate_overlay_job(removed_overlay);
        if let Some(table_name) = removed_key_table {
            let mut state = self.state.lock().await;
            state.key_bindings.unref_table(&table_name);
        }
        if let Some((session_name, session_id)) = removed_session {
            if emit_detached {
                self.emit(LifecycleEvent::ClientDetached {
                    session_name: session_name.clone(),
                    client_name: Some(requester_pid.to_string()),
                })
                .await;
            }
            if let Ok(Some(target)) = self.reconcile_attached_session_size(&session_name).await {
                self.emit(LifecycleEvent::WindowResized { target }).await;
            }
            self.destroy_unattached_sessions(vec![(session_name, session_id)])
                .await;
        }
    }

    pub(crate) async fn current_live_attach_input(&self, identity: ActiveAttachIdentity) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .is_some_and(|active| {
                identity.matches_active(active) && !active.closing.load(Ordering::SeqCst)
            })
    }

    pub(crate) async fn active_attach_identity(
        &self,
        attach_pid: u32,
    ) -> Option<ActiveAttachIdentity> {
        self.active_attach
            .lock()
            .await
            .by_pid
            .get(&attach_pid)
            .map(|active| active.identity(attach_pid))
    }

    #[cfg(test)]
    pub(crate) async fn active_attach_identity_for_test(
        &self,
        attach_pid: u32,
    ) -> ActiveAttachIdentity {
        self.active_attach_identity(attach_pid)
            .await
            .expect("test attach must be registered")
    }
}
