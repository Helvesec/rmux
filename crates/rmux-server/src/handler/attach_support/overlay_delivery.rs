use super::{
    attach_render_session, renderer, session_not_found, transient_message, ActiveAttachIdentity,
    AttachControl, Duration, HandlerState, OverlayFrame, RequestHandler, SessionId, SessionName,
    TerminalSize, TransientMessageInputPolicy,
};
use tokio::time::sleep;

struct AttachedOverlayIdentityExpectation {
    identity: Option<ActiveAttachIdentity>,
    session_name: Option<SessionName>,
    session_id: Option<SessionId>,
    client_size: Option<TerminalSize>,
}

struct AttachedOverlayDelivery {
    status_message: String,
    status_frame: Vec<u8>,
    frame: Vec<u8>,
    clear_frame: Vec<u8>,
    duration: Option<Duration>,
    input_policy: TransientMessageInputPolicy,
}

impl RequestHandler {
    pub(in crate::handler) async fn send_attached_overlay(
        &self,
        session_name: &rmux_proto::SessionName,
        status_message: String,
        duration: Option<Duration>,
        input_policy: TransientMessageInputPolicy,
    ) -> bool {
        let identities = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| active.session_name == *session_name && !active.suspended)
                .map(|(pid, active)| active.identity(*pid))
                .collect::<Vec<_>>()
        };
        let mut delivered = false;
        for identity in identities {
            delivered |= self
                .send_attached_overlay_to_client_identity(
                    identity,
                    Some(identity.session_id()),
                    status_message.clone(),
                    duration,
                    input_policy,
                )
                .await;
        }
        delivered
    }

    pub(in crate::handler) async fn send_attached_overlay_to_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: SessionId,
        status_message: String,
        duration: Option<Duration>,
        input_policy: TransientMessageInputPolicy,
    ) -> bool {
        let identities = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    active.session_name == *session_name
                        && active.session_id == session_id
                        && !active.suspended
                })
                .map(|(pid, active)| active.identity(*pid))
                .collect::<Vec<_>>()
        };
        let mut delivered = false;
        for identity in identities {
            delivered |= self
                .send_attached_overlay_to_client_identity(
                    identity,
                    Some(session_id),
                    status_message.clone(),
                    duration,
                    input_policy,
                )
                .await;
        }
        delivered
    }

    pub(in crate::handler) async fn send_attached_overlay_to_client_identity(
        &self,
        identity: ActiveAttachIdentity,
        expected_session_id: Option<SessionId>,
        status_message: String,
        duration: Option<Duration>,
        input_policy: TransientMessageInputPolicy,
    ) -> bool {
        for _ in 0..3 {
            let client = {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&identity.attach_pid())
                    .filter(|active| {
                        identity.matches_active(active)
                            && expected_session_id
                                .is_none_or(|session_id| active.session_id == session_id)
                            && !active.suspended
                    })
                    .map(|active| (active.session_name.clone(), active.client_size))
            };
            let Some((session_name, client_size)) = client else {
                return false;
            };
            let rendered = {
                let state = self.state.lock().await;
                if expected_session_id.is_some_and(|expected| {
                    state
                        .sessions
                        .session(&session_name)
                        .is_none_or(|session| session.id() != expected)
                }) {
                    return false;
                }
                render_status_overlay_for_attached_size(
                    &state,
                    &session_name,
                    client_size,
                    &status_message,
                )
                .ok()
            };
            let Some(rendered) = rendered else {
                return false;
            };
            let refresh_session_name = session_name.clone();
            if self
                .send_attached_overlay_to_client_guarded(
                    identity.attach_pid(),
                    AttachedOverlayIdentityExpectation {
                        identity: Some(identity),
                        session_name: Some(session_name),
                        session_id: expected_session_id,
                        client_size: Some(client_size),
                    },
                    AttachedOverlayDelivery {
                        status_message: status_message.clone(),
                        status_frame: rendered.status_frame,
                        frame: rendered.overlay_frame,
                        clear_frame: rendered.clear_frame,
                        duration,
                        input_policy,
                    },
                )
                .await
            {
                self.refresh_persistent_surfaces_after_transient(
                    identity,
                    &refresh_session_name,
                    expected_session_id.unwrap_or(identity.session_id()),
                )
                .await;
                return true;
            }
        }
        false
    }

    async fn refresh_persistent_surfaces_after_transient(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &SessionName,
        session_id: SessionId,
    ) {
        let _ = self
            .refresh_clock_overlay_for_client_identity(
                identity.attach_pid(),
                identity.attach_id(),
                session_name,
            )
            .await;
        let _ = self
            .refresh_display_panes_overlay_for_client_identity(
                identity.attach_pid(),
                identity.attach_id(),
                session_name,
            )
            .await;
        let _ = self
            .refresh_interactive_overlay_for_session_identity(identity, session_name, session_id)
            .await;
        let _ = self
            .refresh_mode_tree_overlay_for_client_identity(
                identity.attach_pid(),
                identity.attach_id(),
                session_name,
            )
            .await;
    }

    async fn send_attached_overlay_to_client_guarded(
        &self,
        attach_pid: u32,
        expected: AttachedOverlayIdentityExpectation,
        delivery: AttachedOverlayDelivery,
    ) -> bool {
        let AttachedOverlayDelivery {
            status_message,
            status_frame,
            frame,
            clear_frame,
            duration,
            input_policy,
        } = delivery;
        let handler = self.clone();
        let mut state = self.state.lock().await;
        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return false;
        };
        if active.suspended
            || expected
                .identity
                .is_some_and(|identity| !identity.matches_active(active))
            || expected
                .session_name
                .as_ref()
                .is_some_and(|session_name| active.session_name != *session_name)
            || expected
                .session_id
                .is_some_and(|session_id| active.session_id != session_id)
            || expected
                .client_size
                .is_some_and(|client_size| active.client_size != client_size)
        {
            return false;
        }

        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let render_generation = active.render_generation;
        let overlay_generation = active.overlay_generation;
        let identity = transient_message::active_attach_identity(attach_pid, active);
        let expiry_cancelled = transient_message::arm_transient_message(
            active,
            status_message,
            status_frame,
            clear_frame.clone(),
            input_policy,
            duration.is_some(),
        );
        if active
            .control_tx
            .send(AttachControl::Overlay(OverlayFrame::new(
                frame,
                render_generation,
                overlay_generation,
            )))
            .is_err()
        {
            active_attach.remove_attached_client(attach_pid);
            self.bump_active_attach_epoch();
            let focus_effects = super::super::focus_support::prepare_server_focus_effects(
                &mut state,
                &mut active_attach,
            );
            drop(active_attach);
            drop(state);
            let _ = focus_effects.execute(self).await;
            return false;
        }
        drop(active_attach);
        drop(state);

        if let (Some(duration), Some(expiry_cancelled)) = (duration, expiry_cancelled) {
            tokio::spawn(async move {
                tokio::select! {
                    _ = sleep(duration) => {
                        handler
                            .expire_transient_message_for_identity(identity, overlay_generation)
                            .await;
                    }
                    _ = expiry_cancelled => {}
                }
            });
        }
        true
    }
}

