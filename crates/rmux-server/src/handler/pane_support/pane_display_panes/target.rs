use std::sync::atomic::Ordering;

use rmux_proto::{DisplayPanesRequest, RmuxError};

use super::super::super::{
    attach_support::ActiveAttachIdentity,
    control_support::current_control_queue_identity,
    scripting_support::{queued_display_target_client, QueuedDisplayTargetClient},
    validate_expected_attach_identity, RequestHandler,
};
use super::display_panes_client_error;

impl RequestHandler {
    pub(super) async fn display_panes_client_identity(
        &self,
        requester_pid: u32,
        identity: Option<ActiveAttachIdentity>,
        request: &DisplayPanesRequest,
    ) -> Result<ActiveAttachIdentity, RmuxError> {
        validate_expected_attach_identity(self, requester_pid).await?;
        if let Some(control) = current_control_queue_identity(requester_pid) {
            self.validate_control_queue_session_identity(requester_pid, control.control_id())
                .await?;
        }
        let queued = request
            .target_client
            .as_ref()
            .and_then(|_| queued_display_target_client());
        let identity = if let Some(identity) = identity {
            identity
        } else if let Some(queued) = queued {
            match queued {
                QueuedDisplayTargetClient::Attached(identity) => identity,
                QueuedDisplayTargetClient::ResolutionError(error) => return Err(error),
                QueuedDisplayTargetClient::Missing | QueuedDisplayTargetClient::Control(_) => {
                    return Err(no_current_client());
                }
            }
        } else if request.target_client.is_some() {
            self.resolve_target_attach_client_identity(
                requester_pid,
                request.target_client.as_deref(),
                "display-panes",
            )
            .await
            .map_err(display_panes_client_error)?
        } else {
            let active_attach = self.active_attach.lock().await;
            let mut candidates = active_attach.by_pid.iter().filter(|(_, active)| {
                active.session_name == request.target
                    && !active.suspended
                    && !active.closing.load(Ordering::SeqCst)
                    && !active.control_tx.is_closed()
            });
            if let Some((&pid, active)) = candidates.clone().find(|(pid, _)| **pid == requester_pid)
            {
                active.identity(pid)
            } else {
                let (&pid, active) = candidates.next().ok_or_else(no_current_client)?;
                if candidates.next().is_some() {
                    return Err(no_current_client());
                }
                active.identity(pid)
            }
        };
        let active_attach = self.active_attach.lock().await;
        let active = active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active(active)
                    && !active.suspended
                    && !active.closing.load(Ordering::SeqCst)
                    && !active.control_tx.is_closed()
            })
            .ok_or_else(no_current_client)?;
        if active.session_name != request.target {
            return Err(RmuxError::Message(format!(
                "display-panes target client is not attached to session {}",
                request.target,
            )));
        }
        Ok(identity)
    }

    pub(in crate::handler) async fn capture_parsed_display_panes_client(
        &self,
        requester_pid: u32,
        request: &mut DisplayPanesRequest,
    ) -> Option<QueuedDisplayTargetClient> {
        let target_client = request.target_client.as_deref()?;
        let result = self
            .resolve_target_attach_client_identity(
                requester_pid,
                Some(target_client),
                "display-panes",
            )
            .await;
        Some(match result {
            Ok(identity) => match self.attached_session_name_for_identity(identity).await {
                Ok(session) => {
                    request.target = session;
                    QueuedDisplayTargetClient::Attached(identity)
                }
                Err(error) => QueuedDisplayTargetClient::ResolutionError(error),
            },
            Err(error) => {
                QueuedDisplayTargetClient::ResolutionError(display_panes_client_error(error))
            }
        })
    }
}

fn no_current_client() -> RmuxError {
    RmuxError::Message("no current client".to_owned())
}
