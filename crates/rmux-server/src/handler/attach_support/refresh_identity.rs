use std::sync::atomic::Ordering;

use super::super::RequestHandler;
use super::refresh::enqueue_tracked_attach_control;
use super::{
    ActiveAttachIdentity, ClientRenderSnapshot, TransientMessageRenderSnapshot,
    TransientMessageRestoreGuard,
};
use crate::pane_io::{AttachControl, AttachTarget};

struct BaseRefreshDelivery {
    pan: crate::client_viewport::ClientPan,
    target: AttachTarget,
    transient_message: Option<TransientMessageRenderSnapshot>,
    rendered_status: Option<Vec<u8>>,
    restore_guard: Option<TransientMessageRestoreGuard>,
}

impl RequestHandler {
    pub(crate) async fn refresh_attached_session_for_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) {
        self.refresh_attached_session_for_session_identity_except(session_name, session_id, None)
            .await;
    }

    pub(in crate::handler) async fn refresh_attached_session_for_session_identity_except(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        excluded_identity: Option<ActiveAttachIdentity>,
    ) {
        let identities = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    &active.session_name == session_name
                        && active.session_id == session_id
                        && !active.suspended
                        && !active.closing.load(Ordering::SeqCst)
                })
                .map(|(attach_pid, active)| active.identity(*attach_pid))
                .filter(|identity| excluded_identity != Some(*identity))
                .collect::<Vec<_>>()
        };
        for identity in identities {
            self.refresh_attached_client_for_session_identity(identity, session_name, session_id)
                .await;
        }
        self.refresh_control_session_for_session_identity(session_name, session_id)
            .await;
    }

    pub(in crate::handler) async fn refresh_attached_client_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) {
        self.refresh_attached_client_for_session_identity_guarded(
            identity,
            session_name,
            session_id,
            None,
        )
        .await;
    }

    pub(in crate::handler) async fn restore_attached_client_after_transient(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        restore_guard: TransientMessageRestoreGuard,
    ) {
        self.refresh_attached_client_for_session_identity_guarded(
            identity,
            session_name,
            session_id,
            Some(restore_guard),
        )
        .await;
    }

    async fn refresh_attached_client_for_session_identity_guarded(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        mut restore_guard: Option<TransientMessageRestoreGuard>,
    ) {
        if !self
            .refresh_attached_client_base_for_session_identity_guarded(
                identity,
                session_name,
                session_id,
                restore_guard,
            )
            .await
        {
            return;
        }
        let clock_emitted = match self
            .refresh_clock_overlay_for_session_identity_guarded(
                identity,
                session_name,
                session_id,
                restore_guard,
            )
            .await
        {
            Ok(emitted) => emitted,
            Err(_) => return,
        };
        if clock_emitted {
            restore_guard = restore_guard.map(TransientMessageRestoreGuard::advanced);
        }
        let display_panes_emitted = match self
            .refresh_display_panes_overlay_for_session_identity_guarded(
                identity,
                session_name,
                session_id,
                restore_guard,
            )
            .await
        {
            Ok(emitted) => emitted,
            Err(_) => return,
        };
        if display_panes_emitted {
            restore_guard = restore_guard.map(TransientMessageRestoreGuard::advanced);
        }
        let interactive_overlay_emitted = match self
            .refresh_interactive_overlay_for_session_identity_guarded(
                identity,
                session_name,
                session_id,
                restore_guard,
            )
            .await
        {
            Ok(emitted) => emitted,
            Err(_) => return,
        };
        if interactive_overlay_emitted {
            restore_guard = restore_guard.map(TransientMessageRestoreGuard::advanced);
        }
        let _ = self
            .refresh_mode_tree_overlay_for_session_identity_guarded(
                identity,
                session_name,
                session_id,
                restore_guard,
            )
            .await;
    }

    pub(in crate::handler) async fn refresh_attached_client_base_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) -> bool {
        self.refresh_attached_client_base_for_session_identity_guarded(
            identity,
            session_name,
            session_id,
            None,
        )
        .await
    }

    async fn refresh_attached_client_base_for_session_identity_guarded(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        restore_guard: Option<TransientMessageRestoreGuard>,
    ) -> bool {
        let attach_pid = identity.attach_pid();
        let attached_count = self
            .attached_count_for_session_identity(session_name, session_id)
            .await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let snapshot = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    identity.matches_active_session(active, session_name, session_id)
                        && !active.suspended
                        && !active.closing.load(Ordering::SeqCst)
                })
                .map(|active| ClientRenderSnapshot::capture(attach_pid, active))
        };
        let Some(snapshot) = snapshot else {
            return false;
        };
        let target = {
            let state = self.state.lock().await;
            if state
                .sessions
                .session(session_name)
                .is_none_or(|session| session.id() != session_id)
            {
                return false;
            }
            let target = match super::attach_render_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                snapshot.render_request(&self.socket_path(), Some(&client_loop_snapshots)),
            ) {
                Ok(target) => target,
                Err(_) => return false,
            };
            let rendered_status = match snapshot
                .transient_message
                .as_ref()
                .map(|message| {
                    super::render_status_message_for_attached_size(
                        &state,
                        session_name,
                        snapshot.client_size,
                        message.status_message(),
                    )
                })
                .transpose()
            {
                Ok(rendered_status) => rendered_status,
                Err(_) => return false,
            };
            Some((target, rendered_status))
        };
        let Some((mut target, rendered_status)) = target else {
            return false;
        };
        snapshot.stamp_persistent_overlay_state(&mut target);
        self.deliver_base_refresh_for_session_identity(
            identity,
            session_name,
            session_id,
            BaseRefreshDelivery {
                pan: snapshot.client.pan,
                target,
                transient_message: snapshot.transient_message,
                rendered_status,
                restore_guard,
            },
        )
        .await
    }

    async fn deliver_base_refresh_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        mut delivery: BaseRefreshDelivery,
    ) -> bool {
        #[cfg(test)]
        if delivery.restore_guard.is_some() {
            super::pause_before_transient_restore_commit(identity.attach_pid()).await;
        }
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active_session(active, session_name, session_id)
                    && active.pan == delivery.pan
                    && !active.suspended
                    && !active.closing.load(Ordering::SeqCst)
                    && delivery
                        .restore_guard
                        .is_none_or(|guard| guard.matches(active))
            })
        else {
            return false;
        };
        active.render_generation = active.render_generation.saturating_add(1);
        active.remember_client_title(delivery.target.client_title.as_ref());
        super::compose_transient_message_refresh(
            active,
            delivery.transient_message.as_ref(),
            delivery.rendered_status,
            &mut delivery.target.render_frame,
        );
        enqueue_tracked_attach_control(active, AttachControl::switch(delivery.target))
    }
}
