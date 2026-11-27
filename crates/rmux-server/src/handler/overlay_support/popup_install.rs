use super::super::{
    attach_support::ActiveAttachIdentity,
    focus_support::{prepare_server_focus_effects, FocusEffects},
    mode_tree_support::ModeTreeActionIdentity,
    RequestHandler,
};
use super::{
    popup_job::PopupJob,
    state::{ClientOverlayState, PopupOverlayState},
};
use crate::handler_support::attached_client_required;
use rmux_proto::{RmuxError, SessionName};

pub(super) struct PopupInstallation {
    pub(super) identity: ActiveAttachIdentity,
    session_name: SessionName,
    replaced_job: Option<PopupJob>,
    focus: FocusEffects,
}

impl RequestHandler {
    pub(super) async fn install_popup_state(
        &self,
        popup: &mut PopupOverlayState,
        chooser: Option<ModeTreeActionIdentity>,
    ) -> Result<PopupInstallation, RmuxError> {
        let pid = popup.identity.client_pid();
        let mut state = self.state.lock().await;
        let mut clients = self.active_attach.lock().await;
        let Some(active) = clients.by_pid.get_mut(&pid).filter(|active| {
            popup
                .identity
                .matches(&state, active, &popup.current_target)
                && chooser.is_none_or(|action| {
                    active.id == action.attach_id()
                        && active.mode_tree_state_id == action.state_id()
                        && active.mode_tree.is_some()
                })
        }) else {
            drop(clients);
            drop(state);
            if let Some(job) = &popup.job {
                job.terminate();
            }
            return Err(attached_client_required("display-popup"));
        };
        active.overlay_state_id = active.overlay_state_id.saturating_add(1);
        popup.id = active.overlay_state_id;
        let replaced_job = match active
            .overlay
            .replace(ClientOverlayState::Popup(Box::new(popup.clone())))
        {
            Some(ClientOverlayState::Popup(previous)) => previous.job,
            Some(ClientOverlayState::Menu(_)) | None => None,
        };
        let identity = active.identity(pid);
        let session_name = active.session_name.clone();
        let focus = prepare_server_focus_effects(&mut state, &mut clients);
        Ok(PopupInstallation {
            identity,
            session_name,
            replaced_job,
            focus,
        })
    }

    pub(super) async fn activate_installed_popup(
        &self,
        popup: &PopupOverlayState,
        installed: PopupInstallation,
    ) -> Result<(), RmuxError> {
        let _ = installed.focus.execute(self).await;
        if let Some(job) = installed.replaced_job {
            job.terminate();
        }
        if let Some(job) = popup.job.clone() {
            self.spawn_popup_waiter(installed.identity, popup.id, job.clone());
            if let Err(error) = self.spawn_popup_reader(
                installed.identity,
                popup.id,
                popup.surface.clone(),
                job.clone(),
            ) {
                job.terminate();
                return Err(error);
            }
        }
        self.refresh_interactive_overlay_for_session_identity(
            installed.identity,
            &installed.session_name,
            installed.identity.session_id(),
        )
        .await
    }
}