pub(in crate::handler) fn render_status_message_for_attached_size(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    render_size: TerminalSize,
    message: &str,
) -> Result<Vec<u8>, rmux_proto::RmuxError> {
    render_status_overlay_for_attached_size(state, session_name, render_size, message)
        .map(|frames| frames.status_frame)
}

struct StatusOverlayFrames {
    overlay_frame: Vec<u8>,
    clear_frame: Vec<u8>,
    status_frame: Vec<u8>,
}

fn render_status_overlay_for_attached_size(
    state: &HandlerState,
    session_name: &rmux_proto::SessionName,
    render_size: TerminalSize,
    message: &str,
) -> Result<StatusOverlayFrames, rmux_proto::RmuxError> {
    let canonical_session = state
        .sessions
        .session(session_name)
        .ok_or_else(|| session_not_found(session_name))?;
    let session = attach_render_session(canonical_session, Some(render_size), None, None, None)?;
    let clear_frame = renderer::render_display_panes_clear(session.view(), &state.options, state);
    let status_frame = renderer::render_status_message(session.view(), &state.options, message);
    let mut overlay_frame = clear_frame.clone();
    overlay_frame.extend_from_slice(&status_frame);
    Ok(StatusOverlayFrames {
        overlay_frame,
        clear_frame,
        status_frame,
    })
}
