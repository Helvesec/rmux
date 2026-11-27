use rmux_proto::{SessionId, SessionName};

use super::{current_control_queue_identity, ControlClientIdentity};
use crate::handler::RequestHandler;
use crate::handler_support::attached_client_required;

impl RequestHandler {
    pub(in crate::handler) async fn validate_control_session_for_queue(
        &self,
        identity: ControlClientIdentity,
    ) -> Result<(), rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        Self::validate_control_queue_identity_locked(
            &state,
            &active_control,
            identity.requester_pid(),
            identity.control_id(),
        )?;
        if active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| active.session_name.is_some() && active.session_id.is_some())
        {
            return Ok(());
        }
        Err(attached_client_required("new-session"))
    }

    pub(in crate::handler) async fn prepare_created_session_control_attach(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> bool {
        if let Some(identity) = current_control_queue_identity(requester_pid) {
            let attached = match self
                .attach_created_control_session_for_queue(identity, session_name, Some(session_id))
                .await
            {
                Ok(attached) => attached,
                Err(_) => return false,
            };
            if !attached {
                return false;
            }
            return true;
        }

        let control_identity = {
            let active_control = self.active_control.lock().await;
            let Some(active) = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(std::sync::atomic::Ordering::SeqCst))
            else {
                return false;
            };
            ControlClientIdentity::new(requester_pid, active.id)
        };
        if self
            .set_created_control_session_for_client_identity(
                requester_pid,
                control_identity.control_id(),
                session_name.clone(),
                session_id,
            )
            .await
            .is_err()
        {
            return false;
        }
        true
    }

    pub(in crate::handler) async fn prepare_existing_session_control_attach(
        &self,
        requester_pid: u32,
        session_name: &SessionName,
        session_id: SessionId,
    ) {
        let control_identity = if let Some(identity) = current_control_queue_identity(requester_pid)
        {
            identity
        } else {
            let active_control = self.active_control.lock().await;
            let Some(active) = active_control
                .by_pid
                .get(&requester_pid)
                .filter(|active| !active.closing.load(std::sync::atomic::Ordering::SeqCst))
            else {
                return;
            };
            ControlClientIdentity::new(requester_pid, active.id)
        };
        let _ = self
            .attach_existing_control_session_for_client_identity(
                requester_pid,
                control_identity.control_id(),
                session_name.clone(),
                session_id,
            )
            .await;
    }
}
