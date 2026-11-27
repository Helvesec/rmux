//! `suspend-client`: suspend one attached client and drop it if its control channel is gone.

use rmux_proto::request::SuspendClientRequest;
use rmux_proto::{ErrorResponse, Response, SuspendClientResponse};

use crate::handler_support::attached_client_required;
use crate::pane_io::AttachControl;

use super::super::RequestHandler;

impl RequestHandler {
    pub(in crate::handler) async fn handle_suspend_client(
        &self,
        requester_pid: u32,
        request: SuspendClientRequest,
    ) -> Response {
        let attach_pid = match self
            .resolve_target_attach_client_pid(
                requester_pid,
                request.target_client.as_deref(),
                "suspend-client",
            )
            .await
        {
            Ok(attach_pid) => attach_pid,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };

        let mut active_attach = self.active_attach.lock().await;
        let Some(active) = active_attach.by_pid.get_mut(&attach_pid) else {
            return Response::Error(ErrorResponse {
                error: attached_client_required("suspend-client"),
            });
        };
        active.suspended = true;
        let session_name = active.session_name.clone();
        let stale_client = active
            .control_tx
            .send(AttachControl::Suspend)
            .is_err()
            .then(|| active.identity(attach_pid));
        drop(active_attach);
        if let Some(stale_client) = stale_client {
            let removed_stale_clients = self
                .remove_attached_clients_for_session(&session_name, vec![stale_client])
                .await;
            if !removed_stale_clients.is_empty() {
                let _ = self
                    .reconcile_attached_session_size_and_emit(&session_name)
                    .await;
            }
        }

        Response::SuspendClient(SuspendClientResponse {
            target_client: attach_pid.to_string(),
        })
    }
}
