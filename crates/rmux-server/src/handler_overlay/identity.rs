use std::sync::Arc;

use rmux_proto::{RmuxError, Target};

use super::super::attach_support::{ActiveAttach, ActiveAttachIdentity};
use super::super::lifecycle_support::LifecycleTargetLease;
use super::super::scripting_support::QueueExecutionContext;
use super::super::{RequestHandler, StableTargetIdentity};
use super::state::ClientOverlayState;
use crate::pane_io::{AttachControl, OverlayFrame};
use crate::pane_terminals::HandlerState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct OverlayIdentity {
    client: ActiveAttachIdentity,
    target: StableTargetIdentity,
    target_lease: Option<Arc<LifecycleTargetLease>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlayActionStatus {
    Current,
    Retired,
    Missing,
}

impl OverlayActionStatus {
    pub(super) fn is_current(self) -> bool {
        self == Self::Current
    }

    pub(super) fn was_retired(self) -> bool {
        self == Self::Retired
    }
}

impl OverlayIdentity {
    pub(super) fn capture(
        state: &mut HandlerState,
        client: ActiveAttachIdentity,
        target: Target,
    ) -> Result<Self, RmuxError> {
        let target_lease = match &target {
            Target::Pane(target) => state.capture_retained_pane_lifecycle_target(target),
            Target::Window(target) => state.capture_retained_window_lifecycle_target(target),
            Target::Session(_) => None,
        };
        Ok(Self {
            client,
            target: StableTargetIdentity::capture(state, target)?,
            target_lease,
        })
    }

    pub(super) fn command_context(
        &self,
        context: QueueExecutionContext,
        target: Target,
    ) -> QueueExecutionContext {
        context
            .with_current_target(Some(target))
            .with_retained_lifecycle_target(self.target_lease.clone())
            .with_pinned_current_target_identity(Some(self.target.clone()))
    }

    pub(super) fn matches(
        &self,
        state: &HandlerState,
        active: &ActiveAttach,
        target: &Target,
    ) -> bool {
        self.client.matches_active(active)
            && active.session_id == self.client.session_id()
            && !active.suspended
            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            && state
                .sessions
                .session(&active.session_name)
                .is_some_and(|session| session.id() == self.client.session_id())
            && self.target.matches_target(state, target)
    }

    pub(super) fn rename_session(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        self.target.rename_session(old_name, new_name);
    }
}

impl RequestHandler {
    pub(super) async fn overlay_action_is_current(
        &self,
        attach_pid: u32,
        input_identity: Option<ActiveAttachIdentity>,
    ) -> Result<OverlayActionStatus, RmuxError> {
        let retired = {
            let state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
                return Ok(OverlayActionStatus::Missing);
            };
            if input_identity.is_some_and(|identity| !identity.matches_active(active)) {
                return Ok(OverlayActionStatus::Missing);
            }
            let Some(overlay) = active.overlay.as_ref() else {
                return Ok(OverlayActionStatus::Missing);
            };
            if overlay
                .identity()
                .matches(&state, active, overlay.current_target())
            {
                return Ok(OverlayActionStatus::Current);
            }

            let popup_job = match active.overlay.take() {
                Some(ClientOverlayState::Popup(popup)) => popup.job,
                Some(ClientOverlayState::Menu(_)) | None => None,
            };
            active.overlay_generation = active.overlay_generation.saturating_add(1);
            Some((
                active.control_tx.clone(),
                active.render_generation,
                active.overlay_generation,
                popup_job,
            ))
        };

        if let Some((control_tx, render_generation, overlay_generation, popup_job)) = retired {
            if let Some(job) = popup_job {
                job.terminate();
            }
            let _ = control_tx.send(AttachControl::Overlay(OverlayFrame::persistent(
                Vec::new(),
                render_generation,
                overlay_generation,
            )));
        }
        Ok(OverlayActionStatus::Retired)
    }
}
