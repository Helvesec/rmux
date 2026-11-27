use std::time::Duration;

use rmux_proto::RmuxError;

use super::super::super::{
    attach_support::{
        append_transient_message_frame, attach_target_for_session, ActiveAttachIdentity,
        AttachSessionRenderOptions,
    },
    focus_support::prepare_server_focus_effects,
    RequestHandler,
};
use crate::handler_support::attached_client_required;
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::session_not_found;
use crate::renderer;

impl RequestHandler {
    pub(in crate::handler) async fn refresh_display_panes_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_display_panes_overlay_with_expected_identity(
            attach_pid,
            expected_attach_id,
            session_name,
            None,
            None,
        )
        .await
        .map(|_| ())
    }

    pub(in crate::handler) async fn refresh_display_panes_overlay_for_session_identity_guarded(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        restore_guard: Option<crate::handler::attach_support::TransientMessageRestoreGuard>,
    ) -> Result<bool, RmuxError> {
        self.refresh_display_panes_overlay_with_expected_identity(
            identity.attach_pid(),
            identity.attach_id(),
            session_name,
            Some(session_id),
            restore_guard,
        )
        .await
    }

    async fn refresh_display_panes_overlay_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
        expected_session_id: Option<rmux_proto::SessionId>,
        restore_guard: Option<crate::handler::attach_support::TransientMessageRestoreGuard>,
    ) -> Result<bool, RmuxError> {
        let expected_state_id = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| {
                    active.id == expected_attach_id
                        && &active.session_name == session_name
                        && expected_session_id.is_none_or(|expected| active.session_id == expected)
                        && (expected_session_id.is_none() || active.prompt.is_none())
                        && !active.suspended
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                        && restore_guard.is_none_or(|guard| guard.matches(active))
                })
                .ok_or_else(|| attached_client_required("refresh-client"))?;
            let Some(display_panes) = active.display_panes.as_ref() else {
                return Ok(false);
            };
            display_panes.id
        };
        let (overlay_frame, clear_frame) = {
            let state = self.state.lock().await;
            let session = state
                .sessions
                .session(session_name)
                .ok_or_else(|| session_not_found(session_name))?;
            if expected_session_id.is_some_and(|expected| session.id() != expected) {
                return Err(attached_client_required("refresh-client"));
            }
            (
                renderer::render_display_panes_overlay(session, &state.options),
                renderer::render_display_panes_clear(session, &state.options, &state),
            )
        };

        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .filter(|active| {
                active.id == expected_attach_id
                    && &active.session_name == session_name
                    && expected_session_id.is_none_or(|expected| active.session_id == expected)
                    && (expected_session_id.is_none() || active.prompt.is_none())
                    && !active.suspended
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    && restore_guard.is_none_or(|guard| guard.matches(active))
            })
            .ok_or_else(|| attached_client_required("refresh-client"))?;
        if active
            .display_panes
            .as_ref()
            .is_none_or(|display_panes| display_panes.id != expected_state_id)
        {
            return Ok(false);
        }

        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let mut frame =
            if active.mode_tree.is_some() || active.overlay.is_some() || active.render_stream {
                Vec::new()
            } else {
                clear_frame
            };
        frame.extend_from_slice(&overlay_frame);
        append_transient_message_frame(active, &mut frame);
        active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                frame,
                active.render_generation,
                active.overlay_generation,
            )))
            .map(|_| true)
            .map_err(|_| attached_client_required("refresh-client"))
    }

    pub(super) fn schedule_display_panes_timeout(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
        duration: Duration,
    ) {
        let handler = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(duration).await;
            let _ = handler
                .clear_display_panes_state_for_identity(identity, Some(state_id), true)
                .await;
        });
    }

    pub(super) async fn clear_display_panes_state(
        &self,
        attach_pid: u32,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_inner(attach_pid, None, expected_state_id, send_clear)
            .await
    }

    pub(super) async fn clear_display_panes_state_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_inner(
            identity.attach_pid(),
            Some(identity),
            expected_state_id,
            send_clear,
        )
        .await
    }

    #[cfg(test)]
    pub(in crate::handler) async fn expire_display_panes_for_identity_for_test(
        &self,
        identity: ActiveAttachIdentity,
        state_id: u64,
    ) -> Result<bool, RmuxError> {
        self.clear_display_panes_state_for_identity(identity, Some(state_id), true)
            .await
    }

    async fn clear_display_panes_state_inner(
        &self,
        attach_pid: u32,
        expected_identity: Option<ActiveAttachIdentity>,
        expected_state_id: Option<u64>,
        send_clear: bool,
    ) -> Result<bool, RmuxError> {
        let (captured_identity, captured_state_id, fallback_clear_frame) = {
            let active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get(&attach_pid) else {
                return Ok(false);
            };
            if expected_identity.is_some_and(|identity| !identity.matches_active(active)) {
                return Ok(false);
            }
            let Some(state) = active.display_panes.as_ref() else {
                return Ok(false);
            };
            if expected_state_id.is_some_and(|id| id != state.id) {
                return Ok(false);
            }
            (
                active.identity(attach_pid),
                state.id,
                state.clear_frame.clone(),
            )
        };
        let clear_frame = if send_clear {
            self.render_attached_display_panes_clear_frame(attach_pid)
                .await
                .unwrap_or(fallback_clear_frame)
        } else {
            Vec::new()
        };

        let clear = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(false);
            };
            if !captured_identity.matches_active(active) {
                return Ok(false);
            }
            let matches_state = active
                .display_panes
                .as_ref()
                .is_some_and(|state| state.id == captured_state_id);
            if !matches_state {
                return Ok(false);
            }
            let _state = active
                .display_panes
                .take()
                .expect("display-panes state exists when matched");
            let overlay = if send_clear {
                active.overlay_generation = active.overlay_generation.saturating_add(1);
                let frame = if let Some(overlay) = active.overlay.as_ref() {
                    let mut frame = overlay.render();
                    append_transient_message_frame(active, &mut frame);
                    OverlayFrame::persistent(
                        frame,
                        active.render_generation,
                        active.overlay_generation,
                    )
                    .with_deferred_pane_updates()
                } else if let (Some(_mode_tree), Some(mode_tree_frame)) =
                    (active.mode_tree.as_ref(), active.mode_tree_frame.as_ref())
                {
                    let mut frame = clear_frame;
                    frame.extend_from_slice(mode_tree_frame);
                    append_transient_message_frame(active, &mut frame);
                    OverlayFrame::persistent_with_state(
                        frame,
                        active.render_generation,
                        active.overlay_generation,
                        active.mode_tree_state_id,
                    )
                } else {
                    let mut clear_frame = clear_frame;
                    append_transient_message_frame(active, &mut clear_frame);
                    OverlayFrame::new(
                        clear_frame,
                        active.render_generation,
                        active.overlay_generation,
                    )
                };
                Some((active.control_tx.clone(), frame))
            } else {
                None
            };
            let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
            Some((overlay, focus_effects))
        };

        let cleared_exists = clear.is_some();
        if let Some((overlay, focus_effects)) = clear {
            let _ = focus_effects.execute(self).await;
            if let Some((control_tx, frame)) = overlay {
                let _ = control_tx.send(AttachControl::Overlay(frame));
            }
        }

        Ok(cleared_exists)
    }

    pub(super) async fn render_attached_display_panes_clear_frame(
        &self,
        attach_pid: u32,
    ) -> Option<Vec<u8>> {
        let (session_name, terminal_context, client_title, client) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach.by_pid.get(&attach_pid)?;
            (
                active.session_name.clone(),
                active.terminal_context.clone(),
                active.client_title.clone(),
                crate::handler::client_runtime_support::ListClientSnapshot::from_attached_client(
                    attach_pid, active,
                ),
            )
        };
        let attached_count = self.attached_count(&session_name).await;
        let client_loop_snapshots = self.client_loop_snapshots().await;
        let state = self.state.lock().await;
        let session = state.sessions.session(&session_name)?;
        let target = attach_target_for_session(
            &state,
            &session_name,
            AttachSessionRenderOptions {
                attached_count,
                terminal_context: &terminal_context,
                // Dismissing the overlay replays the base frame; the outer terminal
                // still shows this client's title, so it must not be rewritten.
                previous_title: Some(&client_title),
                client: Some(&client),
                socket_path: &self.socket_path(),
                client_loop_snapshots: Some(&client_loop_snapshots),
            },
        )
        .ok()?;
        Some(renderer::render_display_panes_clear_with_base(
            session,
            &state.options,
            &target.render_frame,
        ))
    }
}
