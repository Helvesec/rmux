use super::*;

impl RequestHandler {
    pub(crate) async fn acquire_unrestricted_pane_read_demand(&self) -> PaneOutputReadDemandGuard {
        let state = self.state.lock().await;
        state.pane_output_read_demand.acquire_unrestricted()
    }

    pub(crate) async fn acquire_attached_pane_read_presence(
        &self,
    ) -> PaneOutputAttachedClientReadDemandGuard {
        let state = self.state.lock().await;
        state.pane_output_read_demand.acquire_attached_client()
    }

    pub(crate) async fn control_pane_output_ids(&self) -> HashSet<u32> {
        let state = self.state.lock().await;
        state.pane_output_ids()
    }

    #[cfg(test)]
    pub(crate) async fn control_session_panes(
        &self,
        session_name: &rmux_proto::SessionName,
    ) -> Result<Vec<(u32, PaneOutputSender)>, rmux_proto::RmuxError> {
        let state = self.state.lock().await;
        state.session_pane_outputs(session_name)
    }

    pub(crate) async fn control_session_panes_for_identity(
        &self,
        control_identity: ControlClientIdentity,
        session_name: &rmux_proto::SessionName,
    ) -> Result<Vec<(u32, PaneOutputSender)>, rmux_proto::RmuxError> {
        let session_id = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .get(&control_identity.requester_pid())
                .filter(|active| {
                    active.id == control_identity.control_id()
                        && !active.closing.load(Ordering::SeqCst)
                        && active.session_name.as_ref() == Some(session_name)
                })
                .and_then(|active| active.session_id)
                .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?
        };
        let state = self.state.lock().await;
        let current_name = state
            .sessions
            .session_by_id(session_id)
            .map(|session| session.name())
            .filter(|current_name| *current_name == session_name)
            .ok_or_else(|| rmux_proto::RmuxError::SessionNotFound(session_name.to_string()))?;
        state.session_pane_outputs(current_name)
    }

    #[cfg(test)]
    pub(crate) async fn set_control_subscription_identity_for_test(
        &self,
        control_identity: ControlClientIdentity,
        session_name: rmux_proto::SessionName,
        session_id: SessionId,
    ) {
        let mut active_control = self.active_control.lock().await;
        let active = active_control
            .by_pid
            .get_mut(&control_identity.requester_pid())
            .filter(|active| active.id == control_identity.control_id())
            .expect("test control identity remains registered");
        active.session_name = Some(session_name);
        active.session_id = Some(session_id);
        active.size_sequence = self.next_client_size_sequence();
    }
}
