use rmux_proto::RmuxError;

use super::super::attach_support::ActiveAttachIdentity;
use super::super::focus_support::prepare_server_focus_effects;
use super::super::RequestHandler;
use super::ClientOverlayState;
use crate::pane_io::{AttachControl, OverlayFrame};

impl RequestHandler {
    pub(in crate::handler) async fn clear_interactive_overlay(
        &self,
        attach_pid: u32,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        self.clear_interactive_overlay_with_identity(
            attach_pid,
            None,
            None,
            None,
            terminate_popup_job,
        )
        .await
    }

    pub(in crate::handler) async fn clear_interactive_overlay_for_identity(
        &self,
        identity: ActiveAttachIdentity,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        self.clear_interactive_overlay_with_identity(
            identity.attach_pid(),
            Some(identity),
            None,
            None,
            terminate_popup_job,
        )
        .await
    }

    pub(super) async fn clear_interactive_overlay_for_session_identity(
        &self,
        identity: ActiveAttachIdentity,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        self.clear_interactive_overlay_with_identity(
            identity.attach_pid(),
            Some(identity),
            Some((session_name, session_id)),
            None,
            terminate_popup_job,
        )
        .await
    }

    pub(in crate::handler) async fn clear_interactive_overlay_for_optional_identity(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        match identity {
            Some(identity) => {
                self.clear_interactive_overlay_for_identity(identity, terminate_popup_job)
                    .await
            }
            None => {
                self.clear_interactive_overlay(attach_pid, terminate_popup_job)
                    .await
            }
        }
    }

    pub(in crate::handler) async fn clear_interactive_overlay_for_optional_identity_and_id(
        &self,
        attach_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        overlay_id: u64,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        self.clear_interactive_overlay_with_identity(
            attach_pid,
            identity,
            None,
            Some(overlay_id),
            terminate_popup_job,
        )
        .await
    }

    async fn clear_interactive_overlay_with_identity(
        &self,
        attach_pid: u32,
        expected_identity: Option<ActiveAttachIdentity>,
        expected_session: Option<(&rmux_proto::SessionName, rmux_proto::SessionId)>,
        expected_overlay_id: Option<u64>,
        terminate_popup_job: bool,
    ) -> Result<(), RmuxError> {
        let (
            control_tx,
            render_generation,
            overlay_generation,
            popup_job,
            transient_restore,
            focus_effects,
        ) = {
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let active = active_attach
                .by_pid
                .get_mut(&attach_pid)
                .filter(|active| {
                    expected_identity.is_none_or(|identity| {
                        identity.matches_active(active)
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    }) && expected_session.is_none_or(|(session_name, session_id)| {
                        &active.session_name == session_name && active.session_id == session_id
                    })
                })
                .ok_or_else(|| RmuxError::Server("attached client disappeared".to_owned()))?;
            if expected_overlay_id.is_some_and(|expected| {
                active.overlay.as_ref().map(ClientOverlayState::id) != Some(expected)
            }) {
                return Ok(());
            }
            let popup_job = match active.overlay.take() {
                Some(ClientOverlayState::Popup(popup)) if terminate_popup_job => popup.job,
                _ => None,
            };
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            // Deferred pane output must be restored even after its producer stops.
            let transient_restore =
                Some((active.identity(attach_pid), active.session_name.clone()));
            let result = (
                active.control_tx.clone(),
                active.render_generation,
                active.overlay_generation,
                popup_job,
                transient_restore,
            );
            let focus_effects = prepare_server_focus_effects(&mut state, &mut active_attach);
            (
                result.0,
                result.1,
                result.2,
                result.3,
                result.4,
                focus_effects,
            )
        };
        let _ = focus_effects.execute(self).await;
        if let Some(job) = popup_job {
            job.terminate();
        }
        let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::persistent(
            Vec::new(),
            render_generation,
            overlay_generation,
        )));
        self.refresh_after_persistent_clear(transient_restore).await;
        Ok(())
    }

    pub(super) async fn refresh_after_persistent_clear(
        &self,
        restore: Option<(ActiveAttachIdentity, rmux_proto::SessionName)>,
    ) {
        let Some((identity, session_name)) = restore else {
            return;
        };
        let _ = self
            .refresh_attached_client_for_identity(
                identity.attach_pid(),
                identity.attach_id(),
                &session_name,
                "overlay clear",
            )
            .await;
    }
}
