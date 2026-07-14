use std::sync::atomic::Ordering;

use super::super::prompt_support::ClientPromptState;
use super::super::RequestHandler;
use super::refresh::enqueue_tracked_render_control;
use super::ActiveAttachIdentity;
use crate::pane_io::{AttachControl, AttachTarget};

impl RequestHandler {
    pub(crate) async fn refresh_attached_session_for_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
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
        if !self
            .refresh_attached_client_base_for_session_identity(identity, session_name, session_id)
            .await
        {
            return;
        }
        if self
            .refresh_clock_overlay_for_session_identity(identity, session_name, session_id)
            .await
            .is_err()
        {
            return;
        }
        if self
            .refresh_display_panes_overlay_for_session_identity(identity, session_name, session_id)
            .await
            .is_err()
        {
            return;
        }
        if self
            .refresh_interactive_overlay_for_session_identity(identity, session_name, session_id)
            .await
            .is_err()
        {
            return;
        }
        let _ = self
            .refresh_mode_tree_overlay_for_session_identity(identity, session_name, session_id)
            .await;
    }

    pub(in crate::handler) async fn refresh_attached_client_base_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) -> bool {
        let attach_pid = identity.attach_pid();
        let attached_count = self
            .attached_count_for_session_identity(session_name, session_id)
            .await;
        let target = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    identity.matches_active_session(active, session_name, session_id)
                        && !active.suspended
                        && !active.closing.load(Ordering::SeqCst)
                })
                .map(|active| {
                    (
                        active
                            .prompt
                            .as_ref()
                            .map(ClientPromptState::rendered_prompt),
                        active.terminal_context.clone(),
                        active.client_size,
                        active.mode_tree_state_id,
                        active.mode_tree.is_some(),
                        active.key_table_name.clone(),
                    )
                })
        };
        let Some((
            prompt,
            terminal_context,
            client_size,
            mode_tree_state_id,
            mode_tree_active,
            key_table,
        )) = target
        else {
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
            super::attach_render_target_for_session_with_prompt(
                &state,
                session_name,
                attached_count,
                super::AttachRenderTargetRequest {
                    prompt: prompt.as_ref(),
                    key_table: key_table.as_deref(),
                    terminal_context: &terminal_context,
                    render_size: Some(client_size),
                    socket_path: &self.socket_path(),
                },
            )
            .ok()
        };
        let Some(mut target) = target else {
            return false;
        };
        if mode_tree_active {
            target.persistent_overlay_state_id = Some(mode_tree_state_id);
        }
        self.deliver_base_refresh_for_session_identity(identity, session_name, session_id, target)
            .await
    }

    async fn deliver_base_refresh_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        target: AttachTarget,
    ) -> bool {
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active_session(active, session_name, session_id)
                    && !active.suspended
                    && !active.closing.load(Ordering::SeqCst)
            })
        else {
            return false;
        };
        active.render_generation = active.render_generation.saturating_add(1);
        enqueue_tracked_render_control(active, AttachControl::switch(target))
    }
}
