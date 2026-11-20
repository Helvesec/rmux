use super::*;

impl RequestHandler {
    pub(super) async fn apply_control_colour_for_identity(
        &self,
        identity: ControlClientIdentity,
        report: crate::control_colours::ColourReport,
    ) -> Result<(), RmuxError> {
        let state = self.state.lock().await;
        let active_control = self.active_control.lock().await;
        if !active_control
            .by_pid
            .get(&identity.requester_pid())
            .is_some_and(|active| {
                active.id == identity.control_id()
                    && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
            })
        {
            return Err(attached_client_required("refresh-client"));
        }
        state.apply_control_colour_report(report);
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../handler_client/refresh/colours/tests.rs"]
mod tests;
