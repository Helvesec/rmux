use super::{
    current_control_queue_identity, ControlClientIdentity, ParsedCommands, RequestHandler,
    RmuxError, ScopeSelector, ENVIRON_HIDDEN,
};

impl RequestHandler {
    pub(super) async fn apply_parse_time_assignments(
        &self,
        requester_pid: u32,
        commands: &ParsedCommands,
        expected_control_id: Option<u64>,
    ) -> Result<(), RmuxError> {
        if commands.assignments().is_empty() {
            return Ok(());
        }

        let expected_control_id = expected_control_id.or_else(|| {
            current_control_queue_identity(requester_pid).map(ControlClientIdentity::control_id)
        });
        let (mut state, _active_control) = if let Some(control_id) = expected_control_id {
            let state = self.state.lock().await;
            let active_control = self.active_control.lock().await;
            RequestHandler::validate_control_queue_identity_locked(
                &state,
                &active_control,
                requester_pid,
                control_id,
            )?;
            if !active_control
                .by_pid
                .get(&requester_pid)
                .expect("validated control client remains registered while locked")
                .can_write
            {
                return Err(RmuxError::Server("client is read-only".to_owned()));
            }
            (state, Some(active_control))
        } else {
            if !self.requester_can_write(requester_pid).await {
                return Err(RmuxError::Server("client is read-only".to_owned()));
            }
            (self.state.lock().await, None)
        };
        for assignment in commands.assignments() {
            state.environment.set_with_flags(
                ScopeSelector::Global,
                assignment.name().to_owned(),
                assignment.value().to_owned(),
                if assignment.hidden() {
                    ENVIRON_HIDDEN
                } else {
                    0
                },
            );
        }
        Ok(())
    }
}
