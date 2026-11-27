use rmux_proto::RmuxError;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use crate::pane_io::{AttachControl, OverlayFrame};

impl RequestHandler {
    pub(in crate::handler) async fn refresh_interactive_overlay_if_active(
        &self,
        attach_pid: u32,
    ) -> Result<(), RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(attach_pid, None, None)
            .await
            .map(|_| ())
    }

    pub(in crate::handler) async fn refresh_interactive_overlay_for_optional_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
    ) -> Result<(), RmuxError> {
        match identity {
            Some(identity) => {
                let session_name = self.attached_session_name_for_identity(identity).await?;
                self.refresh_interactive_overlay_for_client_identity(
                    attach_pid,
                    identity.attach_id(),
                    &session_name,
                )
                .await
            }
            None => self.refresh_interactive_overlay_if_active(attach_pid).await,
        }
    }

    pub(in crate::handler) async fn refresh_interactive_overlay_for_client_identity(
        &self,
        attach_pid: u32,
        expected_attach_id: u64,
        session_name: &rmux_proto::SessionName,
    ) -> Result<(), RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(
            attach_pid,
            Some((expected_attach_id, session_name, None)),
            None,
        )
        .await
        .map(|_| ())
    }

    pub(in crate::handler) async fn refresh_interactive_overlay_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
    ) -> Result<(), RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(
            identity.attach_pid(),
            Some((identity.attach_id(), session_name, Some(session_id))),
            None,
        )
        .await
        .map(|_| ())
    }

    pub(in crate::handler) async fn refresh_interactive_overlay_for_session_identity_guarded(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        restore_guard: Option<crate::handler::attach_support::TransientMessageRestoreGuard>,
    ) -> Result<bool, RmuxError> {
        self.refresh_interactive_overlay_with_expected_identity(
            identity.attach_pid(),
            Some((identity.attach_id(), session_name, Some(session_id))),
            restore_guard,
        )
        .await
    }

    async fn refresh_interactive_overlay_with_expected_identity(
        &self,
        attach_pid: u32,
        expected_identity: Option<(u64, &rmux_proto::SessionName, Option<rmux_proto::SessionId>)>,
        restore_guard: Option<crate::handler::attach_support::TransientMessageRestoreGuard>,
    ) -> Result<bool, RmuxError> {
        let (overlay, captured_attach_id, captured_session_name, captured_session_id) = {
            let active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get(&attach_pid)
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if expected_identity.is_some_and(
                |(expected_attach_id, expected_session_name, expected_session_id)| {
                    active.id != expected_attach_id
                        || &active.session_name != expected_session_name
                        || expected_session_id.is_some_and(|expected| active.session_id != expected)
                        || active.suspended
                        || active.closing.load(std::sync::atomic::Ordering::SeqCst)
                },
            ) {
                return Err(crate::handler_support::attached_client_required(
                    "refresh-client",
                ));
            }
            if expected_identity.is_some_and(|(_, _, expected_session_id)| {
                expected_session_id.is_some() && active.prompt.is_some()
            }) {
                return Ok(false);
            }
            if restore_guard.is_some_and(|guard| !guard.matches(active)) {
                return Ok(false);
            }
            let Some(overlay) = active.overlay.clone() else {
                return Ok(false);
            };
            (
                overlay,
                active.id,
                active.session_name.clone(),
                active.session_id,
            )
        };

        let mut frame = overlay.render();
        let mut active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get_mut(&attach_pid)
            .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
        if active.id != captured_attach_id
            || active.session_name != captured_session_name
            || active.session_id != captured_session_id
            || active.closing.load(std::sync::atomic::Ordering::SeqCst)
        {
            return expected_identity.map_or(Ok(false), |_| {
                Err(crate::handler_support::attached_client_required(
                    "refresh-client",
                ))
            });
        }
        if expected_identity.is_some_and(
            |(expected_attach_id, expected_session_name, expected_session_id)| {
                active.id != expected_attach_id
                    || &active.session_name != expected_session_name
                    || expected_session_id.is_some_and(|expected| active.session_id != expected)
                    || active.suspended
            },
        ) {
            return Err(crate::handler_support::attached_client_required(
                "refresh-client",
            ));
        }
        if expected_identity.is_some_and(|(_, _, expected_session_id)| {
            expected_session_id.is_some() && active.prompt.is_some()
        }) {
            return Ok(false);
        }
        if restore_guard.is_some_and(|guard| !guard.matches(active)) {
            return Ok(false);
        }
        if active
            .overlay
            .as_ref()
            .map(|current| current.id() != overlay.id())
            .unwrap_or(true)
        {
            return Ok(false);
        }
        crate::handler::attach_support::append_transient_message_frame(active, &mut frame);
        active.overlay_generation = active.overlay_generation.saturating_add(1);
        let delivered = active.control_tx.send(AttachControl::Overlay(
            OverlayFrame::persistent(frame, active.render_generation, active.overlay_generation)
                .with_deferred_pane_updates(),
        ));
        if delivered.is_err() && expected_identity.is_some() {
            return Err(crate::handler_support::attached_client_required(
                "refresh-client",
            ));
        }
        Ok(delivered.is_ok())
    }
}
