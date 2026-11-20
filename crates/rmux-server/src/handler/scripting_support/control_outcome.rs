use super::{
    control_client_name, queue_action_from_response, ControlClientIdentity, QueueCommandAction,
    Request, RequestHandler, Response, RmuxError,
};

impl RequestHandler {
    pub(super) async fn control_queue_action_from_outcome(
        &self,
        requester_pid: u32,
        expected_control_id: u64,
        request: Request,
        outcome: crate::pane_io::HandleOutcome,
    ) -> Result<QueueCommandAction, RmuxError> {
        let control_identity = ControlClientIdentity::new(requester_pid, expected_control_id);
        if let Some(attach) = outcome.attach {
            if matches!(
                request,
                Request::AttachSession(_)
                    | Request::AttachSessionExt(_)
                    | Request::AttachSessionExt2(_)
                    | Request::AttachSessionExt3(_)
            ) {
                let Response::AttachSession(response) = &outcome.response else {
                    return Err(RmuxError::Server(
                        "attach-session upgrade requires an attach-session response".to_owned(),
                    ));
                };
                let session_id = attach.session_id;
                self.attach_control_session_for_queue(
                    control_identity,
                    &response.session_name,
                    Some(session_id),
                )
                .await?;
                self.emit_client_session_changed(
                    control_client_name(requester_pid),
                    response.session_name.clone(),
                    session_id,
                )
                .await;
                if let Some(publication) = attach.resize_publication {
                    publication.publish().await;
                }
                self.emit_client_attached_identity(
                    control_client_name(requester_pid),
                    response.session_name.clone(),
                    session_id,
                )
                .await;
            }
        }

        if matches!(request, Request::NewSession(_) | Request::NewSessionExt(_)) {
            if let Response::NewSession(response) = &outcome.response {
                if !response.detached {
                    self.validate_control_session_for_queue(control_identity)
                        .await?;
                }
            }
        }

        queue_action_from_response(outcome.response)
    }
}
